use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use codewhale_command_contract::facets::{
    BackgroundCancel, BackgroundJob, BackgroundRequest, CommandBackgroundContext,
};
use codewhale_command_contract::handler::CommandCapabilities;
use codewhale_command_contract::outcome::{BackgroundAction, BackgroundCommandResult};
use codewhale_localization::{Locale, MessageId, tr};

use crate::commands::CommandResult;
use crate::tui::app::{App, AppAction, DispatchApplyFn};

struct Running {
    id: u64,
    cancel: BackgroundCancel,
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
                running.cancel.store(true, Ordering::SeqCst);
                true
            }
            None => false,
        }
    }

    fn cancel_all(&self) -> Vec<&'static str> {
        let mut cancelled: Vec<&'static str> = self
            .lock()
            .running
            .drain()
            .map(|(command, running)| {
                running.cancel.store(true, Ordering::SeqCst);
                command
            })
            .collect();
        cancelled.sort_unstable();
        cancelled
    }

    fn running(&self) -> Vec<&'static str> {
        let mut running: Vec<&'static str> = self.lock().running.keys().copied().collect();
        running.sort_unstable();
        running
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

pub(crate) fn running() -> Vec<&'static str> {
    shared().running()
}

pub(crate) fn cancel_all() -> Vec<&'static str> {
    shared().cancel_all()
}

pub(crate) fn line(locale: Locale, id: MessageId, command: &str) -> String {
    tr(locale, id).replace("{command}", command)
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
        line(self.locale, id, command)
    }
}

impl CommandBackgroundContext for BackgroundHost {
    fn start(
        &mut self,
        command: &'static str,
        job: BackgroundJob,
        request: BackgroundRequest,
    ) -> Result<String, String> {
        let mut state = self.commands.lock();
        if state.running.contains_key(command) {
            return Err(self.line(MessageId::CmdBackgroundAlreadyRunning, command));
        }
        state.next_id += 1;
        let id = state.next_id;
        let cancel = BackgroundCancel::default();
        state.running.insert(
            command,
            Running {
                id,
                cancel: cancel.clone(),
            },
        );
        drop(state);
        let job_cancel = cancel.clone();
        let task = self
            .runtime
            .spawn_blocking(move || job(request, job_cancel));
        self.runtime.spawn(supervise(
            Supervised {
                commands: self.commands.clone(),
                command,
                id,
                cancel,
            },
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

struct Supervised {
    commands: BackgroundCommands,
    command: &'static str,
    id: u64,
    cancel: BackgroundCancel,
}

async fn supervise(
    supervised: Supervised,
    mut task: tokio::task::JoinHandle<BackgroundCommandResult>,
    completion: tokio::sync::mpsc::Sender<DispatchApplyFn>,
    delivery: Delivery,
) {
    let Supervised {
        commands,
        command,
        id,
        cancel,
    } = supervised;
    let Delivery {
        workspace,
        failed,
        stale,
    } = delivery;
    let joined = tokio::select! {
        joined = &mut task => Some(joined),
        () = completion.closed() => None,
    };
    let current = commands.finish(command, id) && !cancel.load(Ordering::SeqCst);
    let result = match joined {
        None => {
            cancel.store(true, Ordering::SeqCst);
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
    use std::time::{Duration, Instant};

    use codewhale_engine::core::engine::mock_engine_handle;
    use codewhale_runtime::config::Config;

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
        let app = App::new(
            crate::tui::test_support::test_tui_options(workspace),
            &config,
        );
        (app, config)
    }

    fn held(_request: BackgroundRequest, cancel: BackgroundCancel) -> BackgroundCommandResult {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !cancel.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        BackgroundCommandResult::message("held job returned")
    }

    fn init_done(
        _request: BackgroundRequest,
        _cancel: BackgroundCancel,
    ) -> BackgroundCommandResult {
        BackgroundCommandResult::with_message_and_action(
            "Creating AGENTS.md",
            BackgroundAction::SendMessage("prompt".to_string()),
        )
    }

    fn update_failed(
        _request: BackgroundRequest,
        _cancel: BackgroundCancel,
    ) -> BackgroundCommandResult {
        BackgroundCommandResult::error("updater failed")
    }

    fn running_flag(commands: &BackgroundCommands, command: &str) -> BackgroundCancel {
        commands
            .lock()
            .running
            .get(command)
            .expect("running command")
            .cancel
            .clone()
    }

    async fn stopped(flag: &BackgroundCancel) -> bool {
        for _ in 0..400 {
            if flag.load(Ordering::SeqCst) && Arc::strong_count(flag) == 1 {
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

        let started = host.start("init", init_done, BackgroundRequest::default());
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
        assert!(
            host.commands.take_ready().is_empty(),
            "a result is applied once"
        );

        assert!(
            host.start("init", held, BackgroundRequest::default())
                .is_ok(),
            "a finished command can be started again"
        );
        let flag = running_flag(&host.commands, "init");
        host.commands.cancel_all();
        assert!(stopped(&flag).await);
    }

    #[tokio::test]
    async fn a_running_command_shows_a_notice_and_an_interrupt_cancels_it() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut app, _config) = test_app(tmp.path());
        let (completion, _mailbox) = tokio::sync::mpsc::channel::<DispatchApplyFn>(2);
        app.dispatch_completion_tx = Some(completion);
        let mut background = host(&app, CommandCapabilities::BACKGROUND).expect("background host");
        assert!(
            background
                .start("update", held, BackgroundRequest::default())
                .is_ok()
        );
        let flag = running_flag(&background.commands, "update");

        let working = super::line(app.ui_locale, MessageId::CmdBackgroundWorking, "update");
        let mut shown = Vec::new();
        crate::tui::ui::sync_background_command_notices(&mut app, &mut shown);
        assert_eq!(shown, ["update"]);
        assert!(app.status_toasts.iter().any(|toast| toast.text == working));

        assert!(crate::tui::ui::cancel_background_commands(&mut app));
        assert_eq!(
            app.status_message,
            Some(super::line(
                app.ui_locale,
                MessageId::CmdBackgroundCancelled,
                "update"
            ))
        );
        assert!(stopped(&flag).await, "the cancelled job returns");
        crate::tui::ui::sync_background_command_notices(&mut app, &mut shown);
        assert!(shown.is_empty());
        assert!(!app.status_toasts.iter().any(|toast| toast.text == working));
        assert!(!crate::tui::ui::cancel_background_commands(&mut app));
    }

    #[tokio::test]
    async fn an_error_result_is_delivered_as_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut host, mut mailbox) = test_host(tmp.path());
        let (mut app, config) = test_app(tmp.path());
        let engine = mock_engine_handle();

        host.start("update", update_failed, BackgroundRequest::default())
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

        assert!(
            host.start("update", held, BackgroundRequest::default())
                .is_ok()
        );
        let first = running_flag(&host.commands, "update");
        assert_eq!(
            host.start("update", init_done, BackgroundRequest::default()),
            Err(line(MessageId::CmdBackgroundAlreadyRunning, "update"))
        );
        assert!(
            Arc::ptr_eq(&first, &running_flag(&host.commands, "update")),
            "the first run is untouched"
        );
        assert!(!first.load(Ordering::SeqCst));

        assert!(
            host.start("init", held, BackgroundRequest::default())
                .is_ok(),
            "a different command is independent"
        );
        let other = running_flag(&host.commands, "init");
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(mailbox.try_recv().is_err(), "nothing has finished");
        host.commands.cancel_all();
        assert!(stopped(&first).await);
        assert!(stopped(&other).await);
    }

    #[tokio::test]
    async fn cancelling_stops_the_job_and_delivers_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut host, mut mailbox) = test_host(tmp.path());

        assert_eq!(
            host.cancel("update"),
            Err(line(MessageId::CmdBackgroundNotRunning, "update"))
        );
        assert!(
            host.start("update", held, BackgroundRequest::default())
                .is_ok()
        );
        let flag = running_flag(&host.commands, "update");
        assert_eq!(
            host.cancel("update"),
            Ok(line(MessageId::CmdBackgroundCancelled, "update"))
        );
        assert!(stopped(&flag).await, "the cancelled job returns");
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            mailbox.try_recv().is_err(),
            "a cancelled run reports nothing"
        );
        assert!(host.commands.take_ready().is_empty());
        assert_eq!(
            host.cancel("update"),
            Err(line(MessageId::CmdBackgroundNotRunning, "update"))
        );

        assert!(
            host.start("update", held, BackgroundRequest::default())
                .is_ok(),
            "a cancelled command can be started again"
        );
        let again = running_flag(&host.commands, "update");
        host.commands.cancel_all();
        assert!(stopped(&again).await);
    }

    #[tokio::test]
    async fn quitting_closes_the_mailbox_and_cancels_running_work() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut host, mailbox) = test_host(tmp.path());

        assert!(
            host.start("update", held, BackgroundRequest::default())
                .is_ok()
        );
        let flag = running_flag(&host.commands, "update");
        drop(mailbox);
        assert!(
            stopped(&flag).await,
            "the job stops when the event loop is gone"
        );
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

        host.start("init", init_done, BackgroundRequest::default())
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
