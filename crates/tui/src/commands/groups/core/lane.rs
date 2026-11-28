#[cfg(test)]
mod tests {
    use crate::tui::app::{App, TuiOptions};
    use codewhale_lane::control::operations_for_domain;
    use codewhale_lane::{ControlDomain, ControlOperation, ControlSurface};
    use codewhale_runtime::config::Config;
    use std::path::PathBuf;

    fn test_app() -> App {
        let options = TuiOptions {
            ..crate::tui::test_support::test_tui_options(PathBuf::from("."))
        };
        App::new(options, &Config::default())
    }

    #[test]
    fn bare_lane_and_list_resolve_to_the_same_read_verb() {
        let mut app = test_app();
        let bare = crate::commands::execute("/lane", &mut app);
        let whitespace = crate::commands::execute("/lane   ", &mut app);
        let explicit = crate::commands::execute("/lane list", &mut app);
        assert_eq!(bare, explicit);
        assert_eq!(whitespace, explicit);
    }

    #[test]
    fn every_slash_verb_maps_onto_a_shared_lane_operation() {
        for descriptor in operations_for_domain(ControlDomain::Lane) {
            assert_eq!(
                ControlOperation::parse_verb(ControlDomain::Lane, descriptor.verb),
                Some(descriptor.operation),
                "/lane {} must resolve to {}",
                descriptor.verb,
                descriptor.id
            );
        }
        for (spelling, expected) in [
            ("stop", ControlOperation::LaneInterrupt),
            ("cancel", ControlOperation::LaneInterrupt),
            ("inspect", ControlOperation::LaneStatus),
            ("ls", ControlOperation::LaneList),
        ] {
            assert_eq!(
                ControlOperation::parse_verb(ControlDomain::Lane, spelling),
                Some(expected)
            );
        }
    }

    #[test]
    fn unknown_verbs_are_rejected_without_touching_the_registry() {
        let mut app = test_app();
        let result = crate::commands::execute("/lane obliterate lane-a1b2c3d4", &mut app);
        assert!(result.is_error);
        assert!(
            result
                .message
                .as_deref()
                .is_some_and(|message| message.contains("Unknown /lane verb 'obliterate'"))
        );
    }

    #[test]
    fn help_lists_every_verb_with_its_authority_and_cli_twin() {
        let mut app = test_app();
        let help = crate::commands::execute("/lane help", &mut app)
            .message
            .unwrap_or_default();
        for descriptor in operations_for_domain(ControlDomain::Lane) {
            assert!(
                help.contains(descriptor.cli_invocation),
                "help must name the CLI twin of {}",
                descriptor.id
            );
            assert!(
                help.contains(&descriptor.slash_invocation()),
                "help must name the slash form of {}",
                descriptor.id
            );
        }
        assert!(help.contains("read"));
        assert!(help.contains("write"));
        assert!(
            help.contains("$CODEWHALE_HOME/lanes/"),
            "help must say which durable store it reads"
        );
    }

    #[test]
    fn restart_and_resume_report_that_they_have_no_backend() {
        let mut app = test_app();
        for verb in ["restart", "resume"] {
            let result = crate::commands::execute(&format!("/lane {verb} lane-a1b2c3d4"), &mut app);
            assert!(result.is_error, "/lane {verb}");
            let message = result.message.as_deref().unwrap_or_default();
            assert!(
                message.contains("backend_not_implemented") || message.contains("no_lane_registry"),
                "/lane {verb} must explain itself, got: {message}"
            );
        }
    }

    #[test]
    fn slash_command_and_cli_agree_on_lane_verb_ids() {
        let info = crate::commands::get_command_info("lane").expect("lane info");
        for descriptor in operations_for_domain(ControlDomain::Lane) {
            assert_eq!(descriptor.slash_command, info.name);
            assert!(
                info.usage.contains(descriptor.verb),
                "/lane usage must document {}",
                descriptor.verb
            );
            assert!(descriptor.offers(ControlSurface::Slash));
            assert!(descriptor.offers(ControlSurface::Cli));
        }
    }

    #[test]
    fn bare_lane_is_read_only_so_the_hotbar_cannot_mutate_state() {
        let info = crate::commands::get_command_info("lane").expect("lane info");
        let descriptor = ControlOperation::LaneList.descriptor();
        assert_eq!(
            descriptor.authority,
            codewhale_lane::ControlAuthority::Read,
            "a bare hotbar press must not be a write"
        );
        assert!(
            !info.requires_required_argument(),
            "/lane must be directly runnable from the palette and hotbar"
        );
        assert_eq!(descriptor.hotbar_action_id(), "slash.lane");
    }
}
