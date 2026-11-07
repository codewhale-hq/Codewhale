use std::cell::RefCell;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use codewhale_account_cli::{
    AccountClient, AccountLogin, AccountLoginCancellation, AccountLoginPoll, AccountModels,
};
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_palette as palette;
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Padding, Paragraph, Widget, Wrap},
};

use crate::tui::shell_key_routing::{ShellBindingId, binding};
use crate::tui::views::{
    ActionHint, ModalKind, ModalView, ViewAction, ViewEvent, centered_modal_area,
    render_modal_footer, render_modal_surface,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountOperationOutcome {
    Complete,
    LocalSignOut,
    Cancelled,
    ModelCancelled,
    Expired,
    Failed,
}

enum WorkerResult {
    Started(AccountLogin),
    Polled(AccountLogin, AccountLoginPoll),
    Models(AccountModels),
    ModelsFailed,
    SignedOut(bool),
    SignedIn,
    Cancelled,
    Failed,
}

pub(crate) struct AccountLoginView {
    locale: Locale,
    signing_out: bool,
    connecting_models: bool,
    profile: Option<String>,
    models: Option<AccountModels>,
    models_unavailable: bool,
    models_failed: bool,
    worker: Option<Receiver<WorkerResult>>,
    login: Option<AccountLogin>,
    cancellation: Option<AccountLoginCancellation>,
    url: Option<String>,
    code: Option<String>,
    next_poll: Instant,
    selected: usize,
    buttons: RefCell<Vec<(usize, Rect)>>,
    cancelled: Arc<AtomicBool>,
}

impl AccountLoginView {
    pub(crate) fn new(profile: Option<String>, signing_out: bool, locale: Locale) -> Self {
        Self::start(profile, signing_out, false, locale)
    }

    pub(crate) fn connect(profile: Option<String>, locale: Locale) -> Self {
        Self::start(profile, false, true, locale)
    }

    fn start(
        profile: Option<String>,
        signing_out: bool,
        connecting_models: bool,
        locale: Locale,
    ) -> Self {
        let worker_profile = profile.clone();
        let (sender, receiver) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = cancelled.clone();
        tokio::task::spawn_blocking(move || {
            let result = AccountClient::new(worker_profile.as_deref(), None).and_then(|client| {
                if worker_cancelled.load(Ordering::Acquire) {
                    return Ok(WorkerResult::Cancelled);
                }
                if signing_out {
                    let snapshot = client.snapshot()?;
                    if worker_cancelled.load(Ordering::Acquire) {
                        return Ok(WorkerResult::Cancelled);
                    }
                    client
                        .logout_if_unchanged(&snapshot)
                        .map(WorkerResult::SignedOut)
                } else if matches!(
                    client.status()?.account.state,
                    codewhale_secrets::account::AccountSessionState::Authenticated
                        | codewhale_secrets::account::AccountSessionState::OfflineCached
                ) {
                    if connecting_models {
                        Ok(client
                            .connect_models_with_cancel(&worker_cancelled)
                            .map(WorkerResult::Models)
                            .unwrap_or(WorkerResult::ModelsFailed))
                    } else {
                        Ok(WorkerResult::SignedIn)
                    }
                } else {
                    client
                        .start_login_with_cancel(
                            Duration::from_secs(
                                codewhale_account_cli::DEFAULT_LOGIN_TIMEOUT_SECONDS,
                            ),
                            worker_cancelled.clone(),
                        )
                        .map(WorkerResult::Started)
                }
            });
            let _ = sender.send(result.unwrap_or(WorkerResult::Failed));
        });
        Self {
            locale,
            signing_out,
            connecting_models,
            profile,
            models: None,
            models_unavailable: false,
            models_failed: false,
            worker: Some(receiver),
            login: None,
            cancellation: None,
            url: None,
            code: None,
            next_poll: Instant::now(),
            selected: 0,
            buttons: RefCell::new(Vec::new()),
            cancelled,
        }
    }

    fn finish(&self, outcome: AccountOperationOutcome) -> ViewAction {
        if self.signing_out && outcome == AccountOperationOutcome::Cancelled {
            return ViewAction::Close;
        }
        let outcome = if self.connecting_models && outcome == AccountOperationOutcome::Cancelled {
            AccountOperationOutcome::ModelCancelled
        } else {
            outcome
        };
        ViewAction::EmitAndClose(ViewEvent::AccountOperationCompleted {
            signing_out: self.signing_out,
            outcome,
        })
    }

    fn open_browser(&self) {
        if let Some(url) = self.url.clone() {
            let cancelled = self.cancelled.clone();
            tokio::task::spawn_blocking(move || {
                if !cancelled.load(Ordering::Acquire) {
                    let _ = webbrowser::open(&url);
                }
            });
        }
    }

    fn activate(&mut self, selected: usize) -> ViewAction {
        if self.models_unavailable {
            return match selected {
                0 => {
                    self.load_models();
                    ViewAction::Redraw
                }
                1 => ViewAction::EmitAndClose(ViewEvent::AccountConnectionChoice {
                    account: Some(false),
                }),
                _ => self.finish(AccountOperationOutcome::Cancelled),
            };
        }
        if let Some(models) = &mut self.models {
            if let Some(model) = models.models.get(selected) {
                let Some(key) = models.credential.take() else {
                    return self.finish(AccountOperationOutcome::Failed);
                };
                return ViewAction::EmitAndClose(ViewEvent::AccountModelSelected {
                    access: codewhale_runtime::config::AccountModelAccess {
                        session_id: models.session_id.clone(),
                        credential: codewhale_runtime::credentials::Credential::ApiKey { key },
                        expires_at: models.expires_at,
                        profile: self.profile.clone(),
                    },
                    api_base: models.api_base.clone(),
                    account_id: models.account_id.clone(),
                    expected_revision: models.snapshot.fingerprint(),
                    model: model.id.clone(),
                });
            }
            return self.finish(AccountOperationOutcome::Cancelled);
        }
        match selected {
            0 if !self.signing_out && self.url.is_some() => {
                self.open_browser();
                ViewAction::None
            }
            1 if !self.signing_out => self.url.as_ref().map_or(ViewAction::None, |url| {
                ViewAction::Emit(ViewEvent::CopyToClipboard {
                    text: url.clone(),
                    label: tr(self.locale, MessageId::AccountLoginLink).into_owned(),
                })
            }),
            _ => self.finish(AccountOperationOutcome::Cancelled),
        }
    }

    fn load_models(&mut self) {
        self.url = None;
        self.code = None;
        self.models = None;
        self.models_unavailable = false;
        self.models_failed = false;
        self.selected = 0;
        let profile = self.profile.clone();
        let cancelled = self.cancelled.clone();
        let (sender, receiver) = mpsc::channel();
        self.worker = Some(receiver);
        tokio::task::spawn_blocking(move || {
            let result = AccountClient::new(profile.as_deref(), None).and_then(|client| {
                if !matches!(
                    client.status()?.account.state,
                    codewhale_secrets::account::AccountSessionState::Authenticated
                        | codewhale_secrets::account::AccountSessionState::OfflineCached
                ) {
                    return client
                        .start_login_with_cancel(
                            Duration::from_secs(
                                codewhale_account_cli::DEFAULT_LOGIN_TIMEOUT_SECONDS,
                            ),
                            cancelled.clone(),
                        )
                        .map(WorkerResult::Started);
                }
                Ok(client
                    .connect_models_with_cancel(&cancelled)
                    .map(WorkerResult::Models)
                    .unwrap_or(WorkerResult::ModelsFailed))
            });
            let _ = sender.send(result.unwrap_or(WorkerResult::ModelsFailed));
        });
    }
}

impl Drop for AccountLoginView {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(cancellation) = &self.cancellation {
            cancellation.cancel();
        }
    }
}

impl ModalView for AccountLoginView {
    fn kind(&self) -> ModalKind {
        ModalKind::AccountLogin
    }

    fn set_locale(&mut self, locale: Locale) {
        self.locale = locale;
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn handle_key(&mut self, key: KeyEvent) -> ViewAction {
        if key.kind != KeyEventKind::Press {
            return ViewAction::None;
        }
        if binding(ShellBindingId::AccountLoginCancel).matches(&key)
            || key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL
        {
            return self.finish(AccountOperationOutcome::Cancelled);
        }
        if self.url.is_some() && binding(ShellBindingId::AccountLoginOpenBrowser).matches(&key) {
            return self.activate(0);
        }
        if self.url.is_some() && binding(ShellBindingId::AccountLoginCopyLink).matches(&key) {
            return self.activate(1);
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
        {
            return ViewAction::None;
        }
        let count = if let Some(models) = &self.models {
            models.models.len() + 1
        } else if self.url.is_some() || self.models_unavailable {
            3
        } else {
            1
        };
        match key.code {
            KeyCode::Tab | KeyCode::Down | KeyCode::Right => {
                self.selected = (self.selected + 1) % count;
                ViewAction::Redraw
            }
            KeyCode::BackTab | KeyCode::Up | KeyCode::Left => {
                self.selected = (self.selected + count - 1) % count;
                ViewAction::Redraw
            }
            KeyCode::Enter => self.activate(if count == 1 && self.models.is_none() {
                2
            } else {
                self.selected
            }),
            _ => ViewAction::None,
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> ViewAction {
        if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            return ViewAction::None;
        }
        let selected = self.buttons.borrow().iter().find_map(|(index, rect)| {
            crate::tui::mouse_ui::mouse_hits_rect(mouse, Some(*rect)).then_some(*index)
        });
        selected.map_or(ViewAction::None, |selected| self.activate(selected))
    }

    fn occupied_region(&self, area: Rect) -> Rect {
        centered_modal_area(area, 76, 18, 28, 8)
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        let popup = self.occupied_region(area);
        render_modal_surface(area, popup, buf);
        let title = if self.signing_out {
            MessageId::AccountLogoutTitle
        } else if self.connecting_models && self.url.is_none() {
            MessageId::AccountModelsTitle
        } else {
            MessageId::AccountLoginTitle
        };
        let block = Block::default()
            .title(tr(self.locale, title).into_owned())
            .borders(Borders::ALL)
            .border_style(Style::default().fg(palette::BORDER_COLOR))
            .style(Style::default().bg(palette::WHALE_BG))
            .padding(Padding::uniform(1));
        let inner = block.inner(popup);
        block.render(popup, buf);
        let dismiss_id = if self.signing_out {
            MessageId::CtxInspActionClose
        } else {
            MessageId::PickerActionCancel
        };
        let mut hints = Vec::new();
        if self.models.is_some() || self.models_unavailable {
            hints.push(ActionHint::new(
                binding(ShellBindingId::AccountLoginNext).footer_chord,
                tr(self.locale, MessageId::PickerActionMove),
            ));
            hints.push(ActionHint::new(
                binding(ShellBindingId::AccountLoginSelect).footer_chord,
                tr(self.locale, MessageId::PickerActionApply),
            ));
        }
        hints.push(ActionHint::new(
            binding(ShellBindingId::AccountLoginCancel).footer_chord,
            tr(self.locale, dismiss_id),
        ));
        let content = render_modal_footer(inner, buf, &hints);
        let mut lines = Vec::new();
        if let Some(code) = &self.code {
            lines.push(Line::from(Span::styled(
                tr(self.locale, MessageId::AccountLoginCode).replace("{code}", code),
                Style::default()
                    .fg(palette::WHALE_ACTION)
                    .add_modifier(Modifier::BOLD),
            )));
        }
        if let Some(url) = &self.url {
            lines.push(Line::from(url.clone()));
        }
        lines.push(Line::from(
            tr(
                self.locale,
                if self.signing_out {
                    MessageId::AccountLogoutStarting
                } else if self.url.is_some() {
                    MessageId::AccountLoginWaiting
                } else if self.models_unavailable && self.models_failed {
                    MessageId::AccountModelsLoadFailed
                } else if self.models_unavailable {
                    MessageId::AccountModelsUnavailable
                } else if self.connecting_models {
                    MessageId::AccountModelsLoading
                } else {
                    MessageId::AccountLoginStarting
                },
            )
            .into_owned(),
        ));
        if let Some(models) = &self.models {
            let billing = models
                .models
                .get(self.selected)
                .map(|model| {
                    tr(self.locale, MessageId::ProviderExternalBillingLine)
                        .replace("{owner}", &model.provider)
                })
                .unwrap_or_default();
            let paragraph = Paragraph::new(billing).wrap(Wrap { trim: false });
            let billing_height = (paragraph.line_count(content.width.max(1)) as u16)
                .min(content.height.saturating_sub(2));
            paragraph.render(
                Rect {
                    height: billing_height,
                    ..content
                },
                buf,
            );
            let content = Rect {
                y: content.y + billing_height,
                height: content.height.saturating_sub(billing_height),
                ..content
            };
            let mut buttons = self.buttons.borrow_mut();
            buttons.clear();
            let count = models.models.len() + 1;
            let visible = usize::from(content.height);
            let start = self.selected.saturating_sub(visible.saturating_sub(1));
            for (row, index) in (start..count).take(visible).enumerate() {
                let label = models
                    .models
                    .get(index)
                    .map(|model| {
                        let mut clean = String::new();
                        codewhale_tui_kit::osc8::strip_ansi_into(
                            &format!("{} · {}", model.label, model.provider),
                            &mut clean,
                        );
                        clean
                            .chars()
                            .filter(|character| !character.is_control())
                            .take(160)
                            .collect::<String>()
                    })
                    .unwrap_or_else(|| tr(self.locale, dismiss_id).into_owned());
                let rect = Rect::new(content.x, content.y + row as u16, content.width, 1);
                let style = if index == self.selected {
                    codewhale_tui_kit::menu_style::selected_row_style()
                } else {
                    Style::default().fg(palette::TEXT_PRIMARY)
                };
                Paragraph::new(label).style(style).render(rect, buf);
                buttons.push((index, rect));
            }
            return;
        }
        let options: Vec<(usize, MessageId, Option<ShellBindingId>)> = if self.models_unavailable {
            vec![
                (0, MessageId::SetupActionRetry, None),
                (1, MessageId::ConnectionSetupLocal, None),
                (2, dismiss_id, Some(ShellBindingId::AccountLoginCancel)),
            ]
        } else if self.url.is_some() {
            vec![
                (
                    0,
                    MessageId::AccountLoginOpenBrowser,
                    Some(ShellBindingId::AccountLoginOpenBrowser),
                ),
                (
                    1,
                    MessageId::AccountLoginCopyLink,
                    Some(ShellBindingId::AccountLoginCopyLink),
                ),
                (2, dismiss_id, Some(ShellBindingId::AccountLoginCancel)),
            ]
        } else {
            vec![(2, dismiss_id, Some(ShellBindingId::AccountLoginCancel))]
        };
        let options_height = options.len().min(usize::from(content.height)) as u16;
        let text_area = Rect {
            height: content.height.saturating_sub(options_height),
            ..content
        };
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(text_area, buf);
        let mut buttons = self.buttons.borrow_mut();
        buttons.clear();
        for (row, (index, id, shortcut)) in options.iter().enumerate() {
            let rect = Rect::new(
                content.x,
                content.bottom().saturating_sub(options_height) + row as u16,
                content.width,
                1,
            );
            let selected = *index == if options.len() == 1 { 2 } else { self.selected };
            let style = if selected {
                codewhale_tui_kit::menu_style::selected_row_style()
            } else {
                Style::default().fg(palette::TEXT_PRIMARY)
            };
            let chord = shortcut.map(|id| binding(id).footer_chord).unwrap_or("");
            Paragraph::new(format!("{chord}  {}", tr(self.locale, *id)))
                .style(style)
                .render(rect, buf);
            buttons.push((*index, rect));
        }
    }

    fn tick(&mut self) -> ViewAction {
        let outcome = self.worker.as_ref().map(|worker| worker.try_recv());
        match outcome {
            Some(Ok(result)) => {
                self.worker = None;
                match result {
                    WorkerResult::Started(login) => {
                        self.cancellation = Some(login.cancellation());
                        self.url = Some(login.prompt().verification_uri_complete.clone());
                        self.code = Some(login.prompt().user_code.clone());
                        self.login = Some(login);
                        self.next_poll = Instant::now();
                        if std::env::var_os("SSH_CONNECTION").is_none()
                            && std::env::var_os("SSH_TTY").is_none()
                        {
                            self.open_browser();
                        }
                        return ViewAction::Redraw;
                    }
                    WorkerResult::Polled(login, AccountLoginPoll::Pending { retry_after }) => {
                        self.login = Some(login);
                        self.next_poll = Instant::now() + retry_after;
                    }
                    WorkerResult::Polled(_, AccountLoginPoll::Complete { .. })
                        if self.connecting_models =>
                    {
                        self.load_models();
                        return ViewAction::Redraw;
                    }
                    WorkerResult::Models(models) => {
                        self.models_unavailable =
                            models.models.is_empty() || models.credential.is_none();
                        if !self.models_unavailable {
                            self.selected = models
                                .models
                                .iter()
                                .position(|model| Some(&model.id) == models.default_model.as_ref())
                                .unwrap_or(0);
                            self.models = Some(models);
                        }
                        return ViewAction::Redraw;
                    }
                    WorkerResult::ModelsFailed => {
                        self.models_unavailable = true;
                        self.models_failed = true;
                        return ViewAction::Redraw;
                    }
                    WorkerResult::Polled(_, AccountLoginPoll::Complete { .. })
                    | WorkerResult::SignedIn
                    | WorkerResult::SignedOut(true) => {
                        return self.finish(AccountOperationOutcome::Complete);
                    }
                    WorkerResult::SignedOut(false) => {
                        return self.finish(AccountOperationOutcome::LocalSignOut);
                    }
                    WorkerResult::Polled(_, AccountLoginPoll::Cancelled) => {
                        return self.finish(AccountOperationOutcome::Cancelled);
                    }
                    WorkerResult::Polled(_, AccountLoginPoll::Expired) => {
                        return self.finish(AccountOperationOutcome::Expired);
                    }
                    WorkerResult::Cancelled => {
                        return self.finish(AccountOperationOutcome::Cancelled);
                    }
                    WorkerResult::Failed => return self.finish(AccountOperationOutcome::Failed),
                }
            }
            Some(Err(TryRecvError::Disconnected)) => {
                return self.finish(AccountOperationOutcome::Failed);
            }
            Some(Err(TryRecvError::Empty)) | None => {}
        }
        if self.worker.is_none()
            && Instant::now() >= self.next_poll
            && let Some(mut login) = self.login.take()
        {
            let (sender, receiver) = mpsc::channel();
            self.worker = Some(receiver);
            tokio::task::spawn_blocking(move || {
                let result = match login.poll() {
                    Ok(result) => WorkerResult::Polled(login, result),
                    Err(_) => WorkerResult::Failed,
                };
                let _ = sender.send(result);
            });
        }
        ViewAction::None
    }
}

pub(crate) struct ConnectionSetupView {
    locale: Locale,
    selected: usize,
    buttons: RefCell<Vec<(usize, Rect)>>,
}

impl ConnectionSetupView {
    pub(crate) fn new(locale: Locale) -> Self {
        Self {
            locale,
            selected: 0,
            buttons: RefCell::new(Vec::new()),
        }
    }

    fn choose(&self, selected: Option<usize>) -> ViewAction {
        ViewAction::EmitAndClose(ViewEvent::AccountConnectionChoice {
            account: selected.map(|selected| selected == 0),
        })
    }
}

impl ModalView for ConnectionSetupView {
    fn kind(&self) -> ModalKind {
        ModalKind::AccountLogin
    }

    fn set_locale(&mut self, locale: Locale) {
        self.locale = locale;
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn handle_key(&mut self, key: KeyEvent) -> ViewAction {
        if key.kind != KeyEventKind::Press {
            return ViewAction::None;
        }
        if binding(ShellBindingId::AccountLoginCancel).matches(&key)
            || key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL
        {
            return self.choose(None);
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
        {
            return ViewAction::None;
        }
        match key.code {
            KeyCode::Up | KeyCode::Down | KeyCode::Tab | KeyCode::BackTab => {
                self.selected = 1 - self.selected;
                ViewAction::Redraw
            }
            KeyCode::Enter => self.choose(Some(self.selected)),
            _ => ViewAction::None,
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> ViewAction {
        if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            return ViewAction::None;
        }
        let selected = self.buttons.borrow().iter().find_map(|(index, rect)| {
            crate::tui::mouse_ui::mouse_hits_rect(mouse, Some(*rect)).then_some(*index)
        });
        selected.map_or(ViewAction::None, |selected| self.choose(Some(selected)))
    }

    fn occupied_region(&self, area: Rect) -> Rect {
        centered_modal_area(area, 76, 13, 28, 8)
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        let popup = self.occupied_region(area);
        render_modal_surface(area, popup, buf);
        let block = Block::default()
            .title(tr(self.locale, MessageId::ConnectionSetupTitle).into_owned())
            .borders(Borders::ALL)
            .border_style(Style::default().fg(palette::BORDER_COLOR))
            .style(Style::default().bg(palette::WHALE_BG))
            .padding(Padding::uniform(1));
        let inner = block.inner(popup);
        block.render(popup, buf);
        let content = render_modal_footer(
            inner,
            buf,
            &[
                ActionHint::new(
                    binding(ShellBindingId::AccountLoginNext).footer_chord,
                    tr(self.locale, MessageId::PickerActionMove),
                ),
                ActionHint::new(
                    binding(ShellBindingId::AccountLoginSelect).footer_chord,
                    tr(self.locale, MessageId::PickerActionApply),
                ),
                ActionHint::new(
                    binding(ShellBindingId::AccountLoginCancel).footer_chord,
                    tr(self.locale, MessageId::PickerActionCancel),
                ),
            ],
        );
        let mut buttons = self.buttons.borrow_mut();
        buttons.clear();
        let rows = [
            (
                MessageId::ConnectionSetupAccount,
                MessageId::ConnectionSetupAccountDetail,
            ),
            (
                MessageId::ConnectionSetupLocal,
                MessageId::ConnectionSetupLocalDetail,
            ),
        ];
        let height = content.height / 2;
        for (index, (label, detail)) in rows.into_iter().enumerate() {
            let rect = Rect::new(
                content.x,
                content.y + index as u16 * height,
                content.width,
                height,
            );
            let style = if self.selected == index {
                codewhale_tui_kit::menu_style::selected_row_style()
            } else {
                Style::default().fg(palette::TEXT_PRIMARY)
            };
            Paragraph::new(vec![
                Line::from(Span::styled(
                    tr(self.locale, label).into_owned(),
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Line::from(tr(self.locale, detail).into_owned()),
            ])
            .style(style)
            .wrap(Wrap { trim: false })
            .render(rect, buf);
            buttons.push((index, rect));
        }
    }
}
