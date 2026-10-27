//! `/constitution` command surface (#3806).

use std::fmt::Write as _;
use std::path::PathBuf;

use codewhale_config::{
    ConstitutionChoice, ConstitutionSource, ConstitutionValidity, RuntimePostureSource, SetupState,
    SetupStep, UserConstitution, UserConstitutionLoad,
};

use crate::commands::traits::{CommandInfo, RegisterCommand};
use crate::tui::app::{App, AppAction};
use crate::tui::pager::PagerView;
use codewhale_localization::{Locale, MessageId};

use super::CommandResult;

pub(in crate::commands) const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "constitution",
    aliases: &["law"],
    usage: "/constitution [status|preview|base|suggestions|ratify <id>|migrate|rollback|bundled|edit|review|repair|repo|explain|posture|help]",
    description_id: MessageId::CmdConstitutionDescription,
};

pub(in crate::commands) struct ConstitutionCmd;

impl RegisterCommand for ConstitutionCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn execute(app: &mut App, arg: Option<&str>) -> CommandResult {
        match arg.map(str::trim).filter(|arg| !arg.is_empty()) {
            None | Some("status" | "home" | "manager") => {
                CommandResult::action(AppAction::OpenConstitutionManager { review: false })
            }
            Some("preview") => {
                open_preview(app);
                CommandResult::ok()
            }
            Some("review" | "existing") => {
                CommandResult::action(AppAction::OpenConstitutionManager { review: true })
            }
            Some("repo" | "repo-local" | "law") => {
                open_repo_law(app);
                CommandResult::ok()
            }
            Some("explain" | "agents") => {
                open_explanation(app);
                CommandResult::ok()
            }
            Some("edit" | "guided" | "custom") => {
                CommandResult::action(AppAction::OpenSetupWizardAt {
                    step: SetupStep::Constitution,
                })
            }
            Some("repair" | "fix") => CommandResult::with_message_and_action(
                repair_text(app.ui_locale),
                AppAction::OpenSetupWizardAt {
                    step: SetupStep::Constitution,
                },
            ),
            Some("posture" | "runtime-posture") => {
                CommandResult::action(AppAction::OpenSetupWizardAt {
                    step: SetupStep::TrustSandbox,
                })
            }
            Some("bundled" | "default" | "use-bundled" | "use-default") => {
                CommandResult::action(AppAction::UseBundledConstitution)
            }
            // The exact effective base prompt for the next turn (#3928). Routed
            // as an action because the assembly needs the session config, so the
            // preview is built by the same function the dispatch path uses
            // rather than a lookalike reconstruction.
            Some("base" | "prompt" | "base-prompt" | "effective") => {
                CommandResult::action(AppAction::PreviewEffectiveBasePrompt)
            }
            Some("suggestions" | "proposed") => {
                open_suggestions(app);
                CommandResult::ok()
            }
            Some("migrate") => CommandResult::message(migrate_text(app.ui_locale)),
            Some("rollback" | "undo-migrate") => {
                CommandResult::message(rollback_text(app.ui_locale))
            }
            Some(rest) if rest.starts_with("ratify") => {
                let ids: Vec<String> = rest
                    .trim_start_matches("ratify")
                    .split_whitespace()
                    .map(str::to_string)
                    .collect();
                let (message, ratified) = ratify_text(
                    app.ui_locale,
                    app.constitution_reviewed_digest.as_deref(),
                    &ids,
                );
                if ratified.is_some() {
                    app.constitution_reviewed_digest = ratified;
                }
                CommandResult::message(message)
            }
            Some("help") => CommandResult::message(help_text(app.ui_locale)),
            Some(other) => CommandResult::error(format!(
                "Unknown /constitution target '{other}'. Try `/constitution` for the manager."
            )),
        }
    }
}

pub(crate) fn open_manager(app: &mut App, review: bool, account: Result<bool, String>) {
    let locale = app.ui_locale;
    let mut text = format_status(app, locale, &account);
    let title = if review {
        let _ = write!(text, "\n\n{}", preview_text(locale));
        review_title(locale)
    } else {
        manager_title(locale)
    };
    open_pager(app, title, &text);
}

fn open_preview(app: &mut App) {
    let locale = app.ui_locale;
    let text = format!(
        "{}\n\n{}",
        match locale {
            Locale::ZhHans =>
                "这是本机宪章预览。已登录账户的宪章（包括默认值）优先；请在账户设置中预览。",
            _ =>
                "This previews the local constitution. The signed-in profile constitution, including its defaults, takes precedence; preview it in account settings.",
        },
        preview_text(locale)
    );
    open_pager(app, rendered_title(locale), &text);
}

fn open_repo_law(app: &mut App) {
    let locale = app.ui_locale;
    let text = match crate::project_context::load_repo_constitution_block(&app.workspace).0 {
        Some(block) => block,
        None => no_repo_law_text(locale).to_string(),
    };
    open_pager(app, repo_title(locale), &text);
}

fn open_explanation(app: &mut App) {
    let locale = app.ui_locale;
    open_pager(app, explanation_title(locale), agents_explanation(locale));
}

/// Suggestions review surface (#3930).
///
/// Suggested clauses are shown here and *only* here — they are absent from
/// `/constitution preview`, from the rendered block, and from the cache digest,
/// because unratified model advice is not law.
fn open_suggestions(app: &mut App) {
    let locale = app.ui_locale;
    let (text, reviewed) = suggestions_text(locale);
    app.constitution_reviewed_digest = reviewed;
    open_pager(app, suggestions_title(locale), &text);
}

fn suggestions_text(locale: Locale) -> (String, Option<String>) {
    let UserConstitutionStatus::Loaded { constitution, .. } = load_user_constitution() else {
        let text = match locale {
            Locale::ZhHans => "没有可供审阅的结构化用户全局宪章。".to_string(),
            _ => "No structured user-global constitution is available to review.".to_string(),
        };
        return (text, None);
    };
    let digest = constitution.cache_projection().digest;
    let suggested: Vec<_> = constitution.suggested_clauses().collect();
    let accepted = constitution.accepted_clauses().count();

    let mut out = String::new();
    match locale {
        Locale::ZhHans => {
            let _ = writeln!(out, "已生效条款：{accepted}");
            let _ = writeln!(out, "当前基线摘要：{digest}");
            out.push('\n');
            if suggested.is_empty() {
                out.push_str("没有待批准的建议条款。模型建议在你明确批准之前不会生效。\n");
            } else {
                out.push_str("待批准的建议条款（未注入模型，未计入缓存）：\n");
                for clause in &suggested {
                    let _ = writeln!(out, "- [{}] {}", clause.id, clause.text);
                }
                out.push_str("\n用 /constitution ratify <id> 明确批准。基线变化后需要重新审阅。\n");
            }
        }
        _ => {
            let _ = writeln!(out, "Ratified clauses in force: {accepted}");
            let _ = writeln!(out, "Current base digest: {digest}");
            out.push('\n');
            if suggested.is_empty() {
                out.push_str(
                    "No suggested clauses are awaiting review. Model advice never applies itself.\n",
                );
            } else {
                out.push_str(
                    "Suggested clauses awaiting ratification (not injected, not in the cache digest):\n",
                );
                for clause in &suggested {
                    let _ = writeln!(out, "- [{}] {}", clause.id, clause.text);
                }
                out.push_str(
                    "\nRatify explicitly with /constitution ratify <id>. If the base changes first, ratification is refused and you review again.\n",
                );
            }
        }
    }
    (out, Some(digest))
}

/// Explicit ratification (#3930). Fails closed: it hands the digest the user
/// reviewed in `/constitution suggestions` to [`UserConstitution::ratify`],
/// which refuses when the live file has moved since, and persists only when the
/// clause set is exactly what the user named.
fn ratify_text(locale: Locale, reviewed: Option<&str>, ids: &[String]) -> (String, Option<String>) {
    if ids.is_empty() {
        let text = match locale {
            Locale::ZhHans => {
                "用法：/constitution ratify <id> [<id>...]。先用 /constitution suggestions 查看待批准条款。"
                    .to_string()
            }
            _ => "Usage: /constitution ratify <id> [<id>...]. Run /constitution suggestions first to see what is pending.".to_string(),
        };
        return (text, None);
    }

    let (path, constitution) = match load_user_constitution() {
        UserConstitutionStatus::Loaded { path, constitution } => (path, constitution),
        _ => {
            let text = match locale {
                Locale::ZhHans => "没有可批准的结构化用户全局宪章。".to_string(),
                _ => "There is no structured user-global constitution to ratify.".to_string(),
            };
            return (text, None);
        }
    };

    let Some(reviewed) = reviewed else {
        let text = match locale {
            Locale::ZhHans => {
                "未批准任何条款：请先用 /constitution suggestions 审阅待批准条款。".to_string()
            }
            _ => "Nothing was ratified: review the pending clauses with /constitution suggestions first.".to_string(),
        };
        return (text, None);
    };
    match constitution.ratify(reviewed, ids, None) {
        Err(err) => (
            match locale {
                Locale::ZhHans => format!("未批准任何条款：{err}"),
                _ => format!("Nothing was ratified: {err}"),
            },
            None,
        ),
        Ok(ratification) => match ratification.constitution.save_to(&path) {
            Err(err) => (
                match locale {
                    Locale::ZhHans => format!("批准已计算但保存失败，文件未更改：{err:#}"),
                    _ => format!(
                        "Ratification was computed but could not be saved; the file is unchanged: {err:#}"
                    ),
                },
                None,
            ),
            Ok(()) => (
                match locale {
                    Locale::ZhHans => format!(
                        "已批准 {} 条：{}\n摘要 {} → {}\n这些条款从下一轮起作为会话记录中的消息发送给模型；提示词缓存前缀不变。",
                        ratification.accepted_clause_ids.len(),
                        ratification.accepted_clause_ids.join(", "),
                        ratification.before_digest,
                        ratification.after_digest,
                    ),
                    _ => format!(
                        "Ratified {} clause(s): {}\nDigest {} → {}\nThey reach the model from the next turn as a message recorded in the session; the prompt-cache prefix is unchanged.",
                        ratification.accepted_clause_ids.len(),
                        ratification.accepted_clause_ids.join(", "),
                        ratification.before_digest,
                        ratification.after_digest,
                    ),
                },
                Some(ratification.after_digest.clone()),
            ),
        },
    }
}

/// Explicit schema migration with a receipt (#4782). A rejection writes nothing.
fn migrate_text(locale: Locale) -> String {
    let path = match codewhale_config::UserConstitution::path() {
        Ok(path) => path,
        Err(err) => return path_error_preview_text(locale, &err.to_string()),
    };
    match codewhale_config::UserConstitution::migrate_file(&path) {
        Err(err) => match locale {
            Locale::ZhHans => format!("迁移失败，文件未更改：{err:#}"),
            _ => format!("Migration failed; the file is unchanged: {err:#}"),
        },
        Ok(codewhale_config::MigrationOutcome::AlreadyCurrent {
            preserved_unknown_keys,
            ..
        }) => {
            let preserved = format_preserved(&preserved_unknown_keys, locale);
            match locale {
                Locale::ZhHans => {
                    format!("已是当前架构版本，未重写文件。\n保留的未知字段：{preserved}")
                }
                _ => format!(
                    "Already at the current schema; nothing was rewritten.\nPreserved unknown fields: {preserved}"
                ),
            }
        }
        Ok(codewhale_config::MigrationOutcome::Migrated { receipt, .. }) => {
            let preserved = format_preserved(&receipt.preserved_unknown_keys, locale);
            let backup = receipt
                .backup_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            match locale {
                Locale::ZhHans => format!(
                    "已从架构 v{} 迁移到 v{}。\n备份：{backup}\n保留的未知字段：{preserved}\n模型可见字节摘要：{} → {}（{}）\n用 /constitution rollback 撤销。",
                    receipt.from_version,
                    receipt.to_version,
                    receipt.before_digest,
                    receipt.after_digest,
                    if receipt.is_cache_stable() {
                        "未改变，模型下一轮收到相同内容"
                    } else {
                        "已改变，下一轮作为新的会话消息发送"
                    },
                ),
                _ => format!(
                    "Migrated schema v{} → v{}.\nBackup: {backup}\nPreserved unknown fields: {preserved}\nModel-facing digest: {} → {} ({}).\nUndo with /constitution rollback.",
                    receipt.from_version,
                    receipt.to_version,
                    receipt.before_digest,
                    receipt.after_digest,
                    if receipt.is_cache_stable() {
                        "unchanged, so the model receives the same text"
                    } else {
                        "changed; the new text is sent as a session message at the next turn"
                    },
                ),
            }
        }
        Ok(codewhale_config::MigrationOutcome::Rejected(rejection)) => match locale {
            Locale::ZhHans => format!(
                "未迁移，文件保持原样。\n{}\n请手动修正后重试，或用 /constitution bundled 改用内置准则。",
                rejection.receipt()
            ),
            _ => format!(
                "Not migrated; the file is left exactly as it was.\n{}\nFix it by hand and retry, or run /constitution bundled to fall back to bundled law.",
                rejection.receipt()
            ),
        },
    }
}

fn rollback_text(locale: Locale) -> String {
    let path = match codewhale_config::UserConstitution::path() {
        Ok(path) => path,
        Err(err) => return path_error_preview_text(locale, &err.to_string()),
    };
    match codewhale_config::UserConstitution::rollback_file(&path) {
        Ok(backup) => match locale {
            Locale::ZhHans => format!("已从 {} 恢复迁移前的内容。备份已消耗。", backup.display()),
            _ => format!(
                "Restored the pre-migration constitution from {}. The backup is consumed.",
                backup.display()
            ),
        },
        Err(err) => match locale {
            Locale::ZhHans => format!("未回滚：{err:#}"),
            _ => format!("Nothing was rolled back: {err:#}"),
        },
    }
}

fn format_preserved(keys: &[String], locale: Locale) -> String {
    if keys.is_empty() {
        match locale {
            Locale::ZhHans => "无".to_string(),
            _ => "none".to_string(),
        }
    } else {
        keys.join(", ")
    }
}

fn suggestions_title(locale: Locale) -> &'static str {
    match locale {
        Locale::ZhHans => "待批准的建议条款",
        _ => "Suggested Clauses",
    }
}

fn open_pager(app: &mut App, title: &str, text: &str) {
    let width = app
        .viewport
        .last_transcript_area
        .map(|area| area.width)
        .unwrap_or(80);
    app.view_stack
        .push(PagerView::from_text(title, text, width.saturating_sub(2)));
}

fn format_status(app: &App, locale: Locale, account: &Result<bool, String>) -> String {
    let state = load_setup_state();
    let load = load_user_constitution();
    let (constitution_source_path, source_path, has_instructions, warnings) =
        match app.instruction_assembly.as_deref() {
            Some(assembly) => (
                assembly.constitution_source_path.clone(),
                assembly.source_path.clone(),
                assembly.global_tokens + assembly.project_tokens > 0,
                assembly.warnings.clone(),
            ),
            None => {
                let context =
                    crate::project_context::load_project_context_with_parents(&app.workspace);
                (
                    context.constitution_source_path,
                    context.source_path,
                    context.instructions.is_some(),
                    context.warnings,
                )
            }
        };
    let mut out = String::new();

    let copy = ConstitutionManagerCopy::for_locale(locale);

    let _ = writeln!(out, "{}", copy.manager_header);
    out.push_str(match locale {
        Locale::ZhHans => "\n下方为本机宪章设置。已登录账户的宪章（包括默认值）优先于本机设置，由引擎在每轮开始时加载。在应用的设置 → 账户 → 宪章中编辑；更改从下一轮生效。\n",
        _ => "\nLocal constitution settings follow. The signed-in profile constitution, including its defaults, takes precedence and is loaded by the Engine at each turn. Edit it in Settings → Accounts → Constitution; changes apply from the next turn.\n",
    });
    out.push('\n');
    let _ = writeln!(out, "{}", copy.active_stack_header);
    let _ = writeln!(out, "- {}", copy.bundled_active);
    let account_status = match account {
        Ok(true) => codewhale_localization::tr(locale, MessageId::CmdConstitutionAccountApplies)
            .into_owned(),
        Ok(false) => codewhale_localization::tr(locale, MessageId::CmdConstitutionAccountSignedOut)
            .into_owned(),
        Err(error) => {
            codewhale_localization::tr(locale, MessageId::CmdConstitutionAccountCheckFailed)
                .replace("{error}", error)
        }
    };
    let _ = writeln!(out, "- {account_status}");
    let _ = writeln!(
        out,
        "- {}: {}",
        copy.user_global_label,
        user_constitution_stack_status(state.as_ref(), &load, locale, matches!(account, Ok(true)))
    );
    if let Some(path) = constitution_source_path.as_ref() {
        let _ = writeln!(
            out,
            "- {}: {} ({})",
            copy.repo_local_label,
            copy.present,
            path.display()
        );
    } else {
        let _ = writeln!(out, "- {}: {}", copy.repo_local_label, copy.not_present);
    }
    if let Some(path) = source_path.as_ref() {
        let _ = writeln!(
            out,
            "- {}: {} ({})",
            copy.agents_label,
            copy.present,
            path.display()
        );
    } else if has_instructions {
        let _ = writeln!(out, "- {}: {}", copy.agents_label, copy.generated_fallback);
    } else {
        let _ = writeln!(out, "- {}: {}", copy.agents_label, copy.not_present);
    }
    let whale_warnings = ignored_whale_warnings(&warnings);
    if whale_warnings.is_empty() {
        let _ = writeln!(out, "- {}: {}", copy.legacy_whale_label, copy.not_present);
    } else {
        let _ = writeln!(
            out,
            "- {}: {} ({} {})",
            copy.legacy_whale_label,
            copy.whale_ignored,
            whale_warnings.len(),
            copy.location_count_unit(whale_warnings.len())
        );
        for warning in whale_warnings {
            let _ = writeln!(out, "  - {warning}");
        }
    }
    let handoff_path = app.workspace.join(crate::prompts::HANDOFF_RELATIVE_PATH);
    let _ = writeln!(
        out,
        "- {}: {} {}, {} {}",
        copy.memory_handoff_label,
        copy.memory_label,
        if app.use_memory {
            copy.enabled
        } else {
            copy.disabled
        },
        copy.handoff_label,
        if handoff_path.exists() {
            copy.present
        } else {
            copy.not_present
        }
    );

    out.push('\n');
    let _ = writeln!(out, "{}", copy.user_global_header);
    let _ = writeln!(
        out,
        "- {}: {}",
        copy.choice_label,
        state.as_ref().map_or(copy.not_recorded, |s| choice_label(
            s.constitution_choice,
            locale
        ))
    );
    let _ = writeln!(
        out,
        "- {}: {}",
        copy.source_label,
        state.as_ref().map_or(copy.not_recorded, |s| source_label(
            s.constitution_source,
            locale
        ))
    );
    let _ = writeln!(
        out,
        "- {}: {}",
        copy.file_label,
        user_constitution_file_label(&load, locale)
    );
    let _ = writeln!(
        out,
        "- {}: {}",
        copy.validity_label,
        manager_validity_label(&load, state.as_ref(), locale)
    );
    let _ = writeln!(
        out,
        "- {}: {}",
        copy.language_label,
        constitution_language(state.as_ref(), &load, locale)
    );
    let _ = writeln!(
        out,
        "- {}: {}",
        copy.last_preview_label,
        preview_record_label(state.as_ref(), locale)
    );
    let _ = writeln!(
        out,
        "- {}: {}",
        copy.runtime_posture_label,
        state.as_ref().map_or(copy.not_reviewed, |s| posture_label(
            s.runtime_posture_source,
            locale
        ))
    );
    let _ = writeln!(
        out,
        "- {}: {}",
        copy.checkpoint_label,
        state.as_ref().map_or(copy.not_completed.to_string(), |s| {
            s.constitution_checkpoint_completed_for
                .as_ref()
                .map_or_else(|| copy.not_completed.to_string(), |v| copy.completed_for(v))
        })
    );

    out.push('\n');
    let _ = writeln!(out, "{}", copy.preview_header);
    let _ = writeln!(out, "- {}", copy.preview_action);
    let _ = writeln!(out, "- {}", copy.repo_action);

    out.push('\n');
    let _ = writeln!(out, "{}", copy.maintenance_header);
    for action in copy.maintenance_actions {
        let _ = writeln!(out, "- {action}");
    }
    out
}

fn preview_text(locale: Locale) -> String {
    let state = load_setup_state();
    let load = load_user_constitution();
    match load {
        UserConstitutionStatus::Loaded { constitution, .. } => {
            let active = user_constitution_is_active(state.as_ref());
            let mut text = String::new();
            if !active {
                text.push_str(inactive_preview_text(locale));
                text.push_str("\n\n");
            }
            text.push_str(
                &constitution
                    .render_block(None)
                    .unwrap_or_else(|| structured_empty_text(locale).to_string()),
            );
            text
        }
        UserConstitutionStatus::Missing { path } => missing_preview_text(locale, &path),
        UserConstitutionStatus::Empty { path } => empty_preview_text(locale, &path),
        UserConstitutionStatus::Invalid { path, error } => {
            invalid_preview_text(locale, &path, &error)
        }
        UserConstitutionStatus::Unreadable { path, error } => {
            unreadable_preview_text(locale, &path, &error)
        }
        UserConstitutionStatus::PathError { error } => path_error_preview_text(locale, &error),
    }
}

fn load_setup_state() -> Option<SetupState> {
    SetupState::load().ok().flatten()
}

#[derive(Debug)]
enum UserConstitutionStatus {
    Missing {
        path: PathBuf,
    },
    Empty {
        path: PathBuf,
    },
    Invalid {
        path: PathBuf,
        error: String,
    },
    Unreadable {
        path: PathBuf,
        error: String,
    },
    Loaded {
        path: PathBuf,
        constitution: Box<codewhale_config::UserConstitution>,
    },
    PathError {
        error: String,
    },
}

impl UserConstitutionStatus {
    fn validity(&self) -> ConstitutionValidity {
        match self {
            Self::Missing { .. } | Self::PathError { .. } => ConstitutionValidity::Unknown,
            Self::Empty { .. } => ConstitutionValidity::Empty,
            Self::Invalid { .. } => ConstitutionValidity::Invalid,
            Self::Unreadable { .. } => ConstitutionValidity::Unreadable,
            Self::Loaded { constitution, .. } => constitution.validity(),
        }
    }

    fn validity_for_display(&self, state: Option<&SetupState>) -> ConstitutionValidity {
        match self {
            Self::Missing { .. } | Self::PathError { .. } => {
                state.map_or(ConstitutionValidity::Unknown, |s| s.constitution_validity)
            }
            _ => self.validity(),
        }
    }
}

fn load_user_constitution() -> UserConstitutionStatus {
    let path = match UserConstitution::path() {
        Ok(path) => path,
        Err(error) => {
            return UserConstitutionStatus::PathError {
                error: error.to_string(),
            };
        }
    };

    match UserConstitution::load_from(&path) {
        UserConstitutionLoad::Missing => UserConstitutionStatus::Missing { path },
        UserConstitutionLoad::Empty => UserConstitutionStatus::Empty { path },
        UserConstitutionLoad::Invalid(error) => UserConstitutionStatus::Invalid { path, error },
        UserConstitutionLoad::Unreadable(error) => {
            UserConstitutionStatus::Unreadable { path, error }
        }
        UserConstitutionLoad::Loaded(constitution) => {
            UserConstitutionStatus::Loaded { path, constitution }
        }
    }
}

fn user_constitution_stack_status(
    state: Option<&SetupState>,
    load: &UserConstitutionStatus,
    locale: Locale,
    account_applies: bool,
) -> String {
    if account_applies
        && user_constitution_is_active(state)
        && matches!(load, UserConstitutionStatus::Loaded { .. })
    {
        return codewhale_localization::tr(locale, MessageId::CmdConstitutionLocalStandby)
            .into_owned();
    }
    let text = match load {
        UserConstitutionStatus::Loaded { .. } if user_constitution_is_active(state) => match locale
        {
            Locale::ZhHans => "结构化用户全局准则已生效",
            _ => "active structured user-global law",
        },
        UserConstitutionStatus::Loaded { .. } => match locale {
            Locale::ZhHans => "有效但未生效（已选择内置/默认或专家覆盖）",
            _ => "valid but inactive (bundled/default or expert override selected)",
        },
        UserConstitutionStatus::Missing { .. } => match locale {
            Locale::ZhHans => "未配置；使用内置/默认准则",
            _ => "not configured; bundled/default applies",
        },
        UserConstitutionStatus::Empty { .. } => match locale {
            Locale::ZhHans => "为空；建议修复",
            _ => "empty; repair recommended",
        },
        UserConstitutionStatus::Invalid { .. } => match locale {
            Locale::ZhHans => "无效；建议修复",
            _ => "invalid; repair recommended",
        },
        UserConstitutionStatus::Unreadable { .. } => match locale {
            Locale::ZhHans => "无法读取；建议修复",
            _ => "unreadable; repair recommended",
        },
        UserConstitutionStatus::PathError { .. } => match locale {
            Locale::ZhHans => "不可用；CODEWHALE_HOME 错误",
            _ => "unavailable; CODEWHALE_HOME error",
        },
    };
    text.to_string()
}

fn user_constitution_is_active(state: Option<&SetupState>) -> bool {
    state.is_none_or(|state| state.constitution_choice.applies_user_global())
}

fn user_constitution_file_label(load: &UserConstitutionStatus, locale: Locale) -> String {
    match load {
        UserConstitutionStatus::Missing { path }
        | UserConstitutionStatus::Empty { path }
        | UserConstitutionStatus::Invalid { path, .. }
        | UserConstitutionStatus::Unreadable { path, .. }
        | UserConstitutionStatus::Loaded { path, .. } => path.display().to_string(),
        UserConstitutionStatus::PathError { .. } => match locale {
            Locale::ZhHans => "无法解析",
            _ => "unresolved",
        }
        .to_string(),
    }
}

fn constitution_language(
    state: Option<&SetupState>,
    load: &UserConstitutionStatus,
    locale: Locale,
) -> String {
    if let UserConstitutionStatus::Loaded { constitution, .. } = load
        && let Some(language) = constitution.language.as_deref()
    {
        return language.to_string();
    }
    state
        .and_then(|s| s.constitution_language.as_deref())
        .unwrap_or(match locale {
            Locale::ZhHans => "未记录",
            _ => "not recorded",
        })
        .to_string()
}

fn preview_record_label(state: Option<&SetupState>, locale: Locale) -> String {
    let Some(state) = state else {
        return match locale {
            Locale::ZhHans => "未记录",
            _ => "not recorded",
        }
        .to_string();
    };
    match state.constitution_preview_hash.as_deref() {
        Some(hash) => format!("v{} ({hash})", state.constitution_preview_version),
        None => match locale {
            Locale::ZhHans => "未记录",
            _ => "not recorded",
        }
        .to_string(),
    }
}

fn choice_label(choice: ConstitutionChoice, locale: Locale) -> &'static str {
    match (locale, choice) {
        (Locale::ZhHans, ConstitutionChoice::Unset) => "未设置",
        (Locale::ZhHans, ConstitutionChoice::Bundled) => "内置/默认",
        (Locale::ZhHans, ConstitutionChoice::GuidedCustom) => "引导式自定义",
        (Locale::ZhHans, ConstitutionChoice::ExpertOverride) => "专家覆盖",
        (Locale::ZhHans, ConstitutionChoice::Deferred) => "已暂缓；使用内置",
        (_, ConstitutionChoice::Unset) => "not set",
        (_, ConstitutionChoice::Bundled) => "bundled/default",
        (_, ConstitutionChoice::GuidedCustom) => "guided custom",
        (_, ConstitutionChoice::ExpertOverride) => "expert override",
        (_, ConstitutionChoice::Deferred) => "deferred; bundled applies",
    }
}

fn source_label(source: ConstitutionSource, locale: Locale) -> &'static str {
    match (locale, source) {
        (Locale::ZhHans, ConstitutionSource::Bundled) => "内置",
        (Locale::ZhHans, ConstitutionSource::UserGlobal) => "用户全局 constitution.json",
        (Locale::ZhHans, ConstitutionSource::ExpertOverride) => "专家提示词覆盖",
        (_, ConstitutionSource::Bundled) => "bundled",
        (_, ConstitutionSource::UserGlobal) => "user-global constitution.json",
        (_, ConstitutionSource::ExpertOverride) => "expert prompt override",
    }
}

fn validity_label(validity: ConstitutionValidity, locale: Locale) -> &'static str {
    match (locale, validity) {
        (Locale::ZhHans, ConstitutionValidity::Unknown) => "未知或非自定义",
        (Locale::ZhHans, ConstitutionValidity::Valid) => "有效",
        (Locale::ZhHans, ConstitutionValidity::Invalid) => "无效",
        (Locale::ZhHans, ConstitutionValidity::Empty) => "为空",
        (Locale::ZhHans, ConstitutionValidity::Unreadable) => "无法读取",
        (_, ConstitutionValidity::Unknown) => "unknown or not custom",
        (_, ConstitutionValidity::Valid) => "valid",
        (_, ConstitutionValidity::Invalid) => "invalid",
        (_, ConstitutionValidity::Empty) => "empty",
        (_, ConstitutionValidity::Unreadable) => "unreadable",
    }
}

fn manager_validity_label(
    load: &UserConstitutionStatus,
    state: Option<&SetupState>,
    locale: Locale,
) -> String {
    if matches!(load, UserConstitutionStatus::Missing { .. })
        && state.is_some_and(|state| state.constitution_choice == ConstitutionChoice::Bundled)
    {
        return match locale {
            Locale::ZhHans => "不适用（已选择内置/默认；没有自定义文件）".to_string(),
            _ => "not applicable (bundled/default selected; no custom file)".to_string(),
        };
    }
    validity_label(load.validity_for_display(state), locale).to_string()
}

fn posture_label(source: RuntimePostureSource, locale: Locale) -> &'static str {
    match (locale, source) {
        (Locale::ZhHans, RuntimePostureSource::Unset) => "未查看",
        (Locale::ZhHans, RuntimePostureSource::Inherited) => "继承自现有配置",
        (Locale::ZhHans, RuntimePostureSource::Confirmed) => "已在设置中确认",
        (_, RuntimePostureSource::Unset) => "not reviewed",
        (_, RuntimePostureSource::Inherited) => "inherited from existing config",
        (_, RuntimePostureSource::Confirmed) => "confirmed in setup",
    }
}

fn ignored_whale_warnings(warnings: &[String]) -> Vec<&str> {
    warnings
        .iter()
        .map(String::as_str)
        .filter(|warning| warning.contains("WHALE.md is ignored"))
        .collect()
}

fn help_text(locale: Locale) -> String {
    match locale {
        Locale::ZhHans => "\
用法：/constitution [status|preview|base|suggestions|ratify <id>|migrate|rollback|bundled|edit|review|repair|repo|explain|posture|help]

常用命令：
- /constitution：打开宪章管理器和当前层级。
- /constitution preview：显示会注入模型的确切用户全局宪章块；缺失、空、无效或不可读时显示修复说明。
- /constitution edit：打开 /setup 的引导式宪章步骤；用 1-6 调整，按 G 预览，再按 G 保存。
- /constitution repair：说明当前文件状态，然后打开同一个引导式修复步骤。
- /constitution bundled：记录使用内置/默认准则，不创建自定义文件。
- /constitution repo：查看 .codewhale/constitution.json 仓库本地准则。
- /constitution explain：解释内置基础准则、用户全局宪章、仓库宪章、AGENTS.md、记忆和交接的区别。
- /constitution base：显示下一轮实际组装的确切基础提示词，附来源、摘要和字节/词元度量；只读，不发送请求，也不展开工具目录。
- /constitution suggestions：查看待批准的建议条款；它们不会注入模型，也不影响提示词缓存。
- /constitution ratify <id>：明确批准某条建议条款；若基线在审阅后发生变化则拒绝批准。
- /constitution migrate：把宪章文件迁移到当前架构版本并给出回执；被拒绝时文件保持原样。
- /constitution rollback：恢复迁移前的备份。
- /constitution posture：打开运行时姿态；宪章只提供模型指导，不会更改批准、沙盒、Shell、网络、信任或 MCP 权限。",
        _ => "\
Usage: /constitution [status|preview|base|suggestions|ratify <id>|migrate|rollback|bundled|edit|review|repair|repo|explain|posture|help]

Common commands:
- /constitution: open the constitution manager and active stack.
- /constitution preview: show the exact user-global constitution block that would be injected; missing, empty, invalid, or unreadable files show repair guidance instead.
- /constitution edit: open the guided /setup Constitution step; tune 1-6, press G to preview, then G again to save.
- /constitution repair: explain the current file state, then open the same guided repair step.
- /constitution bundled: record bundled/default law without creating a custom file.
- /constitution repo: inspect repo-local .codewhale/constitution.json law.
- /constitution explain: compare Constitution, user-global law, repo law, AGENTS.md, memory, and handoff.
- /constitution base: show the exact effective base prompt assembled for the next turn, with per-block provenance, digests, and byte/token measures. Read-only: no provider request, no tool-catalog expansion.
- /constitution suggestions: review suggested clauses awaiting ratification; they are not injected and do not move the prompt-cache digest.
- /constitution ratify <id>: explicitly ratify a suggested clause. Refused if the base changed after you reviewed it.
- /constitution migrate: migrate the constitution file to the current schema with a receipt; a rejected file is left exactly as it was.
- /constitution rollback: restore the pre-migration backup.
- /constitution posture: open runtime posture; constitution text is model guidance only and does not change approvals, sandbox, shell, network, trust, or MCP permissions.",
    }
    .to_string()
}

fn repair_text(locale: Locale) -> String {
    let state = load_setup_state();
    let load = load_user_constitution();
    let file = user_constitution_file_label(&load, locale);
    let choice = state.as_ref().map_or(
        match locale {
            Locale::ZhHans => "未记录",
            _ => "not recorded",
        },
        |s| choice_label(s.constitution_choice, locale),
    );
    let validity = validity_label(load.validity_for_display(state.as_ref()), locale);
    let status = user_constitution_stack_status(state.as_ref(), &load, locale, false);

    match locale {
        Locale::ZhHans => format!(
            "\
用户全局宪章修复

当前文件：{file}
当前状态：{status}
记录选择：{choice}
有效性：{validity}

接下来将打开 /setup 的宪章步骤。安全修复路径：
- 用 1-6 调整引导式草稿，按 G 预览，再按 G 保存新的结构化 constitution.json。
- 按 U 或运行 /constitution bundled 记录使用内置/默认准则；现有无效/空/不可读文件不会被注入。
- 用 /constitution preview 查看当前错误或渲染结果。

这只处理用户全局 constitution.json。运行时批准、沙盒、Shell、网络、信任、默认模式和 MCP 权限仍由运行时姿态/配置控制。"
        ),
        _ => format!(
            "\
User-global constitution repair

Current file: {file}
Current state: {status}
Recorded choice: {choice}
Validity: {validity}

Opening the /setup Constitution step next. Safe repair paths:
- Tune the guided draft with 1-6, press G to preview, then G again to save a fresh structured constitution.json.
- Press U or run /constitution bundled to record bundled/default law; the existing invalid/empty/unreadable file will not be injected.
- Use /constitution preview to inspect the current error or rendered result.

This only repairs the user-global constitution.json. Runtime approval, sandbox, shell, network, trust, default mode, and MCP authority still belong to runtime posture/config."
        ),
    }
}

fn manager_title(locale: Locale) -> &'static str {
    match locale {
        Locale::ZhHans => "宪章",
        _ => "Constitution",
    }
}

fn review_title(locale: Locale) -> &'static str {
    match locale {
        Locale::ZhHans => "宪章检查",
        _ => "Constitution Review",
    }
}

fn rendered_title(locale: Locale) -> &'static str {
    match locale {
        Locale::ZhHans => "渲染后的用户宪章",
        _ => "Rendered User Constitution",
    }
}

fn repo_title(locale: Locale) -> &'static str {
    match locale {
        Locale::ZhHans => "仓库本地宪章",
        _ => "Repo-Local Constitution",
    }
}

fn explanation_title(locale: Locale) -> &'static str {
    match locale {
        Locale::ZhHans => "AGENTS.md 与宪章",
        _ => "AGENTS.md vs Constitution",
    }
}

fn no_repo_law_text(locale: Locale) -> &'static str {
    match locale {
        Locale::ZhHans => "此工作区未找到仓库本地宪章 .codewhale/constitution.json。",
        _ => "No repo-local constitution found at .codewhale/constitution.json for this workspace.",
    }
}

fn inactive_preview_text(locale: Locale) -> &'static str {
    match locale {
        Locale::ZhHans => "非活动预览：当前选择了内置/默认或专家覆盖。",
        _ => "Inactive preview: bundled/default or expert override is selected.",
    }
}

fn structured_empty_text(locale: Locale) -> &'static str {
    match locale {
        Locale::ZhHans => "结构化宪章为空。",
        _ => "The structured constitution is empty.",
    }
}

fn missing_preview_text(locale: Locale, path: &std::path::Path) -> String {
    match locale {
        Locale::ZhHans => format!(
            "未在 {} 找到结构化用户全局宪章。\n\n当前使用内置准则。使用 /constitution edit 创建引导式长期偏好，或使用 /constitution bundled 明确记录内置/默认。",
            path.display()
        ),
        _ => format!(
            "No structured user-global constitution found at {}.\n\nBundled law applies. Use /constitution edit to create guided standing preferences, or /constitution bundled to record bundled/default explicitly.",
            path.display()
        ),
    }
}

fn empty_preview_text(locale: Locale, path: &std::path::Path) -> String {
    match locale {
        Locale::ZhHans => format!(
            "{} 的结构化用户全局宪章为空。使用 /constitution repair 返回引导式宪章步骤。",
            path.display()
        ),
        _ => format!(
            "The structured user-global constitution at {} is empty. Use /constitution repair to return to the guided constitution step.",
            path.display()
        ),
    }
}

fn invalid_preview_text(locale: Locale, path: &std::path::Path, error: &str) -> String {
    match locale {
        Locale::ZhHans => format!(
            "{} 的结构化用户全局宪章无效，且不会注入。\n\n{error}\n\n使用 /constitution repair 返回引导式宪章步骤。",
            path.display()
        ),
        _ => format!(
            "The structured user-global constitution at {} is invalid and is not injected.\n\n{error}\n\nUse /constitution repair to return to the guided constitution step.",
            path.display()
        ),
    }
}

fn unreadable_preview_text(locale: Locale, path: &std::path::Path, error: &str) -> String {
    match locale {
        Locale::ZhHans => format!(
            "无法读取 {} 的结构化用户全局宪章，且不会注入。\n\n{error}\n\n使用 /constitution repair 返回引导式宪章步骤。",
            path.display()
        ),
        _ => format!(
            "The structured user-global constitution at {} could not be read and is not injected.\n\n{error}\n\nUse /constitution repair to return to the guided constitution step.",
            path.display()
        ),
    }
}

fn path_error_preview_text(locale: Locale, error: &str) -> String {
    match locale {
        Locale::ZhHans => format!("无法为用户全局宪章解析 CODEWHALE_HOME：\n\n{error}"),
        _ => {
            format!("Could not resolve CODEWHALE_HOME for the user-global constitution:\n\n{error}")
        }
    }
}

fn agents_explanation(locale: Locale) -> &'static str {
    match locale {
        Locale::ZhHans => {
            "\
AGENTS.md 与宪章

内置基础准则是精简的全局判断约定：身份、事实、验证、克制和优先级顺序。

用户全局宪章是个人长期偏好。它是结构化数据，确定性渲染，并低于当前用户请求和内置基础准则。

.codewhale/constitution.json 是仓库本地宪章。它属于某个工作区，并作为独立的仓库宪章块渲染。

AGENTS.md 和项目说明是项目规则/实现指导。它们可以描述构建命令、仓库规范和本地流程；按优先级顺序，它们低于当前用户请求和内置基础准则，高于用户全局长期偏好、记忆和交接。

WHALE.md 已忽略。将普通项目说明迁移到 AGENTS.md，将 Codewhale 专属权限策略迁移到 .codewhale/constitution.json。

运行时姿态是独立设置。宪章可以建议主动性，但不会改变批准策略、沙盒、Shell、网络、信任、MCP 权限或默认模式。使用 /constitution posture 查看这些控制。"
        }
        _ => {
            "\
AGENTS.md vs constitution

The bundled Constitution is the compact global judgment contract: identity, ground truth, verification, restraint, and precedence.

The user-global constitution is personal standing preference law. It is structured, rendered deterministically, and subordinate to the current user request and the bundled Constitution.

.codewhale/constitution.json is repo-local law. It belongs to a workspace and is rendered as a separate repo constitution block.

AGENTS.md and project instructions are project law / implementation guidance. They can describe build commands, repository norms, and local workflows. Under “Whose word wins,” they sit below the current user request and bundled Constitution, and above user-global standing preferences, memory, and handoff.

WHALE.md is ignored. Move ordinary project instructions to AGENTS.md and Codewhale-specific authority policy to .codewhale/constitution.json.

Runtime posture is separate. A constitution can recommend autonomy, but it does not change approval policy, sandbox, shell, network, trust, MCP permissions, or default mode. Use /constitution posture to review those controls."
        }
    }
}

struct ConstitutionManagerCopy {
    manager_header: &'static str,
    active_stack_header: &'static str,
    bundled_active: &'static str,
    user_global_label: &'static str,
    repo_local_label: &'static str,
    agents_label: &'static str,
    legacy_whale_label: &'static str,
    memory_handoff_label: &'static str,
    memory_label: &'static str,
    handoff_label: &'static str,
    user_global_header: &'static str,
    choice_label: &'static str,
    source_label: &'static str,
    file_label: &'static str,
    validity_label: &'static str,
    language_label: &'static str,
    last_preview_label: &'static str,
    runtime_posture_label: &'static str,
    checkpoint_label: &'static str,
    preview_header: &'static str,
    preview_action: &'static str,
    repo_action: &'static str,
    maintenance_header: &'static str,
    maintenance_actions: &'static [&'static str],
    present: &'static str,
    not_present: &'static str,
    generated_fallback: &'static str,
    whale_ignored: &'static str,
    enabled: &'static str,
    disabled: &'static str,
    not_recorded: &'static str,
    not_reviewed: &'static str,
    not_completed: &'static str,
}

impl ConstitutionManagerCopy {
    fn for_locale(locale: Locale) -> Self {
        match locale {
            Locale::ZhHans => Self {
                manager_header: "宪章管理器",
                active_stack_header: "生效层级",
                bundled_active: "内置基础准则：始终生效",
                user_global_label: "用户全局宪章",
                repo_local_label: "仓库本地宪章",
                agents_label: "AGENTS/项目说明",
                legacy_whale_label: "旧版 WHALE.md",
                memory_handoff_label: "记忆/交接",
                memory_label: "记忆",
                handoff_label: "交接",
                user_global_header: "用户全局宪章",
                choice_label: "选择",
                source_label: "来源",
                file_label: "文件",
                validity_label: "有效性",
                language_label: "语言",
                last_preview_label: "上次接受的预览",
                runtime_posture_label: "运行时姿态",
                checkpoint_label: "准则检查点代次",
                preview_header: "预览",
                preview_action: "/constitution preview 会在存在时打开精确渲染的用户全局块。",
                repo_action: "/constitution repo 会在存在时显示 .codewhale/constitution.json 本地准则。",
                maintenance_header: "维护",
                maintenance_actions: &[
                    "编辑引导式宪章：/constitution edit",
                    "预览渲染后的宪章：/constitution preview",
                    "查看下一轮的确切基础提示词：/constitution base",
                    "审阅待批准的建议条款：/constitution suggestions",
                    "批准某条建议条款：/constitution ratify <id>",
                    "迁移到当前架构版本：/constitution migrate",
                    "使用内置/默认：/constitution bundled",
                    "查看现有内容：/constitution review",
                    "修复无效/空/不可读文件：/constitution repair",
                    "显示仓库本地准则：/constitution repo",
                    "解释 AGENTS.md 与宪章：/constitution explain",
                    "打开运行时姿态：/constitution posture",
                ],
                present: "存在",
                not_present: "不存在",
                generated_fallback: "生成的后备内容",
                whale_ignored: "已忽略；需要迁移",
                enabled: "启用",
                disabled: "停用",
                not_recorded: "未记录",
                not_reviewed: "未查看",
                not_completed: "未完成",
            },
            _ => Self {
                manager_header: "Constitution Manager",
                active_stack_header: "Active stack",
                bundled_active: "Bundled Constitution: active base law (always on)",
                user_global_label: "User-global constitution",
                repo_local_label: "Repo-local constitution",
                agents_label: "AGENTS/project instructions",
                legacy_whale_label: "Legacy WHALE.md",
                memory_handoff_label: "Memory/handoff",
                memory_label: "memory",
                handoff_label: "handoff",
                user_global_header: "User-global constitution",
                choice_label: "Choice",
                source_label: "Source",
                file_label: "File",
                validity_label: "Validity",
                language_label: "Language",
                last_preview_label: "Last accepted preview",
                runtime_posture_label: "Runtime posture",
                checkpoint_label: "Constitution checkpoint generation",
                preview_header: "Preview",
                preview_action: "/constitution preview opens the exact rendered user-global block when present.",
                repo_action: "/constitution repo shows .codewhale/constitution.json local law when present.",
                maintenance_header: "Maintenance",
                maintenance_actions: &[
                    "Edit guided constitution: /constitution edit",
                    "Preview rendered constitution: /constitution preview",
                    "Show the next turn's exact base prompt: /constitution base",
                    "Review suggested clauses: /constitution suggestions",
                    "Ratify a suggested clause: /constitution ratify <id>",
                    "Migrate to the current schema: /constitution migrate",
                    "Use bundled/default: /constitution bundled",
                    "Review existing: /constitution review",
                    "Repair invalid/empty/unreadable: /constitution repair",
                    "Show repo-local law: /constitution repo",
                    "Explain AGENTS.md vs constitution: /constitution explain",
                    "Open runtime posture: /constitution posture",
                ],
                present: "present",
                not_present: "not present",
                generated_fallback: "generated fallback",
                whale_ignored: "ignored; migration needed",
                enabled: "enabled",
                disabled: "disabled",
                not_recorded: "not recorded",
                not_reviewed: "not reviewed",
                not_completed: "not completed",
            },
        }
    }

    fn location_count_unit(&self, count: usize) -> &'static str {
        if self.manager_header == "宪章管理器" {
            "处"
        } else if count == 1 {
            "location"
        } else {
            "locations"
        }
    }

    fn completed_for(&self, version: &str) -> String {
        if self.manager_header == "宪章管理器" {
            format!("当前（自 {version} 引入）")
        } else {
            format!("current (introduced in {version})")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::TuiOptions;
    use crate::tui::pager::PagerView;
    use crate::tui::views::ModalKind;
    use codewhale_runtime::config::Config;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn test_app() -> App {
        test_app_with_workspace(PathBuf::from("."))
    }

    fn test_app_with_workspace(workspace: PathBuf) -> App {
        let options = TuiOptions {
            ..crate::tui::test_support::test_tui_options(workspace)
        };
        App::new(options, &Config::default())
    }

    fn pop_pager_body(app: &mut App) -> String {
        let mut view = app.view_stack.pop().expect("pager view");
        let pager = view
            .as_any_mut()
            .downcast_mut::<PagerView>()
            .expect("top view should be pager");
        pager.body_text()
    }

    #[test]
    fn constitution_default_opens_manager_pager() {
        let mut app = test_app();
        app.ui_locale = Locale::En;

        let result = ConstitutionCmd::execute(&mut app, None);
        assert_eq!(
            result.action,
            Some(AppAction::OpenConstitutionManager { review: false })
        );
        assert!(result.message.is_none());
        open_manager(&mut app, false, Ok(false));
        assert_eq!(app.view_stack.top_kind(), Some(ModalKind::Pager));
        assert!(pop_pager_body(&mut app).contains("Constitution Manager"));
    }

    #[test]
    fn constitution_manager_marks_whale_md_ignored() {
        let tmp = tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("WHALE.md"), "legacy instructions").expect("write whale");
        let mut app = test_app_with_workspace(tmp.path().to_path_buf());
        app.ui_locale = Locale::En;

        open_manager(&mut app, false, Ok(false));
        let body = pop_pager_body(&mut app);
        assert!(body.contains("Legacy WHALE.md: ignored"));
        assert!(body.contains("WHALE.md is ignored"));
        assert!(!body.contains("legacy instructions"));
    }

    #[test]
    fn constitution_bundled_emits_action() {
        let mut app = test_app();

        let result = ConstitutionCmd::execute(&mut app, Some("bundled"));

        assert_eq!(result.action, Some(AppAction::UseBundledConstitution));
    }

    #[test]
    fn constitution_edit_opens_setup_at_constitution() {
        let mut app = test_app();

        let result = ConstitutionCmd::execute(&mut app, Some("edit"));

        assert_eq!(
            result.action,
            Some(AppAction::OpenSetupWizardAt {
                step: SetupStep::Constitution
            })
        );
    }

    #[test]
    fn constitution_help_lists_repair_and_runtime_boundary() {
        let mut app = test_app();
        app.ui_locale = Locale::En;

        let result = ConstitutionCmd::execute(&mut app, Some("help"));

        let message = result.message.expect("help message");
        assert!(message.contains("Usage: /constitution"));
        assert!(message.contains("/constitution repair"));
        assert!(message.contains("/constitution posture"));
        assert!(message.contains("model guidance only"));
        assert!(message.contains("does not change approvals"));
    }

    #[test]
    fn constitution_repair_explains_invalid_file_and_opens_setup() {
        let _env_guard = crate::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        let home = tmp.path().join("codewhale-home");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::write(home.join("constitution.json"), "{not valid json").expect("invalid file");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.as_os_str());
        let mut app = test_app();
        app.ui_locale = Locale::En;

        let result = ConstitutionCmd::execute(&mut app, Some("repair"));

        assert_eq!(
            result.action,
            Some(AppAction::OpenSetupWizardAt {
                step: SetupStep::Constitution
            })
        );
        let message = result.message.expect("repair message");
        assert!(message.contains("User-global constitution repair"));
        assert!(message.contains("Current state: invalid; repair recommended"));
        assert!(message.contains("Validity: invalid"));
        assert!(message.contains("constitution.json"));
        assert!(message.contains("will not be injected"));
        assert!(message.contains("Runtime approval"));
    }

    #[test]
    fn constitution_preview_renders_structured_block() {
        let _env_guard = crate::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        let home = tmp.path().join("codewhale-home");
        std::fs::create_dir_all(&home).expect("home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.as_os_str());
        let constitution = UserConstitution {
            about: Some("Maintains release lanes.".to_string()),
            ..UserConstitution::default()
        };
        constitution.save().expect("save constitution");
        let mut app = test_app();

        let result = ConstitutionCmd::execute(&mut app, Some("preview"));

        assert!(result.message.is_none());
        let body = pop_pager_body(&mut app);
        assert!(body.contains("<codewhale_user_constitution"));
        assert!(body.contains("Maintains release lanes."));
    }

    /// Seal `CODEWHALE_HOME` to a temp dir and write a constitution there.
    fn sealed_home(
        tmp: &tempfile::TempDir,
        constitution: &UserConstitution,
    ) -> crate::test_support::EnvVarGuard {
        let home = tmp.path().join("codewhale-home");
        std::fs::create_dir_all(&home).expect("home");
        let guard = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.as_os_str());
        constitution
            .save_to(&home.join("constitution.json"))
            .expect("save constitution");
        guard
    }

    fn with_suggestion() -> UserConstitution {
        UserConstitution {
            about: Some("Maintains release lanes.".to_string()),
            ..UserConstitution::default()
        }
        .with_recommendation(&codewhale_config::ConstitutionRecommendation {
            clauses: vec![codewhale_config::ConstitutionClause::suggested(
                "c1",
                "Always run the focused suite before claiming green.",
            )],
            rationale: Vec::new(),
        })
    }

    #[test]
    fn constitution_base_previews_the_exact_next_turn_prompt() {
        let mut app = test_app();

        let result = ConstitutionCmd::execute(&mut app, Some("base"));

        // Routed as an action so the preview is assembled by the same function
        // the dispatch path uses, with the session config in hand.
        assert_eq!(result.action, Some(AppAction::PreviewEffectiveBasePrompt));
        assert!(result.message.is_none());
    }

    #[test]
    fn suggestions_are_listed_but_never_previewed_as_law() {
        let _env_guard = crate::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        let _home = sealed_home(&tmp, &with_suggestion());
        let mut app = test_app();
        app.ui_locale = Locale::En;

        ConstitutionCmd::execute(&mut app, Some("suggestions"));
        let suggestions = pop_pager_body(&mut app);
        assert!(suggestions.contains("[c1]"));
        assert!(suggestions.contains("focused suite"));
        assert!(suggestions.contains("Ratified clauses in force: 0"));

        // The same clause must be absent from the injected preview.
        ConstitutionCmd::execute(&mut app, Some("preview"));
        let preview = pop_pager_body(&mut app);
        assert!(preview.contains("<codewhale_user_constitution"));
        assert!(
            !preview.contains("focused suite"),
            "unratified advice reached the model-facing block: {preview}"
        );
    }

    #[test]
    fn ratify_requires_an_id_and_then_makes_the_clause_law() {
        let _env_guard = crate::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        let _home = sealed_home(&tmp, &with_suggestion());
        let mut app = test_app();
        app.ui_locale = Locale::En;

        let bare = ConstitutionCmd::execute(&mut app, Some("ratify"))
            .message
            .expect("usage message");
        assert!(bare.contains("Usage: /constitution ratify <id>"));

        let unreviewed = ConstitutionCmd::execute(&mut app, Some("ratify c1"))
            .message
            .expect("refusal message");
        assert!(unreviewed.contains("Nothing was ratified"), "{unreviewed}");

        ConstitutionCmd::execute(&mut app, Some("suggestions"));
        pop_pager_body(&mut app);

        let unknown = ConstitutionCmd::execute(&mut app, Some("ratify nope"))
            .message
            .expect("refusal message");
        assert!(unknown.contains("Nothing was ratified"));

        let ok = ConstitutionCmd::execute(&mut app, Some("ratify c1"))
            .message
            .expect("ratified message");
        assert!(ok.contains("Ratified 1 clause(s): c1"), "{ok}");
        assert!(ok.contains("Digest"), "{ok}");

        // Now — and only now — it appears in the injected block.
        ConstitutionCmd::execute(&mut app, Some("preview"));
        assert!(pop_pager_body(&mut app).contains("focused suite"));
    }

    #[test]
    fn migrate_reports_a_receipt_and_rollback_restores() {
        let _env_guard = crate::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        let home = tmp.path().join("codewhale-home");
        std::fs::create_dir_all(&home).expect("home");
        let _home_guard = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.as_os_str());
        let path = home.join("constitution.json");
        let v1 = r#"{"schema_version":1,"about":"Legacy user."}"#;
        std::fs::write(&path, v1).expect("write v1");
        let mut app = test_app();
        app.ui_locale = Locale::En;

        let message = ConstitutionCmd::execute(&mut app, Some("migrate"))
            .message
            .expect("migrate receipt");
        assert!(message.contains("Migrated schema v1"), "{message}");
        assert!(
            message.contains("Preserved unknown fields: none"),
            "{message}"
        );
        assert!(
            message.contains("the model receives the same text"),
            "a v1 file carries no clauses, so no model-facing byte moved: {message}"
        );

        let rolled = ConstitutionCmd::execute(&mut app, Some("rollback"))
            .message
            .expect("rollback receipt");
        assert!(rolled.contains("Restored the pre-migration constitution"));
        assert_eq!(std::fs::read_to_string(&path).expect("read back"), v1);
    }

    #[test]
    fn migrate_rejects_a_runtime_policy_key_and_changes_nothing() {
        let _env_guard = crate::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        let home = tmp.path().join("codewhale-home");
        std::fs::create_dir_all(&home).expect("home");
        let _home_guard = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.as_os_str());
        let path = home.join("constitution.json");
        let raw = r#"{"about":"x","approval_policy":"bypass"}"#;
        std::fs::write(&path, raw).expect("write file");
        let mut app = test_app();
        app.ui_locale = Locale::En;

        let message = ConstitutionCmd::execute(&mut app, Some("migrate"))
            .message
            .expect("rejection receipt");
        assert!(message.contains("Not migrated"), "{message}");
        assert!(message.contains("approval_policy"), "{message}");
        assert_eq!(std::fs::read_to_string(&path).expect("read back"), raw);
    }

    #[test]
    fn help_and_manager_name_the_new_inspection_surfaces() {
        let mut app = test_app();
        app.ui_locale = Locale::En;

        let help = ConstitutionCmd::execute(&mut app, Some("help"))
            .message
            .expect("help message");
        assert!(help.contains("/constitution base"));
        assert!(help.contains("no provider request"));
        assert!(help.contains("/constitution ratify <id>"));
        assert!(help.contains("Refused if the base changed"));
        assert!(help.contains("/constitution migrate"));

        open_manager(&mut app, false, Ok(false));
        let body = pop_pager_body(&mut app);
        assert!(body.contains("/constitution base"));
        assert!(body.contains("/constitution suggestions"));
    }

    #[test]
    fn constitution_manager_uses_zh_hans_copy() {
        let _env_guard = crate::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        let home = tmp.path().join("codewhale-home");
        std::fs::create_dir_all(&home).expect("home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.as_os_str());
        let mut app = test_app();
        app.ui_locale = codewhale_localization::Locale::ZhHans;

        open_manager(&mut app, false, Ok(false));
        let body = pop_pager_body(&mut app);
        assert!(body.contains("宪章管理器"));
        assert!(body.contains("生效层级"));
        assert!(body.contains("用户全局宪章"));
        assert!(body.contains("/constitution preview 会"));
        assert!(!body.contains("宪法"));
        assert!(!body.contains("Constitution Manager"));
    }

    #[test]
    fn constitution_preview_missing_uses_zh_hans_copy() {
        let _env_guard = crate::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        let home = tmp.path().join("codewhale-home");
        std::fs::create_dir_all(&home).expect("home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.as_os_str());
        let mut app = test_app();
        app.ui_locale = codewhale_localization::Locale::ZhHans;

        let result = ConstitutionCmd::execute(&mut app, Some("preview"));

        assert!(result.message.is_none());
        let body = pop_pager_body(&mut app);
        assert!(body.contains("未在"));
        assert!(body.contains("当前使用内置准则"));
        assert!(body.contains("/constitution edit"));
        assert!(!body.contains("宪法"));
        assert!(!body.contains("No structured user-global constitution"));
    }

    #[test]
    fn constitution_explanation_uses_zh_hans_copy() {
        let mut app = test_app();
        app.ui_locale = codewhale_localization::Locale::ZhHans;

        let result = ConstitutionCmd::execute(&mut app, Some("explain"));

        assert!(result.message.is_none());
        let body = pop_pager_body(&mut app);
        assert!(body.contains("AGENTS.md 与宪章"));
        assert!(body.contains(".codewhale/constitution.json"));
        assert!(body.contains("运行时姿态是独立设置"));
        assert!(!body.contains("宪法"));
        assert!(!body.contains("项目法律"));
        assert!(!body.contains("Runtime posture is separate"));
    }
}
