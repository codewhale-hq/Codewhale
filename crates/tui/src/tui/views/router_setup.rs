//! Router setup (#6525): the one view behind `/router` and `/model router`.
//!
//! It writes the existing `[auto.router]` table and nothing else — there is
//! no second routing system. Presets derive from route capabilities (the
//! decision API a key can reach, the active provider's runnable fast tier),
//! never from model-name tables:
//!
//! * **Jev** — TypeSafe's decision model through OpenRouter or TypeSafe
//!   direct, whichever key exists (OpenRouter first).
//! * **Fast** — the active provider's runnable fast sibling, thinking off.
//! * **Off** — removes `[auto.router]`; Auto takes its local fallback: the
//!   default model, or the fast tier when `[auto] cost_saving` is on (Off
//!   leaves that separate opt-in alone and says so).
//! * **Custom** — shows the TOML to edit by hand.
//!
//! Choosing Jev or Fast makes exactly one test call through the per-turn
//! routing path and shows the result before anything is saved; saving is a
//! separate, explicit step. No preset is ever elected automatically. The test
//! call's usage enters session cost like any routing call, and a save is
//! published to the runtime threads as well as the UI's config.
//!
//! Known limits: "Free OpenRouter model" and "Local model" presets need live
//! catalog and reachability probes and are not offered yet; the `/model`
//! picker "Router" row is a follow-up.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Padding, Paragraph, Widget, Wrap},
};

use crate::tui::app::{App, StatusToastLevel};
use crate::tui::history::HistoryCell;
use crate::tui::views::{
    ActionHint, CommandPaletteAction, ModalKind, ModalView, ViewAction, ViewEvent,
    centered_modal_area, render_modal_footer, render_modal_surface,
};
use codewhale_config::ProviderKind;
use codewhale_engine::client::system_one::DecisionRouterRoute;
use codewhale_engine::model_inventory::ModelInventory;
use codewhale_engine::model_routing::{AutoRouteSelection, provider_router_candidates};
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_palette as palette;
use codewhale_runtime::config::{AutoConfig, AutoRouterConfig, Config};
use codewhale_tui_kit::menu_style;

/// Pinned OpenRouter id for Jev; `~typesafe/jev-latest` is also accepted.
pub(crate) const JEV_OPENROUTER_MODEL: &str = "typesafe/jev-1.13";
/// TypeSafe direct model id.
pub(crate) const JEV_TYPESAFE_MODEL: &str = "jev-latest";
/// Router timeout the Jev preset writes; Jev's p50 is ~0.1–0.2 s.
const JEV_TIMEOUT_SECS: u64 = 2;

/// A Router setup preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RouterPreset {
    /// Jev; `None` picks whichever route has a key (OpenRouter first).
    Jev(Option<DecisionRouterRoute>),
    Fast,
    Off,
    Custom,
}

impl RouterPreset {
    /// Parse `jev [openrouter|typesafe]`, `fast`, `off` or `custom`.
    #[must_use]
    pub(crate) fn parse(args: &str) -> Option<Self> {
        let mut words = args.split_whitespace();
        let preset = match words.next()?.to_ascii_lowercase().as_str() {
            "jev" => Self::Jev(match words.next() {
                None => None,
                Some(route) => Some(DecisionRouterRoute::parse(route)?),
            }),
            "fast" => Self::Fast,
            "off" | "none" => Self::Off,
            "custom" => Self::Custom,
            _ => return None,
        };
        words.next().is_none().then_some(preset)
    }

    /// The `/router` arguments that select this preset.
    #[must_use]
    pub(crate) fn command_args(self) -> String {
        match self {
            Self::Jev(None) => "jev".to_string(),
            Self::Jev(Some(route)) => format!("jev {}", route.as_str()),
            Self::Fast => "fast".to_string(),
            Self::Off => "off".to_string(),
            Self::Custom => "custom".to_string(),
        }
    }
}

/// What `/router` asked the app to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RouterRequest {
    /// Open the Router setup view.
    Open,
    /// Make one test call with this preset and show the result.
    Test(RouterPreset),
    /// Persist this preset to `config.toml` and the live config.
    Save(RouterPreset),
}

/// Parse `/router` (or `/model router`) arguments.
pub(crate) fn parse_router_args(args: Option<&str>) -> Result<RouterRequest, String> {
    let args = args.map(str::trim).unwrap_or_default();
    if args.is_empty() {
        return Ok(RouterRequest::Open);
    }
    let (save, rest) = match args.split_once(char::is_whitespace) {
        Some((head, rest)) if head.eq_ignore_ascii_case("save") => (true, rest.trim()),
        _ => (false, args),
    };
    let preset = RouterPreset::parse(rest).ok_or_else(|| {
        "Usage: /router [jev [openrouter|typesafe]|fast|off|custom] · /router save <preset>"
            .to_string()
    })?;
    Ok(if save {
        RouterRequest::Save(preset)
    } else {
        RouterRequest::Test(preset)
    })
}

/// The `[auto.router]` table a preset writes for this config, `Ok(None)` for
/// Off, or a non-secret reason the preset is unavailable.
pub(crate) fn preset_router_config(
    config: &Config,
    preset: RouterPreset,
    locale: Locale,
) -> Result<Option<AutoRouterConfig>, String> {
    match preset {
        RouterPreset::Off => Ok(None),
        RouterPreset::Custom => Err(tr(locale, MessageId::RouterCustomByHand).into_owned()),
        RouterPreset::Jev(route) => {
            let route = match route {
                Some(route) if route.has_key(config) => route,
                Some(route) => {
                    return Err(no_key_reason(locale, route));
                }
                None => [
                    DecisionRouterRoute::Openrouter,
                    DecisionRouterRoute::Typesafe,
                ]
                .into_iter()
                .find(|route| route.has_key(config))
                .ok_or_else(|| tr(locale, MessageId::RouterNeedKey).into_owned())?,
            };
            let model = match route {
                DecisionRouterRoute::Openrouter => JEV_OPENROUTER_MODEL,
                DecisionRouterRoute::Typesafe => JEV_TYPESAFE_MODEL,
            };
            Ok(Some(AutoRouterConfig {
                kind: Some("decision".to_string()),
                provider: Some(route.as_str().to_string()),
                model: Some(model.to_string()),
                timeout_secs: Some(JEV_TIMEOUT_SECS),
                min_confidence: Some(codewhale_runtime::config::DEFAULT_AUTO_ROUTER_MIN_CONFIDENCE),
                ..Default::default()
            }))
        }
        RouterPreset::Fast => {
            let (provider, fast) = runnable_fast_tier(config)
                .ok_or_else(|| tr(locale, MessageId::RouterNoFastTier).into_owned())?;
            Ok(Some(AutoRouterConfig {
                kind: Some("chat".to_string()),
                provider: Some(provider.as_str().to_string()),
                model: Some(fast),
                thinking: Some("off".to_string()),
                ..Default::default()
            }))
        }
    }
}

/// What Off does for this config: `[auto] cost_saving` is a separate opt-in
/// that Off leaves in place, so its fallback is the fast tier, not the default.
fn off_hint(config: &Config, locale: Locale) -> String {
    let id = if config.auto_cost_saving() {
        MessageId::RouterPresetOffCostSavingHint
    } else {
        MessageId::RouterPresetOffHint
    };
    tr(locale, id).into_owned()
}

fn no_key_reason(locale: Locale, route: DecisionRouterRoute) -> String {
    tr(locale, MessageId::RouterNoKey).replace("{route}", route.display_name())
}

/// The active provider's runnable fast sibling, from route capabilities.
fn runnable_fast_tier(config: &Config) -> Option<(ProviderKind, String)> {
    let inventory = ModelInventory::from_config(config).ok()?;
    let active = inventory.active_default()?;
    let fast = provider_router_candidates(active.provider, &active.model).cheap?;
    let candidate = inventory
        .candidate(active.identity.key.as_str(), &fast)
        .filter(|candidate| candidate.readiness.can_attempt())?;
    Some((active.provider, candidate.model.clone()))
}

/// Write `router` as `[auto.router]` (or remove the table for `None`) through
/// the single config writer, replacing any previous router keys.
pub(crate) fn persist_auto_router(
    config_path: Option<&Path>,
    router: Option<&AutoRouterConfig>,
) -> anyhow::Result<PathBuf> {
    use codewhale_runtime::config_persistence::{
        config_toml_path, mutate_config_document, set_document_value, unset_document_value,
    };
    let path = config_toml_path(config_path)?;
    mutate_config_document(&path, |doc| {
        unset_document_value(doc, &["auto", "router"])?;
        let Some(router) = router else {
            return Ok(());
        };
        let strings = [
            ("kind", router.kind.as_deref()),
            ("provider", router.provider.as_deref()),
            ("model", router.model.as_deref()),
            ("thinking", router.thinking.as_deref()),
            ("base_url", router.base_url.as_deref()),
        ];
        for (key, value) in strings {
            if let Some(value) = value {
                set_document_value(doc, &["auto", "router", key], value)?;
            }
        }
        if let Some(secs) = router.timeout_secs {
            let secs = i64::try_from(secs).unwrap_or(i64::MAX);
            set_document_value(doc, &["auto", "router", "timeout_secs"], secs)?;
        }
        if let Some(min_confidence) = router.min_confidence {
            set_document_value(doc, &["auto", "router", "min_confidence"], min_confidence)?;
        }
        Ok(())
    })?;
    Ok(path)
}

/// Handle a `/router` request on the UI loop, where the live config lives.
pub(crate) async fn handle_router_request(
    app: &mut App,
    config: &mut Config,
    task_manager: &codewhale_engine::task_manager::TaskManager,
    request: RouterRequest,
) {
    match request {
        RouterRequest::Open => {
            if app.view_stack.top_kind() != Some(ModalKind::RouterSetup) {
                app.view_stack
                    .push(RouterSetupView::picker(config, app.ui_locale));
            }
        }
        RouterRequest::Test(RouterPreset::Custom) => {
            app.add_message(HistoryCell::System {
                content: custom_router_help(config, app.ui_locale),
            });
        }
        RouterRequest::Test(preset) => test_preset(app, config, preset).await,
        RouterRequest::Save(preset) => save_preset(app, config, task_manager, preset).await,
    }
}

async fn test_preset(app: &mut App, config: &Config, preset: RouterPreset) {
    let locale = app.ui_locale;
    let router = match preset_router_config(config, preset, locale) {
        Ok(router) => router,
        Err(reason) => {
            app.push_status_toast(
                tr(app.ui_locale, MessageId::RouterPresetUnavailable).replace("{reason}", &reason),
                StatusToastLevel::Warning,
                Some(6_000),
            );
            return;
        }
    };
    let lines = match router.as_ref() {
        None => vec![off_hint(config, locale)],
        Some(router) => {
            // The test call awaits a provider inline on the UI loop; refuse
            // while Runtime Chat owns the run, as other inline calls do.
            if app.remote_control.runtime_chat_blocks_local_dispatch() {
                app.push_status_toast(
                    tr(app.ui_locale, MessageId::SettingLockedDuringTurn)
                        .replace("{setting}", "Codewhale Runtime"),
                    StatusToastLevel::Info,
                    Some(6_000),
                );
                return;
            }
            let mut candidate = config.clone();
            candidate
                .auto
                .get_or_insert_with(AutoConfig::default)
                .router = Some(router.clone());
            let mut lines = vec![router_summary(router, locale)];
            // Captured before the call, like any background provider request.
            let cost_scope = codewhale_engine::cost_status::scope_token();
            match codewhale_engine::model_routing::test_auto_router(&candidate).await {
                Ok((selection, latency_ms)) => {
                    // The test call is real spend: settle it into session
                    // cost exactly as a turn's routing call is settled.
                    codewhale_engine::cost_status::report_runtime_usage_batch(
                        cost_scope,
                        None,
                        &codewhale_engine::cost_status::RuntimeUsageBatch {
                            decisions: Vec::new(),
                            records: selection.routed_usage.clone(),
                            drop_records: selection.routed_usage_drop_records.clone(),
                            dropped_records: selection.routed_usage_dropped_records,
                        },
                    );
                    lines.extend(describe_test_selection(&selection, latency_ms, locale));
                }
                Err(reason) => lines
                    .push(tr(locale, MessageId::RouterTestNotMade).replace("{reason}", &reason)),
            }
            lines
        }
    };
    app.add_message(HistoryCell::System {
        content: format!(
            "{}\n{}",
            tr(locale, MessageId::RouterTestHeader).replace("{args}", &preset.command_args()),
            lines.join("\n")
        ),
    });
    app.view_stack
        .push(RouterSetupView::confirm(preset, lines, app.ui_locale));
}

async fn save_preset(
    app: &mut App,
    config: &mut Config,
    task_manager: &codewhale_engine::task_manager::TaskManager,
    preset: RouterPreset,
) {
    let locale = app.ui_locale;
    let router = match preset_router_config(config, preset, locale) {
        Ok(router) => router,
        Err(reason) => {
            app.add_message(HistoryCell::System {
                content: tr(locale, MessageId::RouterPresetUnavailable)
                    .replace("{reason}", &reason),
            });
            return;
        }
    };
    // The config writer reads, locks and rewrites config.toml; keep that
    // file IO off the UI runtime (#6149).
    let config_path = app.config_path.clone();
    let to_write = router.clone();
    let persisted = tokio::task::spawn_blocking(move || {
        persist_auto_router(config_path.as_deref(), to_write.as_ref())
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|result| result);
    match persisted {
        Ok(path) => {
            config.auto.get_or_insert_with(AutoConfig::default).router = router.clone();
            let what = router.as_ref().map_or_else(
                || tr(locale, MessageId::ConfigValueOff).into_owned(),
                |router| router_summary(router, locale),
            );
            app.add_message(HistoryCell::System {
                content: tr(locale, MessageId::RouterSaved)
                    .replace("{router}", &what)
                    .replace("{path}", &path.display().to_string()),
            });
            // Runtime-chat and queued runtime turns resolve Auto from the
            // runtime manager's own config snapshot; publish the new router
            // there too, or they keep routing with the old one.
            let runtime_router = router.clone();
            if let Err(error) = task_manager
                .reload_runtime_config_with(|runtime| {
                    runtime.auto.get_or_insert_with(AutoConfig::default).router = runtime_router;
                })
                .await
            {
                app.add_message(HistoryCell::Error {
                    message: tr(locale, MessageId::RouterRuntimeNotReloaded)
                        .replace("{error}", &error.to_string()),
                    severity: codewhale_error_taxonomy::ErrorSeverity::Warning,
                });
            }
        }
        Err(error) => app.add_message(HistoryCell::Error {
            message: tr(locale, MessageId::RouterNotSaved).replace("{error}", &error.to_string()),
            severity: codewhale_error_taxonomy::ErrorSeverity::Error,
        }),
    }
}

fn router_summary(router: &AutoRouterConfig, locale: Locale) -> String {
    let kind = router.kind.as_deref().unwrap_or("chat");
    let provider = router.provider.as_deref().unwrap_or("?");
    let model = router.model.as_deref().unwrap_or("?");
    let id = if router.thinking.is_some() {
        MessageId::RouterSummaryThinking
    } else {
        MessageId::RouterSummary
    };
    tr(locale, id)
        .replace("{kind}", kind)
        .replace("{provider}", provider)
        .replace("{model}", model)
        .replace("{thinking}", router.thinking.as_deref().unwrap_or_default())
}

/// The test result: which tier it picked, why, and what the call cost.
fn describe_test_selection(
    selection: &AutoRouteSelection,
    latency_ms: u64,
    locale: Locale,
) -> Vec<String> {
    let percent = |bp: u16| format!("{}%", (u32::from(bp) + 50) / 100);
    let mut lines = vec![
        tr(locale, MessageId::RouterTestWouldRoute)
            .replace(
                "{request}",
                codewhale_engine::model_routing::ROUTER_TEST_REQUEST,
            )
            .replace(
                "{provider}",
                selection
                    .provider
                    .compatibility()
                    .map_or(selection.provider.key.as_str(), |row| row.label),
            )
            .replace("{model}", &selection.model)
            .replace("{latency}", &latency_ms.to_string()),
    ];
    let Some(receipt) = selection.receipt.as_ref() else {
        return lines;
    };
    lines.push(
        tr(locale, MessageId::RouterTestDecision)
            .replace("{tier}", receipt.tier.label())
            .replace("{reason}", &receipt.reason.label()),
    );
    if let Some(decision) = receipt.decision.as_ref() {
        let probabilities = decision
            .probabilities_bp
            .iter()
            .map(|(option, bp)| format!("{option} {}", percent(*bp)))
            .collect::<Vec<_>>()
            .join(" · ");
        lines.push(
            tr(locale, MessageId::RouterTestChoice)
                .replace("{choice}", &decision.choice)
                .replace("{probabilities}", &probabilities)
                .replace("{confidence}", &percent(decision.confidence_bp))
                .replace("{min}", &percent(decision.min_confidence_bp)),
        );
        lines.push(match decision.provider_reported_cost_usd.as_deref() {
            Some(cost) => tr(locale, MessageId::RouterTestCostReported).replace("{cost}", cost),
            None => tr(locale, MessageId::RouterTestCostUnreported).into_owned(),
        });
    } else if let Some(usage) = selection.routed_usage.first() {
        lines.push(
            tr(locale, MessageId::RouterTestUsage)
                .replace("{input}", &usage.usage.usage.input_tokens.to_string())
                .replace("{output}", &usage.usage.usage.output_tokens.to_string()),
        );
    }
    if let Some(failure) = receipt.router_failure {
        lines.push(tr(locale, MessageId::RouterTestFailing).replace("{reason}", &failure.label()));
    }
    lines
}

fn custom_router_help(config: &Config, locale: Locale) -> String {
    let current = config
        .auto
        .as_ref()
        .and_then(|auto| auto.router.as_ref())
        .map_or_else(
            || tr(locale, MessageId::ConfigValueOff).into_owned(),
            |router| router_summary(router, locale),
        );
    format!(
        "{}\n{}\n\n\
[auto.router]\n\
kind = \"chat\"            # or \"decision\" for Jev\n\
provider = \"deepseek\"    # decision: \"openrouter\" or \"typesafe\"\n\
model = \"deepseek-flash\"\n\
thinking = \"off\"         # chat routers only\n\
timeout_secs = 4\n\
# min_confidence = 0.5     # decision routers only\n\n\
{}",
        tr(locale, MessageId::RouterCurrent).replace("{router}", &current),
        tr(locale, MessageId::RouterCustomHelpIntro),
        tr(locale, MessageId::RouterCustomHelpOutro),
    )
}

struct PresetRow {
    preset: RouterPreset,
    label: String,
    hint: String,
    available: bool,
}

enum Mode {
    Pick,
    Confirm {
        preset: RouterPreset,
        lines: Vec<String>,
    },
}

/// The Router setup view: a preset picker, then a test-result confirmation.
pub(crate) struct RouterSetupView {
    mode: Mode,
    rows: Vec<PresetRow>,
    current: String,
    cursor: usize,
    locale: Locale,
    /// `(row index, rect)` for the rows actually drawn this frame.
    row_hitboxes: RefCell<Vec<(usize, Rect)>>,
}

impl RouterSetupView {
    fn picker(config: &Config, locale: Locale) -> Self {
        let unavailable = |reason: String| {
            tr(locale, MessageId::RouterPresetUnavailable).replace("{reason}", &reason)
        };
        let mut rows = Vec::new();
        for route in [
            DecisionRouterRoute::Openrouter,
            DecisionRouterRoute::Typesafe,
        ] {
            let preset = RouterPreset::Jev(Some(route));
            let mut hint =
                tr(locale, MessageId::RouterPresetJevHint).replace("{route}", route.display_name());
            let available = route.has_key(config);
            if !available {
                hint = unavailable(no_key_reason(locale, route));
            }
            if route == DecisionRouterRoute::Typesafe {
                hint = format!(
                    "{hint} · {}",
                    tr(locale, MessageId::RouterTypesafeSignupPaused)
                );
            }
            rows.push(PresetRow {
                preset,
                label: format!("Jev · {}", route.display_name()),
                hint,
                available,
            });
        }
        let fast = runnable_fast_tier(config);
        rows.push(PresetRow {
            preset: RouterPreset::Fast,
            label: tr(locale, MessageId::RouterPresetFastLabel).into_owned(),
            hint: match fast.as_ref() {
                Some((provider, model)) => tr(locale, MessageId::RouterPresetFastHint)
                    .replace("{model}", model)
                    .replace("{provider}", provider.provider().display_name()),
                None => unavailable(tr(locale, MessageId::RouterNoFastTier).into_owned()),
            },
            available: fast.is_some(),
        });
        rows.push(PresetRow {
            preset: RouterPreset::Off,
            label: tr(locale, MessageId::ConfigValueOff).into_owned(),
            hint: off_hint(config, locale),
            available: true,
        });
        rows.push(PresetRow {
            preset: RouterPreset::Custom,
            label: tr(locale, MessageId::RouterPresetCustomLabel).into_owned(),
            hint: tr(locale, MessageId::RouterPresetCustomHint).into_owned(),
            available: true,
        });
        let inventory = ModelInventory::from_config(config);
        let current = match config.auto.as_ref().and_then(|auto| auto.router.as_ref()) {
            None => tr(locale, MessageId::RouterCurrent)
                .replace("{router}", &tr(locale, MessageId::ConfigValueOff)),
            Some(router) => match inventory {
                Err(reason) => tr(locale, MessageId::RouterCurrentFailing)
                    .replace("{router}", &router_summary(router, locale))
                    .replace("{reason}", &reason),
                Ok(inventory) => match inventory.router_setup_issue {
                    Some(issue) => tr(locale, MessageId::RouterCurrentFailing)
                        .replace("{router}", &router_summary(router, locale))
                        .replace("{reason}", issue.label()),
                    None => tr(locale, MessageId::RouterCurrent)
                        .replace("{router}", &router_summary(router, locale)),
                },
            },
        };
        Self {
            mode: Mode::Pick,
            rows,
            current,
            cursor: 0,
            locale,
            row_hitboxes: RefCell::new(Vec::new()),
        }
    }

    fn confirm(preset: RouterPreset, lines: Vec<String>, locale: Locale) -> Self {
        Self {
            mode: Mode::Confirm { preset, lines },
            rows: Vec::new(),
            current: String::new(),
            cursor: 0,
            locale,
            row_hitboxes: RefCell::new(Vec::new()),
        }
    }

    fn run(command: String) -> ViewAction {
        ViewAction::EmitAndClose(ViewEvent::CommandPaletteSelected {
            action: CommandPaletteAction::ExecuteCommand { command },
        })
    }

    fn select(&self) -> ViewAction {
        match &self.mode {
            Mode::Confirm { preset, .. } => {
                Self::run(format!("/router save {}", preset.command_args()))
            }
            Mode::Pick => match self.rows.get(self.cursor) {
                Some(row) if row.available => {
                    Self::run(format!("/router {}", row.preset.command_args()))
                }
                _ => ViewAction::None,
            },
        }
    }
}

impl ModalView for RouterSetupView {
    fn kind(&self) -> ModalKind {
        ModalKind::RouterSetup
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn handle_key(&mut self, key: KeyEvent) -> ViewAction {
        if matches!(self.mode, Mode::Pick)
            && let Some(motion) = codewhale_tui_kit::list_nav::motion(&key)
            && let Some(next) = codewhale_tui_kit::list_nav::apply(
                self.cursor,
                self.rows.len(),
                self.rows.len(),
                motion,
            )
        {
            self.cursor = next;
            return ViewAction::None;
        }
        match key.code {
            KeyCode::Esc => ViewAction::Close,
            KeyCode::Enter => self.select(),
            _ => ViewAction::None,
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> ViewAction {
        if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
            let clicked = self
                .row_hitboxes
                .borrow()
                .iter()
                .find(|(_, rect)| {
                    rect.contains(ratatui::layout::Position::new(mouse.column, mouse.row))
                })
                .map(|(index, _)| *index);
            if let Some(index) = clicked {
                self.cursor = index;
                return self.select();
            }
        }
        ViewAction::None
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        let body_rows = match &self.mode {
            Mode::Pick => self.rows.len() * 2 + 2,
            Mode::Confirm { lines, .. } => lines.len() + 3,
        };
        let popup_height = u16::try_from(body_rows)
            .unwrap_or(u16::MAX)
            .saturating_add(6);
        let popup_area = centered_modal_area(area, 96, popup_height, 44, 10);
        render_modal_surface(area, popup_area, buf);

        let title = format!(" {} ", tr(self.locale, MessageId::RouterSetupTitle));
        let block = Block::default()
            .title(Line::from(Span::styled(
                title,
                Style::default()
                    .fg(palette::WHALE_ACTION)
                    .add_modifier(Modifier::BOLD),
            )))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(palette::BORDER_COLOR))
            .style(Style::default().bg(palette::WHALE_BG))
            .padding(Padding::uniform(1));
        let inner = block.inner(popup_area);
        block.render(popup_area, buf);

        let locale = self.locale;
        let hints = match self.mode {
            Mode::Pick => vec![
                ActionHint::new("↑/↓", tr(locale, MessageId::PickerActionMove)),
                ActionHint::new("Enter", tr(locale, MessageId::RouterActionTest)),
                ActionHint::new("Esc", tr(locale, MessageId::SessionsActionClose)),
            ],
            Mode::Confirm { .. } => vec![
                ActionHint::new("Enter", tr(locale, MessageId::RouterActionSave)),
                ActionHint::new("Esc", tr(locale, MessageId::RouterActionDiscard)),
            ],
        };
        let content = render_modal_footer(inner, buf, &hints);
        self.row_hitboxes.borrow_mut().clear();

        let muted = Style::default().fg(palette::TEXT_MUTED);
        let mut lines = Vec::new();
        match &self.mode {
            Mode::Confirm { lines: result, .. } => {
                for line in result {
                    lines.push(Line::from(Span::styled(
                        line.clone(),
                        Style::default().fg(palette::TEXT_PRIMARY),
                    )));
                }
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    tr(self.locale, MessageId::RouterTestSaveHint).into_owned(),
                    muted,
                )));
            }
            Mode::Pick => {
                // Label + hint per row when everything fits; otherwise one
                // line per row, windowed so the focused row is always drawn
                // (compact terminals, e.g. 40x12).
                let height = usize::from(content.height);
                let width = usize::from(content.width);
                let two_line = height >= 2 + self.rows.len() * 2;
                let header = if two_line {
                    2
                } else {
                    usize::from(height >= 2)
                };
                let row_height: u16 = if two_line { 2 } else { 1 };
                let visible = (height.saturating_sub(header) / usize::from(row_height)).max(1);
                let start = self
                    .cursor
                    .saturating_sub(visible - 1)
                    .min(self.rows.len().saturating_sub(visible));
                if header > 0 {
                    lines.push(Line::from(Span::styled(
                        codewhale_tui_kit::ui_text::semantic_truncate(&self.current, width),
                        muted,
                    )));
                }
                if header > 1 {
                    lines.push(Line::from(""));
                }
                for (idx, row) in self.rows.iter().enumerate().skip(start).take(visible) {
                    let is_cursor = idx == self.cursor;
                    let label_style = if is_cursor {
                        menu_style::selected_row_style()
                    } else if row.available {
                        Style::default().fg(palette::TEXT_PRIMARY)
                    } else {
                        muted
                    };
                    let pointer = codewhale_tui_kit::glyphs::selection_marker(is_cursor);
                    let row_y = content
                        .y
                        .saturating_add(u16::try_from(lines.len()).unwrap_or(u16::MAX));
                    lines.push(Line::from(Span::styled(
                        codewhale_tui_kit::ui_text::semantic_truncate(
                            &format!("{pointer} {}", row.label),
                            width,
                        ),
                        label_style,
                    )));
                    if two_line {
                        lines.push(Line::from(Span::styled(
                            format!(
                                "    {}",
                                codewhale_tui_kit::ui_text::semantic_truncate(
                                    &row.hint,
                                    width.saturating_sub(4)
                                )
                            ),
                            muted,
                        )));
                    }
                    self.row_hitboxes
                        .borrow_mut()
                        .push((idx, Rect::new(content.x, row_y, content.width, row_height)));
                }
            }
        }
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(content, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tuple fields drop in order: restore the environment, then unlock.
    fn hermetic() -> (
        crate::test_support::EnvVarGuard,
        crate::test_support::EnvVarGuard,
        crate::test_support::TestEnvLock,
    ) {
        let lock = crate::test_support::lock_test_env();
        (
            crate::test_support::EnvVarGuard::remove("OPENROUTER_API_KEY"),
            crate::test_support::EnvVarGuard::remove("TYPESAFE_API_KEY"),
            lock,
        )
    }

    fn deepseek_with_openrouter_key(openrouter_key: bool) -> Config {
        {
            let __cw_config_init_0_0 = Some("deepseek".to_string());
            let __cw_config_init_0_1 = Some("deepseek-v4-pro".to_string());
            let __cw_config_init_0_2 = Some(codewhale_runtime::config::ProvidersConfig {
                deepseek: codewhale_runtime::config::ProviderConfig {
                    api_key: Some("ds-test-key".to_string()),
                    ..Default::default()
                },
                openrouter: codewhale_runtime::config::ProviderConfig {
                    api_key: openrouter_key.then(|| "or-test-key".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            });
            let mut __cw_config_init_0_value: Config = Default::default();
            __cw_config_init_0_value.provider = __cw_config_init_0_0;
            __cw_config_init_0_value.default_text_model = __cw_config_init_0_1;
            __cw_config_init_0_value.providers = __cw_config_init_0_2;
            __cw_config_init_0_value
        }
    }

    #[test]
    fn router_args_parse_to_requests() {
        assert_eq!(parse_router_args(None), Ok(RouterRequest::Open));
        assert_eq!(
            parse_router_args(Some("jev")),
            Ok(RouterRequest::Test(RouterPreset::Jev(None)))
        );
        assert_eq!(
            parse_router_args(Some("save jev typesafe")),
            Ok(RouterRequest::Save(RouterPreset::Jev(Some(
                DecisionRouterRoute::Typesafe
            ))))
        );
        assert_eq!(
            parse_router_args(Some("fast")),
            Ok(RouterRequest::Test(RouterPreset::Fast))
        );
        assert!(parse_router_args(Some("jev zai")).is_err());
        assert!(parse_router_args(Some("turbo")).is_err());
    }

    #[test]
    fn presets_derive_from_keys_and_route_capabilities() {
        let _env = hermetic();
        let config = deepseek_with_openrouter_key(true);
        let jev = preset_router_config(&config, RouterPreset::Jev(None), Locale::En)
            .expect("jev available")
            .expect("jev writes a table");
        assert_eq!(jev.kind.as_deref(), Some("decision"));
        assert_eq!(jev.provider.as_deref(), Some("openrouter"));
        assert_eq!(jev.model.as_deref(), Some(JEV_OPENROUTER_MODEL));
        assert_eq!(jev.timeout_secs, Some(2));

        let fast = preset_router_config(&config, RouterPreset::Fast, Locale::En)
            .expect("fast available")
            .expect("fast writes a table");
        assert_eq!(fast.kind.as_deref(), Some("chat"));
        assert_eq!(fast.provider.as_deref(), Some("deepseek"));
        assert_eq!(fast.model.as_deref(), Some("deepseek-flash"));
        assert_eq!(fast.thinking.as_deref(), Some("off"));

        assert!(matches!(
            preset_router_config(&config, RouterPreset::Off, Locale::En),
            Ok(None)
        ));

        // No key anywhere: Jev is unavailable, never guessed.
        let no_key = deepseek_with_openrouter_key(false);
        assert!(preset_router_config(&no_key, RouterPreset::Jev(None), Locale::En).is_err());
    }

    #[test]
    fn saving_a_preset_writes_exactly_the_router_table() {
        let _env = hermetic();
        let dir = tempfile::tempdir().expect("config dir");
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "# keep me\nprovider = \"deepseek\"\n\n[auto]\ncost_saving = true\n\n[auto.router]\nprovider = \"zai\"\nmodel = \"glm-5-turbo\"\nthinking = \"low\"\n",
        )
        .expect("seed config");
        let config = deepseek_with_openrouter_key(true);
        let jev = preset_router_config(&config, RouterPreset::Jev(None), Locale::En)
            .expect("jev")
            .expect("table");

        persist_auto_router(Some(&path), Some(&jev)).expect("persist jev");
        let written = std::fs::read_to_string(&path).expect("read back");
        assert!(written.contains("# keep me"), "{written}");
        let reloaded: Config = toml::from_str(&written).expect("reload");
        let auto = reloaded.auto.expect("[auto]");
        assert_eq!(auto.cost_saving, Some(true));
        let router = auto.router.expect("[auto.router]");
        assert_eq!(router.kind.as_deref(), Some("decision"));
        assert_eq!(router.provider.as_deref(), Some("openrouter"));
        assert_eq!(router.model.as_deref(), Some(JEV_OPENROUTER_MODEL));
        assert_eq!(router.thinking, None, "stale chat keys are replaced");
        assert_eq!(router.timeout_secs, Some(2));
        assert_eq!(router.min_confidence, Some(0.5));

        persist_auto_router(Some(&path), None).expect("persist off");
        let reloaded: Config =
            toml::from_str(&std::fs::read_to_string(&path).expect("read back")).expect("reload");
        assert!(reloaded.auto.expect("[auto] kept").router.is_none());
    }

    #[test]
    fn confirm_view_saves_only_on_enter() {
        use crossterm::event::KeyModifiers;
        let preset = RouterPreset::Jev(Some(DecisionRouterRoute::Openrouter));
        let mut view = RouterSetupView::confirm(preset, vec!["result".to_string()], Locale::En);
        match view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)) {
            ViewAction::EmitAndClose(ViewEvent::CommandPaletteSelected {
                action: CommandPaletteAction::ExecuteCommand { command },
            }) => assert_eq!(command, "/router save jev openrouter"),
            other => panic!("expected save command, got {other:?}"),
        }
        assert!(matches!(
            view.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            ViewAction::Close
        ));
    }

    fn rendered(view: &RouterSetupView, width: u16, height: u16) -> String {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
        (0..height)
            .map(|y| (0..width).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn compact_picker_keeps_the_focused_row_visible_and_clickable() {
        use crossterm::event::KeyModifiers;
        let _env = hermetic();
        let mut view = RouterSetupView::picker(&deepseek_with_openrouter_key(true), Locale::En);
        let last = view.rows.len() - 1;
        for _ in 0..last {
            view.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        assert_eq!(view.cursor, last);
        let text = rendered(&view, 40, 12);
        let custom = tr(Locale::En, MessageId::RouterPresetCustomLabel);
        assert!(
            text.contains(custom.as_ref()),
            "focused row hidden:\n{text}"
        );
        let hitboxes = view.row_hitboxes.borrow();
        assert!(
            hitboxes.iter().any(|(index, _)| *index == last),
            "focused row has no hitbox"
        );
        assert!(hitboxes.iter().all(|(_, rect)| rect.bottom() <= 12));
        drop(hitboxes);

        // A roomy frame still shows every row with its hint.
        let roomy = rendered(&view, 100, 40);
        assert!(roomy.contains(tr(Locale::En, MessageId::RouterPresetCustomHint).as_ref()));
        assert_eq!(view.row_hitboxes.borrow().len(), view.rows.len());
    }

    #[test]
    fn off_hint_names_the_cost_saving_fallback() {
        let _env = hermetic();
        let mut config = deepseek_with_openrouter_key(true);
        let off = |config: &Config| {
            RouterSetupView::picker(config, Locale::En)
                .rows
                .into_iter()
                .find(|row| row.preset == RouterPreset::Off)
                .expect("off row")
                .hint
        };
        assert_eq!(off(&config), tr(Locale::En, MessageId::RouterPresetOffHint));
        config.auto = Some(AutoConfig {
            cost_saving: Some(true),
            ..Default::default()
        });
        assert_eq!(
            off(&config),
            tr(Locale::En, MessageId::RouterPresetOffCostSavingHint)
        );
    }

    #[test]
    fn picker_and_reasons_follow_the_ui_locale() {
        let _env = hermetic();
        let config = deepseek_with_openrouter_key(false);
        let view = RouterSetupView::picker(&config, Locale::Ja);
        let fast = view
            .rows
            .iter()
            .find(|row| row.preset == RouterPreset::Fast)
            .expect("fast row");
        assert_eq!(fast.label, tr(Locale::Ja, MessageId::RouterPresetFastLabel));
        assert_ne!(fast.label, "Fast tier");
        let english = ["Current:", "no OpenRouter API key", "Custom"];
        let rendered: Vec<&str> = std::iter::once(view.current.as_str())
            .chain(
                view.rows
                    .iter()
                    .flat_map(|row| [row.label.as_str(), row.hint.as_str()]),
            )
            .collect();
        for text in &rendered {
            for word in english {
                assert!(
                    !text.contains(word),
                    "{word:?} left in Japanese view: {text}"
                );
            }
        }
        let reason = preset_router_config(&config, RouterPreset::Jev(None), Locale::Ja)
            .expect_err("no decision key");
        assert_eq!(reason, tr(Locale::Ja, MessageId::RouterNeedKey));
    }
}
