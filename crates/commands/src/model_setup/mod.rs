mod hf;
mod model;
mod modeldb;
mod models;
mod profile;
mod provider;
mod setup;

pub use hf::HfCmd;
pub use model::ModelCmd;
pub use modeldb::ModelDbCmd;
pub use models::ModelsCmd;
pub use profile::ProfileCmd;
pub use provider::ProviderCmd;
pub use setup::SetupCmd;

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_command_contract::handler::{CommandContexts, CommandHandler};
    use codewhale_command_contract::metadata::RegisterCommand;
    use codewhale_command_contract::model_setup::{
        ModelAction, ProviderAction, SetupAction, SetupStep,
    };
    use codewhale_command_contract::outcome::CommandResult;

    struct ModelPicker;
    impl TryFrom<ModelAction<'_>> for ModelPicker {
        type Error = String;
        fn try_from(action: ModelAction<'_>) -> Result<Self, String> {
            matches!(action, ModelAction::OpenPicker)
                .then_some(Self)
                .ok_or_else(|| "expected model picker".into())
        }
    }

    struct ProviderPicker;
    impl TryFrom<ProviderAction<'_>> for ProviderPicker {
        type Error = String;
        fn try_from(action: ProviderAction<'_>) -> Result<Self, String> {
            matches!(action, ProviderAction::OpenPicker)
                .then_some(Self)
                .ok_or_else(|| "expected provider picker".into())
        }
    }

    struct RuntimeSetup;
    impl TryFrom<SetupAction<'_>> for RuntimeSetup {
        type Error = String;
        fn try_from(action: SetupAction<'_>) -> Result<Self, String> {
            matches!(
                action,
                SetupAction::OpenWizard(Some(SetupStep::RemoteRuntime))
            )
            .then_some(Self)
            .ok_or_else(|| "expected remote setup".into())
        }
    }

    #[test]
    fn commands_accept_hosts_with_only_their_own_output_contract() {
        fn run<A, C: RegisterCommand<CommandResult<A>>>(arg: Option<&str>) -> CommandResult<A> {
            match C::handler() {
                CommandHandler::Pure(handler) => handler(arg),
                CommandHandler::Contextual { handler, .. } => {
                    handler(CommandContexts::empty(), arg)
                }
            }
        }
        assert!(run::<ModelPicker, ModelCmd>(None).action.is_some());
        assert!(run::<ProviderPicker, ProviderCmd>(None).action.is_some());
        assert!(
            run::<RuntimeSetup, SetupCmd>(Some("phone"))
                .action
                .is_some()
        );
        assert!(run::<ModelPicker, ModelCmd>(Some("auto")).is_error);
        assert!(run::<ProviderPicker, ProviderCmd>(Some("fallback")).is_error);
    }
}
