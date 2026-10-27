//! Per-scope instruction budget, selection, and notice behaviour (#6526).
//!
//! One test per branch the design specifies: the `AGENTS.md`/`CLAUDE.md`
//! truth table, skip-as-absent, symlink dedupe, per-scope trims, rules
//! charging, configured entries, and the `refuse` policy.

use super::*;
use tempfile::tempdir;

fn write(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(path, body).expect("write");
}

fn with_instruction_settings<T>(
    budget: InstructionBudgetConfig,
    run: impl FnOnce(&InstructionSettings) -> T,
) -> T {
    run(&InstructionSettings {
        budget,
        ..Default::default()
    })
}

fn budgets(global: usize, project: usize) -> InstructionBudgets {
    InstructionBudgets {
        global_tokens: global,
        project_tokens: project,
        static_guard_tokens: Some(900_000),
        file_max_bytes: 8 * 1024 * 1024,
        policy: InstructionBudgetPolicy::Trim,
        claude_fallback: false,
        usable_window_tokens: Some(1_000_000),
    }
}

fn load_isolated(workspace: &Path, home: &Path) -> ProjectContext {
    load_project_context_with_parents_and_home(workspace, Some(home))
}

fn assemble(
    workspace: &Path,
    home: &Path,
    configured: &[crate::prompts::InstructionSource],
    budgets: &InstructionBudgets,
) -> (ProjectContext, InstructionAssembly) {
    let mut ctx = load_isolated(workspace, home);
    let assembly = assemble_instruction_block(&mut ctx, configured, budgets);
    (ctx, assembly)
}

fn claude_imports() -> ForeignInstructionImports {
    ForeignInstructionImports::from_config(&["claude".to_string()]).0
}

// ── Selection ──────────────────────────────────────────────────────────────

#[test]
fn agents_md_wins_over_claude_md() {
    let ws = tempdir().unwrap();
    let home = tempdir().unwrap();
    write(&ws.path().join("AGENTS.md"), "AGENTS_WINS");
    write(&ws.path().join("CLAUDE.md"), "CLAUDE_LOSES");

    let ctx = load_isolated(ws.path(), home.path());
    let text = ctx.instructions.unwrap_or_default();
    assert!(text.contains("AGENTS_WINS"), "{text}");
    assert!(
        !text.contains("CLAUDE_LOSES"),
        "AGENTS.md wins; CLAUDE.md is not co-read without an import:\n{text}"
    );
}

#[test]
fn claude_md_requires_opt_in() {
    let ws = tempdir().unwrap();
    let home = tempdir().unwrap();
    write(&ws.path().join("CLAUDE.md"), "CLAUDE_FALLBACK_TEXT");

    let ctx = load_isolated(ws.path(), home.path());
    assert!(
        !ctx.instructions
            .unwrap_or_default()
            .contains("CLAUDE_FALLBACK_TEXT")
    );
}

#[test]
fn claude_fallback_can_be_disabled() {
    let config = InstructionBudgetConfig {
        claude_fallback: Some(false),
        ..Default::default()
    };
    with_instruction_settings(config, |settings| {
        let ws = tempdir().unwrap();
        let home = tempdir().unwrap();
        write(&ws.path().join("CLAUDE.md"), "CLAUDE_OPTED_OUT");

        let ctx = load_project_context_with_settings(
            ws.path(),
            &InstructionSettings {
                home: Some(home.path().to_path_buf()),
                ..settings.clone()
            },
        );
        assert!(
            !ctx.instructions
                .unwrap_or_default()
                .contains("CLAUDE_OPTED_OUT"),
            "claude_fallback = false is an explicit opt-out"
        );
    });
}

#[test]
fn claude_import_reads_when_agents_absent_and_fallback_disabled() {
    let config = InstructionBudgetConfig {
        claude_fallback: Some(false),
        ..Default::default()
    };
    with_instruction_settings(config, |settings| {
        let ws = tempdir().unwrap();
        let home = tempdir().unwrap();
        write(&ws.path().join("CLAUDE.md"), "CLAUDE_EXPLICIT_IMPORT");

        // The import set is explicit here: the process-wide default is
        // "import nothing", and the fallback flag is off, so only the explicit
        // `claude` import can read the file.
        let mut ctx = load_project_context_with_settings(
            ws.path(),
            &InstructionSettings {
                home: Some(home.path().to_path_buf()),
                imports: claude_imports(),
                ..settings.clone()
            },
        );
        let assembly = assemble_instruction_block(&mut ctx, &[], &budgets(1_000, 1_000));
        assert!(
            assembly
                .block
                .unwrap_or_default()
                .contains("CLAUDE_EXPLICIT_IMPORT")
        );
    });

    // The explicit import outranks the fallback flag: load through the import
    // set the loader reads.
    let ws = tempdir().unwrap();
    write(&ws.path().join("CLAUDE.md"), "CLAUDE_IMPORTED");
    let ctx = load_project_context_with_imports(ws.path(), &claude_imports());
    assert!(
        ctx.instructions
            .unwrap_or_default()
            .contains("CLAUDE_IMPORTED")
    );
}

#[test]
fn explicit_claude_import_reads_both() {
    let ws = tempdir().unwrap();
    write(&ws.path().join("AGENTS.md"), "AGENTS_PRIMARY");
    write(&ws.path().join("CLAUDE.md"), "CLAUDE_CO_READ");

    let ctx = load_project_context_with_imports(ws.path(), &claude_imports());
    let text = ctx.instructions.unwrap_or_default();
    assert!(text.contains("AGENTS_PRIMARY"), "{text}");
    assert!(
        text.contains("CLAUDE_CO_READ"),
        "an explicit claude import co-reads CLAUDE.md next to AGENTS.md:\n{text}"
    );
    let agents_at = text.find("AGENTS_PRIMARY").unwrap();
    let claude_at = text.find("CLAUDE_CO_READ").unwrap();
    assert!(agents_at < claude_at, "AGENTS.md reads first");
}

#[test]
fn agents_md_skipped_falls_back_to_claude() {
    let config = InstructionBudgetConfig {
        file_max_bytes: Some(16),
        claude_fallback: Some(true),
        ..Default::default()
    };
    with_instruction_settings(config, |settings| {
        let ws = tempdir().unwrap();
        let home = tempdir().unwrap();
        write(&ws.path().join("AGENTS.md"), "A".repeat(200).as_str());
        // Comfortably under the 16-byte ceiling above, or the fallback would
        // be skipped for the same reason as `AGENTS.md`.
        write(&ws.path().join("CLAUDE.md"), "CLAUDE_OK");

        let ctx = load_project_context_with_settings(
            ws.path(),
            &InstructionSettings {
                home: Some(home.path().to_path_buf()),
                ..settings.clone()
            },
        );
        let text = ctx.instructions.unwrap_or_default();
        assert!(
            text.contains("CLAUDE_OK"),
            "a skipped AGENTS.md counts as absent:\n{text}"
        );
        let skipped = ctx
            .notices
            .iter()
            .find(|notice| notice.limit == InstructionLimit::FileCeiling)
            .expect("the skip is reported");
        assert_eq!(skipped.action, InstructionAction::Skipped);
        assert!(
            skipped
                .fallback_selected
                .as_deref()
                .is_some_and(|label| label.contains("CLAUDE.md")),
            "the notice names the file that replaced it: {skipped:?}"
        );
    });
}

#[test]
fn global_claude_is_last_resort_after_instructions_md() {
    let ws = tempdir().unwrap();
    let home = tempdir().unwrap();
    write(
        &home.path().join(".codewhale/instructions.md"),
        "GLOBAL_INSTRUCTIONS_WIN",
    );
    write(&home.path().join(".claude/CLAUDE.md"), "GLOBAL_CLAUDE_LAST");

    let ctx = load_isolated(ws.path(), home.path());
    let text = ctx.instructions.unwrap_or_default();
    assert!(text.contains("GLOBAL_INSTRUCTIONS_WIN"), "{text}");
    assert!(
        !text.contains("GLOBAL_CLAUDE_LAST"),
        "a foreign file never outranks codewhale's own:\n{text}"
    );
}

#[test]
fn global_claude_is_read_when_nothing_else_exists() {
    let config = InstructionBudgetConfig {
        claude_fallback: Some(true),
        ..Default::default()
    };
    with_instruction_settings(config, |settings| {
        let ws = tempdir().unwrap();
        let home = tempdir().unwrap();
        write(&home.path().join(".claude/CLAUDE.md"), "GLOBAL_CLAUDE_ONLY");

        let ctx = load_project_context_with_settings(
            ws.path(),
            &InstructionSettings {
                home: Some(home.path().to_path_buf()),
                ..settings.clone()
            },
        );
        assert!(
            ctx.instructions
                .unwrap_or_default()
                .contains("GLOBAL_CLAUDE_ONLY")
        );
    });
}

#[cfg(unix)]
#[test]
fn symlinked_duplicate_is_loaded_once() {
    let ws = tempdir().unwrap();
    let home = tempdir().unwrap();
    let global = home.path().join(".codewhale/AGENTS.md");
    write(&global, "SHARED_SYMLINK_BODY");
    std::os::unix::fs::symlink(&global, ws.path().join("AGENTS.md")).expect("symlink");

    let ctx = load_isolated(ws.path(), home.path());
    let text = ctx.instructions.unwrap_or_default();
    assert_eq!(
        text.matches("SHARED_SYMLINK_BODY").count(),
        1,
        "one physical file loads once:\n{text}"
    );
    assert!(
        ctx.notices.iter().any(|notice| {
            notice.path.as_deref()
                == Some(
                    ws.path()
                        .canonicalize()
                        .unwrap()
                        .join("AGENTS.md")
                        .as_path(),
                )
                && notice.action == InstructionAction::Skipped
        }),
        "the duplicate is reported, not silently dropped: {:?}",
        ctx.notices
    );
}

// ── Per-scope budgets ──────────────────────────────────────────────────────

#[test]
fn no_trim_within_scope_budget() {
    let ws = tempdir().unwrap();
    let home = tempdir().unwrap();
    write(&ws.path().join("AGENTS.md"), "SMALL PROJECT FILE");

    let (ctx, assembly) = assemble(ws.path(), home.path(), &[], &budgets(10_000, 10_000));
    assert!(
        !assembly.notices.iter().any(|notice| notice.is_loss()),
        "nothing is lost while a source is within its budget: {:?}",
        assembly.notices
    );
    assert_eq!(
        ctx.instructions.as_deref(),
        Some("SMALL PROJECT FILE"),
        "content is unmodified"
    );
}

#[test]
fn per_scope_budgets_are_independent() {
    let ws = tempdir().unwrap();
    let home = tempdir().unwrap();
    write(
        &home.path().join(".codewhale/AGENTS.md"),
        format!("GLOBAL {}", "g".repeat(4_000)).as_str(),
    );
    write(&ws.path().join("AGENTS.md"), "PROJECT INTACT");

    let (_ctx, assembly) = assemble(ws.path(), home.path(), &[], &budgets(50, 10_000));
    let global_loss = assembly
        .notices
        .iter()
        .find(|notice| notice.is_loss())
        .expect("the global scope is over its own budget");
    assert_eq!(global_loss.scope, InstructionScope::Global);
    assert!(
        !assembly
            .notices
            .iter()
            .any(|notice| notice.is_loss() && notice.scope == InstructionScope::Project),
        "a huge global file does not shrink the project scope: {:?}",
        assembly.notices
    );
    assert!(
        assembly
            .block
            .unwrap_or_default()
            .contains("PROJECT INTACT")
    );
}

#[test]
fn trim_keeps_the_nearest_scope() {
    let ws = tempdir().unwrap();
    let home = tempdir().unwrap();
    fs::create_dir_all(ws.path().join(".git")).unwrap();
    write(&ws.path().join(".git/HEAD"), "ref: refs/heads/main\n");
    write(
        &ws.path().join("AGENTS.md"),
        format!("ROOT-MARKER {}", "R".repeat(20_000)).as_str(),
    );
    let nested = ws.path().join("crates").join("tui");
    write(&nested.join("AGENTS.md"), "NEAREST-MARKER stays");

    let (_ctx, assembly) = assemble(&nested, home.path(), &[], &budgets(10_000, 200));
    let block = assembly.block.unwrap_or_default();
    assert!(block.contains("NEAREST-MARKER stays"), "{block}");
    assert!(
        !block.contains("ROOT-MARKER"),
        "the broadest segment is dropped first:\n{block}"
    );
}

#[test]
fn single_oversized_file_keeps_the_head() {
    let ws = tempdir().unwrap();
    let home = tempdir().unwrap();
    let body = format!("HEAD-MARKER {}", "z".repeat(20_000));
    write(&ws.path().join("AGENTS.md"), &body);

    let (_ctx, assembly) = assemble(ws.path(), home.path(), &[], &budgets(10_000, 300));
    let block = assembly.block.unwrap_or_default();
    assert!(block.contains("HEAD-MARKER"), "{block}");
    let head_at = block.find("HEAD-MARKER").expect("head marker");
    assert!(
        head_at < block.find("instructions trimmed").unwrap_or(usize::MAX),
        "the head survives and the marker records the cut:\n{block}"
    );
    assert!(block.contains("instructions trimmed"), "{block}");
    assert!(
        block.contains("estimated tokens"),
        "the marker carries numbers"
    );
    assert!(assembly.global_tokens <= 300);
}

#[test]
fn zero_bytes_left_is_reported_as_skipped() {
    let ws = tempdir().unwrap();
    let home = tempdir().unwrap();
    write(&ws.path().join("AGENTS.md"), &"q".repeat(4_000));

    let (_ctx, assembly) = assemble(ws.path(), home.path(), &[], &budgets(10_000, 1));
    let notice = assembly
        .notices
        .iter()
        .find(|notice| notice.scope == InstructionScope::Project)
        .expect("a notice");
    assert_eq!(
        notice.action,
        InstructionAction::Skipped,
        "no source bytes left means skipped, not trimmed: {notice:?}"
    );
    assert_eq!(notice.limit, InstructionLimit::ScopeBudget);
}

#[test]
fn rules_share_the_project_scope_budget() {
    let ws = tempdir().unwrap();
    let home = tempdir().unwrap();
    write(&ws.path().join("AGENTS.md"), &"I".repeat(1_000));
    write(&ws.path().join(".codewhale/rules/a.md"), &"R".repeat(4_000));

    let tokens_for_instructions = estimate_instruction_tokens(&"I".repeat(1_000));
    let (_ctx, assembly) = assemble(
        ws.path(),
        home.path(),
        &[],
        &budgets(10_000, tokens_for_instructions + 10),
    );
    let rules = assembly
        .notices
        .iter()
        .find(|notice| notice.origin == InstructionOrigin::Rules)
        .expect("rules are charged after the chain");
    assert_eq!(rules.limit, InstructionLimit::RulesBudget);
}

#[test]
fn rules_block_is_head_trimmed_with_numbers() {
    let ws = tempdir().unwrap();
    let home = tempdir().unwrap();
    write(&ws.path().join("AGENTS.md"), "SHORT");
    write(&ws.path().join(".codewhale/rules/a.md"), &"R".repeat(8_000));

    let (_ctx, assembly) = assemble(ws.path(), home.path(), &[], &budgets(10_000, 400));
    let block = assembly.block.unwrap_or_default();
    assert!(block.contains("rules block trimmed"), "{block}");
    assert!(
        block.contains("estimated tokens"),
        "numbers, not just a marker"
    );
}

// ── Per-file ceiling, empties, cap, duplicates ─────────────────────────────

#[test]
fn file_over_ceiling_is_skipped_not_trimmed() {
    let config = InstructionBudgetConfig {
        file_max_bytes: Some(8),
        ..Default::default()
    };
    with_instruction_settings(config, |settings| {
        let ws = tempdir().unwrap();
        let home = tempdir().unwrap();
        write(
            &ws.path().join("AGENTS.md"),
            "THIS IS LONGER THAN EIGHT BYTES",
        );

        let ctx = load_project_context_with_settings(
            ws.path(),
            &InstructionSettings {
                home: Some(home.path().to_path_buf()),
                ..settings.clone()
            },
        );
        assert!(
            !ctx.instructions
                .unwrap_or_default()
                .contains("LONGER THAN EIGHT")
        );
        let notice = ctx
            .notices
            .iter()
            .find(|notice| notice.limit == InstructionLimit::FileCeiling)
            .expect("the ceiling is reported");
        assert_eq!(notice.action, InstructionAction::Skipped);
    });
}

#[test]
fn empty_file_is_skipped() {
    let ws = tempdir().unwrap();
    let home = tempdir().unwrap();
    write(&ws.path().join("AGENTS.md"), "   \n\t\n");
    write(&ws.path().join("CLAUDE.md"), "CLAUDE_AFTER_EMPTY");

    let ctx = load_project_context_with_settings(
        ws.path(),
        &InstructionSettings {
            home: Some(home.path().to_path_buf()),
            budget: InstructionBudgetConfig {
                claude_fallback: Some(true),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    assert!(
        ctx.notices
            .iter()
            .any(|notice| notice.limit == InstructionLimit::EmptyFile)
    );
    assert!(
        ctx.instructions
            .unwrap_or_default()
            .contains("CLAUDE_AFTER_EMPTY")
    );
}

#[test]
fn rules_cap_is_reported() {
    let ws = tempdir().unwrap();
    let home = tempdir().unwrap();
    for index in 0..MAX_RULES_FILES + 5 {
        write(
            &ws.path()
                .join(format!(".codewhale/rules/rule_{index:03}.md")),
            "rule body",
        );
    }

    let ctx = load_isolated(ws.path(), home.path());
    let capped = ctx
        .notices
        .iter()
        .filter(|notice| notice.limit == InstructionLimit::RulesCap)
        .count();
    assert_eq!(capped, 5, "every file beyond the cap is reported");
}

// ── Configured entries and the cache ───────────────────────────────────────

#[test]
fn configured_instructions_are_read_fresh() {
    let ws = tempdir().unwrap();
    let home = tempdir().unwrap();
    let extra = ws.path().join("extra.md");
    write(&extra, "VERSION_ONE");
    let configured = vec![crate::prompts::InstructionSource::File(extra.clone())];

    let (_ctx, first) = assemble(
        ws.path(),
        home.path(),
        &configured,
        &budgets(10_000, 10_000),
    );
    assert!(first.block.unwrap_or_default().contains("VERSION_ONE"));

    write(&extra, "VERSION_TWO");
    let (_ctx, second) = assemble(
        ws.path(),
        home.path(),
        &configured,
        &budgets(10_000, 10_000),
    );
    let block = second.block.unwrap_or_default();
    assert!(
        block.contains("VERSION_TWO") && !block.contains("VERSION_ONE"),
        "configured entries are read after the cache lookup:\n{block}"
    );
}

#[test]
fn inline_instructions_stay_outside_cache() {
    let ws = tempdir().unwrap();
    let home = tempdir().unwrap();
    write(&ws.path().join("AGENTS.md"), "PROJECT");
    let configured = vec![crate::prompts::InstructionSource::Inline {
        name: "embedded:test".to_string(),
        content: "INLINE_ONLY".to_string(),
    }];

    let (_ctx, assembly) = assemble(
        ws.path(),
        home.path(),
        &configured,
        &budgets(10_000, 10_000),
    );
    assert!(assembly.block.unwrap_or_default().contains("INLINE_ONLY"));

    let cached = load_isolated(ws.path(), home.path());
    assert!(
        !cached
            .instructions
            .unwrap_or_default()
            .contains("INLINE_ONLY"),
        "inline content never enters the file-keyed cache"
    );
}

#[test]
fn instructions_array_counts_toward_global() {
    let ws = tempdir().unwrap();
    let home = tempdir().unwrap();
    write(&ws.path().join("AGENTS.md"), "PROJECT_KEPT");
    let global_file = ws.path().join("global-extra.md");
    write(&global_file, &"G".repeat(4_000));
    let configured = vec![crate::prompts::InstructionSource::File(global_file)];

    let (_ctx, assembly) = assemble(ws.path(), home.path(), &configured, &budgets(60, 10_000));
    assert!(
        assembly
            .notices
            .iter()
            .any(|notice| notice.is_loss() && notice.scope == InstructionScope::Global),
        "configured entries are global authority: {:?}",
        assembly.notices
    );
    assert!(assembly.block.unwrap_or_default().contains("PROJECT_KEPT"));
    assert!(assembly.project_tokens > 0);
}

#[test]
fn cache_returns_untrimmed_segments_across_budgets() {
    let ws = tempdir().unwrap();
    let home = tempdir().unwrap();
    let body = format!("FULL_BODY {}", "c".repeat(10_000));
    write(&ws.path().join("AGENTS.md"), &body);

    let (_small, _assembly) = assemble(ws.path(), home.path(), &[], &budgets(10_000, 50));
    let cached = load_isolated(ws.path(), home.path());
    assert_eq!(
        cached.instructions.as_deref().map(str::len),
        Some(body.len()),
        "a trim must not be written back into the cache"
    );
    assert_eq!(cached.instruction_segments.len(), 1);
}

// ── Policy ────────────────────────────────────────────────────────────────

#[test]
fn refuse_policy_stops_on_trim_but_not_on_skip() {
    let trim_config = InstructionBudgetConfig {
        project_budget_tokens: Some(10),
        policy: Some(InstructionBudgetPolicy::Refuse),
        ..Default::default()
    };
    with_instruction_settings(trim_config, |settings| {
        let ws = tempdir().unwrap();
        write(&ws.path().join("AGENTS.md"), &"T".repeat(4_000));
        let outcome =
            startup_instruction_notice(ws.path(), &[], Some(128_000), Some(2_048), settings);
        let error = outcome.expect_err("refuse stops on a trim");
        assert!(error.contains("refuse"), "{error}");
    });

    let skip_config = InstructionBudgetConfig {
        file_max_bytes: Some(8),
        policy: Some(InstructionBudgetPolicy::Refuse),
        ..Default::default()
    };
    with_instruction_settings(skip_config, |settings| {
        let ws = tempdir().unwrap();
        write(&ws.path().join("AGENTS.md"), &"S".repeat(1_000));
        let notice =
            startup_instruction_notice(ws.path(), &[], Some(128_000), Some(2_048), settings)
                .expect("a skip is not a refusal")
                .expect("a skip is reported");
        assert!(notice.contains("skipped"), "{notice}");
    });
}

#[test]
fn startup_notice_names_the_loss_and_numbers() {
    with_instruction_settings(
        InstructionBudgetConfig {
            project_budget_tokens: Some(50),
            ..Default::default()
        },
        |settings| {
            let ws = tempdir().unwrap();
            write(&ws.path().join("AGENTS.md"), &"N".repeat(4_000));
            let notice =
                startup_instruction_notice(ws.path(), &[], Some(128_000), Some(2_048), settings)
                    .expect("valid config")
                    .expect("a trim is reported");
            assert!(notice.contains("trimmed"), "{notice}");
            assert!(notice.contains("estimated tokens"), "{notice}");
            assert!(notice.contains("bytes"), "{notice}");
        },
    );
}

#[test]
fn file_max_bytes_zero_is_rejected() {
    let config = InstructionBudgetConfig {
        file_max_bytes: Some(0),
        ..Default::default()
    };
    assert!(config.validate().is_err());
}
