//! Full viewport ownership uses the existing modal stack. Hidden composer,
//! history, selection and active Engine state are untouched. Escape returns
//! to the pet backdrop and the real composer when pet mode is enabled.
use super::Control;
use crate::tui::{
    shell_key_routing::{self, Focus, ShellBindingId as Id},
    views::{CommandPaletteAction, ModalKind, ModalView, ViewAction, ViewEvent},
};
use crossterm::event::{KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect},
};
use std::sync::{Arc, Mutex};
pub struct Habitat {
    controls: Arc<Mutex<Vec<Control>>>,
    selection: Arc<Mutex<Option<String>>>,
    copy_area: Arc<Mutex<Option<Rect>>>,
}
impl Habitat {
    pub fn new(
        controls: Arc<Mutex<Vec<Control>>>,
        selection: Arc<Mutex<Option<String>>>,
        copy_area: Arc<Mutex<Option<Rect>>>,
    ) -> Self {
        Self {
            controls,
            selection,
            copy_area,
        }
    }
}
fn copy_reply() -> ViewAction {
    // Keep completion authority, Markdown, recovery and clipboard acknowledgements
    // with /copy. This deliberately copies the last finished reply during a stream.
    ViewAction::Emit(ViewEvent::CommandPaletteSelected {
        action: CommandPaletteAction::ExecuteCommand {
            command: "/copy".into(),
        },
    })
}
impl ModalView for Habitat {
    fn kind(&self) -> ModalKind {
        ModalKind::PetHabitat
    }
    fn handle_key(&mut self, key: KeyEvent) -> ViewAction {
        let action = match shell_key_routing::route(Focus::Modal(self.kind()), &key) {
            Some(Id::PetResultUp) => Some(Control::Scroll(-1)),
            Some(Id::PetResultDown) => Some(Control::Scroll(1)),
            Some(Id::PetResultPageUp) => Some(Control::Scroll(-10)),
            Some(Id::PetResultPageDown) => Some(Control::Scroll(10)),
            Some(Id::PetBack) => return ViewAction::Close,
            Some(Id::PetCopyReply) => return copy_reply(),
            Some(Id::PetFocusAgents) => Some(Control::FocusAgents),
            Some(Id::PetResultStart) => Some(Control::ScrollEnd(false)),
            Some(Id::PetResultEnd) => Some(Control::ScrollEnd(true)),
            Some(Id::PetOpenAgent) => {
                if let Some(agent_id) = self.selection.lock().ok().and_then(|s| s.clone()) {
                    return ViewAction::EmitAndClose(ViewEvent::OpenAgentTranscript { agent_id });
                }
                None
            }
            Some(Id::PetSound) => Some(Control::Sound),
            Some(Id::PetBrowser) => Some(Control::Browser),
            _ => None,
        };
        if let Some(action) = action
            && let Ok(mut queue) = self.controls.lock()
            && queue.len() < 16
        {
            queue.push(action);
        }
        ViewAction::None
    }
    fn handle_mouse(&mut self, mouse: MouseEvent) -> ViewAction {
        if mouse.kind == MouseEventKind::Down(MouseButton::Left)
            && self
                .copy_area
                .lock()
                .ok()
                .and_then(|area| *area)
                .is_some_and(|area| area.contains(Position::new(mouse.column, mouse.row)))
        {
            return copy_reply();
        }
        if let Ok(mut queue) = self.controls.lock()
            && queue.len() < 16
        {
            queue.push(Control::Mouse(mouse));
        }
        ViewAction::None
    }
    fn render(&self, _area: Rect, _buf: &mut Buffer) {}
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
pub fn companion_hints(locale: codewhale_localization::Locale) -> String {
    use codewhale_localization::{MessageId, tr};
    let mut text = tr(locale, MessageId::PetModeCompanionHints).into_owned();
    for (name, id) in [("sound", Id::PetSound), ("browser", Id::PetBrowser)] {
        text = text.replace(
            &format!("{{{name}}}"),
            &shell_key_routing::display_chord(shell_key_routing::binding(id).footer_chord),
        );
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};
    #[test]
    fn enter_opens_only_the_painted_worker_through_the_existing_event_and_closes_pet_focus() {
        let controls = Arc::new(Mutex::new(Vec::new()));
        let selection = Arc::new(Mutex::new(None));
        let mut view = Habitat::new(controls, selection.clone(), Arc::new(Mutex::new(None)));
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(view.handle_key(enter), ViewAction::None));
        *selection.lock().unwrap() = Some("worker-id".into());
        assert!(
            matches!(view.handle_key(enter), ViewAction::EmitAndClose(ViewEvent::OpenAgentTranscript { agent_id }) if agent_id == "worker-id")
        );
    }
    #[test]
    fn pet_habitat_full_view_preserves_composer_history_and_session_on_escape() {
        let mut app = crate::tui::test_support::test_app_with_options(
            crate::tui::test_support::test_tui_options("."),
        );
        app.input = "unfinished composer".into();
        app.add_message(crate::tui::history::HistoryCell::System {
            content: "retained transcript".into(),
        });
        let history = app.history.len();
        let session = app.current_session_id.clone();
        // Push the actual focus owner without starting a network client.
        let controls = Arc::new(Mutex::new(Vec::new()));
        app.view_stack.push(Habitat::new(
            controls.clone(),
            Arc::new(Mutex::new(None)),
            Arc::new(Mutex::new(None)),
        ));
        assert_eq!(app.focus(), Focus::Modal(ModalKind::PetHabitat));
        assert_eq!(
            shell_key_routing::route(
                Focus::Composer,
                &KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE)
            ),
            Some(Id::PetInspect)
        );
        app.view_stack
            .handle_key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE));
        assert!(matches!(
            controls.lock().unwrap().as_slice(),
            [Control::Sound]
        ));
        app.view_stack
            .handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        app.view_stack
            .handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.view_stack.is_empty());
        assert_eq!(app.input, "unfinished composer");
        assert_eq!(app.history.len(), history);
        assert_eq!(app.current_session_id, session);
    }
    #[test]
    fn pet_copy_keyboard_and_mouse_share_the_finished_reply_receipt_without_closing_or_editing() {
        let _env = crate::test_support::lock_test_env();
        let tmp = tempfile::TempDir::new().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("HOME", tmp.path());
        let _state = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", tmp.path());
        let mut app = crate::tui::test_support::test_app_with_options(
            crate::tui::test_support::test_tui_options("."),
        );
        app.clipboard = crate::tui::clipboard::ClipboardHandler::for_test(false, false);
        app.input = "unfinished draft".into();
        let answer = "Finished **Markdown**\n```rust\nlet x = 1;\n```";
        let index = app.history.len();
        app.add_message(crate::tui::history::HistoryCell::Assistant {
            content: answer.into(),
            streaming: false,
        });
        app.record_completed_assistant_output(index, answer);
        app.add_message(crate::tui::history::HistoryCell::Assistant {
            content: "partial new answer".into(),
            streaming: true,
        });
        let copy_area = Arc::new(Mutex::new(Some(Rect::new(2, 8, 24, 1))));
        let mut view = Habitat::new(
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(Mutex::new(None)),
            copy_area.clone(),
        );
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE);
        assert_eq!(shell_key_routing::route(Focus::Composer, &key), None);
        let actions = [
            view.handle_key(key),
            view.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 3,
                row: 8,
                modifiers: KeyModifiers::NONE,
            }),
        ];
        for action in actions {
            let ViewAction::Emit(ViewEvent::CommandPaletteSelected {
                action: CommandPaletteAction::ExecuteCommand { command },
            }) = action
            else {
                panic!("copy must emit without closing the inspector")
            };
            assert_eq!(command, "/copy");
            let result = crate::commands::execute(&command, &mut app);
            assert!(!result.is_error, "{:?}", result.message);
            let toast = app
                .active_status_toast(crate::tui::underwater::ShellPhase::Done)
                .expect("manual copy feedback must survive the completed phase");
            assert_eq!(toast.text, result.message.unwrap());
            assert_eq!(toast.level, crate::tui::app::StatusToastLevel::Info);
            assert_eq!(toast.ttl_ms, Some(8_000));
            assert_eq!(
                std::fs::read_to_string(tmp.path().join("exports/last-copy.md")).unwrap(),
                answer
            );
            assert_eq!(app.input, "unfinished draft");
        }
        *copy_area.lock().unwrap() = None;
        assert!(
            matches!(
                view.handle_mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: 3,
                    row: 8,
                    modifiers: KeyModifiers::NONE
                }),
                ViewAction::None
            ),
            "hidden copy targets must not remain active"
        );
    }
}
