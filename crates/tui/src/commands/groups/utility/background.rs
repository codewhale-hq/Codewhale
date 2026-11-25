use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use codewhale_command_contract::facets::{BackgroundCommandWork, CommandBackgroundContext};
use codewhale_command_contract::handler::CommandCapabilities;
use codewhale_command_contract::outcome::{BackgroundAction, BackgroundCommandResult};
use codewhale_localization::{Locale, MessageId, tr};

use crate::commands::CommandResult;
use crate::tui::app::{App, AppAction, DispatchApplyFn};

struct Running {
    id: u64,
    abort: tokio::task::AbortHandle,
}

#[derive(Default)]
struct State {
    next_id: u64,
    running: HashMap<&'static str, Running>,
    ready: Vec<BackgroundCommandResult>,
}

#[derive(Clone, Default)]
struct BackgroundCommands {
    state: Arc<Mutex<State>>,
}

impl BackgroundCommands {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn finish(&self, command: &'static str, id: u64) -> bool {
        let mut state = self.lock();
        if state
            .running
            .get(command)
            .is_some_and(|running| running.id == id)
        {
            state.running.remove(command);
            true
        } else {
            false
        }
    }

    fn cancel(&self, command: &'static str) -> bool {
        let running = self.lock().running.remove(command);
        match running {
            Some(running) => {
                running.abort.abort();
                true
            }
            None => false,
        }
    }

    fn push_ready(&self, result: BackgroundCommandResult) {
        self.lock().ready.push(result);
    }

    fn take_ready(&self) -> Vec<CommandResult> {
        let ready = std::mem::take(&mut self.lock().ready);
        ready.into_iter().map(host_result).collect()
    }
}

fn shared() -> &'static BackgroundCommands {
    static SHARED: OnceLock<BackgroundCommands> = OnceLock::new();
    SHARED.get_or_init(BackgroundCommands::default)
}

fn host_result(result: BackgroundCommandResult) -> CommandResult {
    CommandResult {
        message: result.message,
        action: result.action.map(|action| match action {
            BackgroundAction::SendMessage(content) => AppAction::SendMessage(content),
        }),
        is_error: result.is_error,
    }
}

pub(crate) fn take_ready() -> Vec<CommandResult> {
    shared().take_ready()
}

pub(in crate::commands) struct BackgroundHost {
    commands: BackgroundCommands,
    completion: tokio::sync::mpsc::Sender<DispatchApplyFn>,
    runtime: tokio::runtime::Handle,
    locale: Locale,
    workspace: PathBuf,
}

pub(in crate::commands) fn host(
    app: &App,
    capabilities: CommandCapabilities,
) -> Option<BackgroundHost> {
    if !capabilities.contains(CommandCapabilities::BACKGROUND) {
        return None;
    }
    Some(BackgroundHost {
        commands: shared().clone(),
        completion: app.dispatch_completion_tx.clone()?,
        runtime: tokio::runtime::Handle::try_current().ok()?,
        locale: app.ui_locale,
        workspace: app.workspace.clone(),
    })
}

impl BackgroundHost {
    fn line(&self, id: MessageId, command: &str) -> String {
        tr(self.locale, id).replace("{command}", command)
    }
}

impl CommandBackgroundContext for BackgroundHost {
    fn start(
        &mut self,
        command: &'static str,
        work: BackgroundCommandWork,
    ) -> Result<String, String> {
        let mut state = self.commands.lock();
        if state.running.contains_key(command) {
            return Err(self.line(MessageId::CmdBackgroundAlreadyRunning, command));
        }
        state.next_id += 1;
        let id = state.next_id;
        let task = self.runtime.spawn(work);
        state.running.insert(
            command,
            Running {
                id,
                abort: task.abort_handle(),
            },
        );
        drop(state);
        self.runtime.spawn(supervise(
            self.commands.clone(),
            command,
            id,
            task,
            self.completion.clone(),
            Delivery {
                workspace: self.workspace.clone(),
                failed: self.line(MessageId::CmdBackgroundFailed, command),
                stale: self.line(MessageId::CmdBackgroundStale, command),
            },
        ));
        Ok(self.line(MessageId::CmdBackgroundStarted, command))
    }

    fn cancel(&mut self, command: &'static str) -> Result<String, String> {
        if self.commands.cancel(command) {
            Ok(self.line(MessageId::CmdBackgroundCancelled, command))
        } else {
            Err(self.line(MessageId::CmdBackgroundNotRunning, command))
        }
    }
}

struct Delivery {
    workspace: PathBuf,
    failed: String,
    stale: String,
}

async fn supervise(
    commands: BackgroundCommands,
    command: &'static str,
    id: u64,
    mut task: tokio::task::JoinHandle<BackgroundCommandResult>,
    completion: tokio::sync::mpsc::Sender<DispatchApplyFn>,
    delivery: Delivery,
) {
    let Delivery {
        workspace,
        failed,
        stale,
    } = delivery;
    let joined = tokio::select! {
        joined = &mut task => Some(joined),
        () = completion.closed() => None,
    };
    let current = commands.finish(command, id);
    let result = match joined {
        None => {
            task.abort();
            return;
        }
        Some(Ok(result)) => result,
        Some(Err(error)) if error.is_cancelled() => return,
        Some(Err(_)) => BackgroundCommandResult::error(failed),
    };
    if !current {
        return;
    }
    let apply: DispatchApplyFn = Box::new(move |app: &mut App, _, _| {
        let result = if result.action.is_some() && app.workspace != workspace {
            BackgroundCommandResult::message(stale)
        } else {
            result
        };
        commands.push_ready(result);
        app.needs_redraw = true;
        Ok(())
    });
    let _ = completion.send(apply).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use codewhale_engine::core::engine::mock_engine_handle;
    use codewhale_runtime::config::Config;

    struct DropFlag(Arc<AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    type Mailbox = tokio::sync::mpsc::Receiver<DispatchApplyFn>;

    fn test_host(workspace: &std::path::Path) -> (BackgroundHost, Mailbox) {
        let (completion, mailbox) = tokio::sync::mpsc::channel::<DispatchApplyFn>(2);
        let host = BackgroundHost {
            commands: BackgroundCommands::default(),
            completion,
            runtime: tokio::runtime::Handle::current(),
            locale: Locale::En,
            workspace: workspace.to_path_buf(),
        };
        (host, mailbox)
    }

    fn test_app(workspace: &std::path::Path) -> (App, Config) {
        let config = Config::default();
        let app = App::new(crate::tui::test_support::test_tui_options(workspace), &config);
        (app, config)
    }

    fn held(flag: &Arc<AtomicBool>) -> BackgroundCommandWork {
        let flag = DropFlag(flag.clone());
        Box::pin(async move {
            let _held = flag;
            std::future::pending::<BackgroundCommandResult>().await
        })
    }

    async fn settled(flag: &Arc<AtomicBool>) -> bool {
        for _ in 0..200 {
            if flag.load(Ordering::SeqCst) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        false
    }

    async fn delivered(mailbox: &mut Mailbox) -> DispatchApplyFn {
        tokio::time::timeout(Duration::from_secs(5), mailbox.recv())
            .await
            .expect("background completion timeout")
            .expect("background completion")
    }

    fn line(id: MessageId, command: &str) -> String {
        tr(Locale::En, id).replace("{command}", command)
    }

    #[tokio::test]
    async fn a_finished_command_delivers_its_result_and_action_through_the_mailbox() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut host, mut mailbox) = test_host(tmp.path());
        let (mut app, config) = test_app(tmp.path());
        app.workspace = tmp.path().to_path_buf();
        let engine = mock_engine_handle();

        let started = host.start(
            "init",
            Box::pin(async {
                BackgroundCommandResult::with_message_and_action(
                    "Creating AGENTS.md",
                    BackgroundAction::SendMessage("prompt".to_string()),
                )
            }),
        );
        assert_eq!(started, Ok(line(MessageId::CmdBackgroundStarted, "init")));
        assert!(
            host.commands.take_ready().is_empty(),
            "starting returns before any result exists"
        );

        let apply = delivered(&mut mailbox).await;
        app.needs_redraw = false;
        apply(&mut app, &engine.handle, &config).expect("apply completion");
        assert!(app.needs_redraw);

        let ready = host.commands.take_ready();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].message.as_deref(), Some("Creating AGENTS.md"));
        assert!(
            matches!(&ready[0].action, Some(AppAction::SendMessage(prompt)) if prompt == "prompt")
        );
        assert!(!ready[0].is_error);
        assert!(host.commands.take_ready().is_empty(), "a result is applied once");

        let flag = Arc::new(AtomicBool::new(false));
        assert!(
            host.start("init", held(&flag)).is_ok(),
            "a finished command can be started again"
        );
    }

    #[tokio::test]
    async fn an_error_result_is_delivered_as_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut host, mut mailbox) = test_host(tmp.path());
        let (mut app, config) = test_app(tmp.path());
        let engine = mock_engine_handle();

        host.start(
            "update",
            Box::pin(async { BackgroundCommandResult::error("updater failed") }),
        )
        .expect("start");
        let apply = delivered(&mut mailbox).await;
        apply(&mut app, &engine.handle, &config).expect("apply completion");

        let ready = host.commands.take_ready();
        assert_eq!(ready.len(), 1);
        assert!(ready[0].is_error);
        assert_eq!(ready[0].message.as_deref(), Some("Error: updater failed"));
        assert!(ready[0].action.is_none());
    }

    #[tokio::test]
    async fn a_second_run_of_the_same_command_is_refused_while_the_first_is_running() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut host, mut mailbox) = test_host(tmp.path());
        let first = Arc::new(AtomicBool::new(false));
        let second = Arc::new(AtomicBool::new(false));

        assert!(host.start("update", held(&first)).is_ok());
        assert_eq!(
            host.start("update", held(&second)),
            Err(line(MessageId::CmdBackgroundAlreadyRunning, "update"))
        );
        assert!(
            second.load(Ordering::SeqCst),
            "refused work is dropped without running"
        );
        assert!(!first.load(Ordering::SeqCst), "the first run is untouched");

        let other = Arc::new(AtomicBool::new(false));
        assert!(
            host.start("init", held(&other)).is_ok(),
            "a different command is independent"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(mailbox.try_recv().is_err(), "nothing has finished");
    }

    #[tokio::test]
    async fn cancelling_drops_the_work_and_delivers_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut host, mut mailbox) = test_host(tmp.path());
        let flag = Arc::new(AtomicBool::new(false));

        assert_eq!(
            host.cancel("update"),
            Err(line(MessageId::CmdBackgroundNotRunning, "update"))
        );
        assert!(host.start("update", held(&flag)).is_ok());
        assert_eq!(
            host.cancel("update"),
            Ok(line(MessageId::CmdBackgroundCancelled, "update"))
        );
        assert!(settled(&flag).await, "cancelled work is dropped");
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(mailbox.try_recv().is_err(), "a cancelled run reports nothing");
        assert!(host.commands.take_ready().is_empty());
        assert_eq!(
            host.cancel("update"),
            Err(line(MessageId::CmdBackgroundNotRunning, "update"))
        );

        let again = Arc::new(AtomicBool::new(false));
        assert!(
            host.start("update", held(&again)).is_ok(),
            "a cancelled command can be started again"
        );
    }

    #[tokio::test]
    async fn quitting_closes_the_mailbox_and_cancels_running_work() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut host, mailbox) = test_host(tmp.path());
        let flag = Arc::new(AtomicBool::new(false));

        assert!(host.start("update", held(&flag)).is_ok());
        drop(mailbox);
        assert!(settled(&flag).await, "work stops when the event loop is gone");
        assert!(host.commands.lock().running.is_empty());
        assert!(host.commands.take_ready().is_empty());
    }

    #[tokio::test]
    async fn a_follow_up_action_is_dropped_when_the_workspace_changed() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut host, mut mailbox) = test_host(tmp.path());
        let (mut app, config) = test_app(tmp.path());
        app.workspace = tmp.path().join("elsewhere");
        let engine = mock_engine_handle();

        host.start(
            "init",
            Box::pin(async {
                BackgroundCommandResult::with_message_and_action(
                    "Creating AGENTS.md",
                    BackgroundAction::SendMessage("prompt".to_string()),
                )
            }),
        )
        .expect("start");
        let apply = delivered(&mut mailbox).await;
        apply(&mut app, &engine.handle, &config).expect("apply completion");

        let ready = host.commands.take_ready();
        assert_eq!(ready.len(), 1);
        assert_eq!(
            ready[0].message,
            Some(line(MessageId::CmdBackgroundStale, "init"))
        );
        assert!(ready[0].action.is_none());
    }

    #[test]
    fn the_facet_is_offered_only_to_commands_that_declare_it_with_a_live_mailbox() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut app, _config) = test_app(tmp.path());
        assert!(host(&app, CommandCapabilities::BACKGROUND).is_none());

        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        assert!(host(&app, CommandCapabilities::BACKGROUND).is_none());
        let (completion, _mailbox) = tokio::sync::mpsc::channel::<DispatchApplyFn>(2);
        app.dispatch_completion_tx = Some(completion);
        assert!(host(&app, CommandCapabilities::WORKSPACE).is_none());
        assert!(
            host(
                &app,
                CommandCapabilities::BACKGROUND | CommandCapabilities::WORKSPACE
            )
            .is_some()
        );
    }
}
