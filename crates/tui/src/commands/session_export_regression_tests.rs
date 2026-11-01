//! FEAT-025 Phase 3: real-host regression coverage for the session-export
//! adapter.
//!
//! These tests deliberately stay outside `groups/session`, which FEAT-043
//! moves into `codewhale-commands`. They exercise the TUI-owned
//! `SessionExportAdapter` through the capability envelope and assert the
//! baseline host contracts: metadata derivation, data-minimized projections,
//! the shared turn-handoff renderer, read-only restore-point projection,
//! clipboard/recovery ordering inputs, and protected file resolution/writing.
//!
//! No test depends on a manual terminal, GUI, device, or live clipboard.

use std::path::{Path, PathBuf};

use tempfile::TempDir;

use codewhale_command_contract::facets::{
    ConversationExportProjection, ExportBlock, HistoryEntry, RestorePointProjection,
    RestoreSnapshot, TranscriptProjection, TurnHandoffProjection,
};
use codewhale_command_contract::handler::CommandCapabilities;

use crate::test_support::{EnvVarGuard, TestEnvLock};
use crate::tui::app::{App, TuiOptions};
use crate::tui::clipboard::ClipboardHandler;
use crate::tui::history::HistoryCell;
use codewhale_error_taxonomy::ErrorSeverity;
use codewhale_models::{ContentBlock, ImageUrlContent, Message, Role, ToolCaller};
use codewhale_runtime::config::Config;
use codewhale_snapshot::SnapshotRepo;

use crate::commands::session_export_test_support::{
    assert_only_export_facet_exposed, normalize_export_time, normalize_recorded_export,
    normalize_turn_generated_at,
};
use crate::commands::{CommandResult, execute};

struct ExportHarness {
    app: App,
    // Guards are declared before the lock and the temporary directory so the
    // environment is restored before the lock is released and before the
    // temporary files are removed (matches ControlHarness).
    _home: EnvVarGuard,
    _codewhale_home: EnvVarGuard,
    _ssh: [EnvVarGuard; 3],
    _env_lock: TestEnvLock,
    temp: TempDir,
}

impl ExportHarness {
    fn new() -> Self {
        let env_lock = crate::test_support::lock_test_env();
        let temp = TempDir::new().expect("tempdir");
        let home = temp.path().join("home");
        std::fs::create_dir_all(&home).expect("home dir");
        let _home = EnvVarGuard::set("HOME", &home);
        let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", &home);
        let _ssh = ["SSH_CLIENT", "SSH_CONNECTION", "SSH_TTY"].map(EnvVarGuard::remove);
        let options = TuiOptions {
            skills_dir: temp.path().join("skills"),
            memory_path: temp.path().join("memory.md"),
            notes_path: temp.path().join("notes.txt"),
            mcp_config_path: temp.path().join("mcp.json"),
            ..crate::tui::test_support::test_tui_options(temp.path())
        };
        let app = App::new(options, &Config::default());
        Self {
            app,
            _home,
            _codewhale_home,
            _ssh,
            _env_lock: env_lock,
            temp,
        }
    }
}

fn conversation_projection(app: &mut App) -> ConversationExportProjection {
    let mut bundle = app.command_contexts();
    let mut parts = bundle.parts();
    parts
        .export
        .as_mut()
        .expect("export facet")
        .conversation_projection()
}

fn turn_handoff_projection(app: &mut App) -> TurnHandoffProjection {
    let mut bundle = app.command_contexts();
    let mut parts = bundle.parts();
    parts
        .export
        .as_mut()
        .expect("export facet")
        .turn_handoff_projection()
}

fn text_message(role: Role, text: &str) -> Message {
    Message {
        role,
        content: vec![ContentBlock::Text {
            text: text.to_string(),
            cache_control: None,
        }],
    }
}

#[test]
fn adapter_projects_authoritative_metadata_and_omits_hidden_payloads() {
    let mut harness = ExportHarness::new();
    harness.app.current_session_id = Some("session-123456789".to_string());
    harness.app.api_messages = std::sync::Arc::new(vec![
        text_message(Role::System, "hidden policy must never export"),
        text_message(Role::User, "please inspect\nthe output"),
        Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Thinking {
                    thinking: "private chain of thought".to_string(),
                    signature: Some("signature-secret".to_string()),
                    state: None,
                    redacted_data: None,
                },
                ContentBlock::ToolUse {
                    execution_id: None,
                    id: "call-1".to_string(),
                    name: "fetch_url".to_string(),
                    input: serde_json::json!({"url": "https://example.com/a"}),
                    caller: Some(ToolCaller {
                        caller_type: "code_execution_20250825".to_string(),
                        tool_id: Some("server-tool-1".to_string()),
                    }),
                    thought_signature: None,
                },
                ContentBlock::ImageUrl {
                    image_url: ImageUrlContent {
                        url: "data:image/png;base64,very-secret-image-data".to_string(),
                    },
                },
                ContentBlock::ImageUrl {
                    image_url: ImageUrlContent {
                        url: "https://example.com/remote.png".to_string(),
                    },
                },
            ],
        },
    ]);

    let projection = conversation_projection(&mut harness.app);

    assert_eq!(projection.metadata.session_label, "session-");
    assert_eq!(
        projection.metadata.provider,
        harness.app.provider_identity_for_persistence()
    );
    assert_eq!(projection.metadata.model, harness.app.model_display_label());
    assert_eq!(projection.metadata.mode, harness.app.mode.display_name());
    assert_eq!(
        projection.metadata.workspace_name,
        harness
            .temp
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap()
    );
    assert_eq!(projection.metadata.message_count, 3);
    assert!(projection.metadata.exported_at_unix > 0);

    let TranscriptProjection::Authoritative(messages) = &projection.transcript else {
        panic!("authoritative transcript expected");
    };
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[1].role, "user");
    assert_eq!(
        messages[1].prompt_snippet.as_deref(),
        Some("please inspect")
    );
    assert!(matches!(
        messages[2].blocks[0],
        ExportBlock::InternalReasoning
    ));
    assert!(matches!(messages[2].blocks[2], ExportBlock::ImageOmitted));
    assert!(matches!(
        &messages[2].blocks[3],
        ExportBlock::ImageReference { url } if url == "https://example.com/remote.png"
    ));
    let ExportBlock::ToolCall { caller, .. } = &messages[2].blocks[1] else {
        panic!("tool call expected");
    };
    let caller = caller.as_ref().expect("caller projection");
    assert_eq!(caller.caller_type, "code_execution_20250825");
    assert_eq!(caller.tool_id.as_deref(), Some("server-tool-1"));

    let debug = format!("{projection:?}");
    for forbidden in [
        "private chain of thought",
        "signature-secret",
        "very-secret-image-data",
    ] {
        assert!(
            !debug.contains(forbidden),
            "hidden payload {forbidden:?} crossed the projection boundary"
        );
    }
}

#[test]
fn adapter_projects_visible_history_fallback_with_baseline_markers() {
    let mut harness = ExportHarness::new();
    harness.app.api_messages_mut().clear();
    harness.app.history = vec![
        HistoryCell::User {
            content: "user text".to_string(),
        },
        HistoryCell::Assistant {
            content: "assistant text".to_string(),
            streaming: false,
        },
        HistoryCell::System {
            content: "hidden system".to_string(),
        },
        HistoryCell::Thinking {
            content: "hidden reasoning".to_string(),
            streaming: false,
            duration_secs: None,
        },
        HistoryCell::Error {
            message: "boom".to_string(),
            severity: ErrorSeverity::Warning,
        },
    ];

    let projection = conversation_projection(&mut harness.app);
    let TranscriptProjection::HistoryFallback(entries) = &projection.transcript else {
        panic!("history fallback expected");
    };
    assert_eq!(projection.metadata.message_count, 5);
    assert_eq!(
        entries[0],
        HistoryEntry::Sanitized {
            role: "user".to_string(),
            body: "user text".to_string(),
        }
    );
    assert_eq!(
        entries[1],
        HistoryEntry::Sanitized {
            role: "assistant".to_string(),
            body: "assistant text".to_string(),
        }
    );
    assert_eq!(
        entries[2],
        HistoryEntry::Literal {
            role: "system".to_string(),
            body: "[internal context omitted]".to_string(),
        }
    );
    assert_eq!(
        entries[3],
        HistoryEntry::Literal {
            role: "internal reasoning".to_string(),
            body: "[internal reasoning omitted]".to_string(),
        }
    );
    assert_eq!(
        entries[4],
        HistoryEntry::Sanitized {
            role: "warning".to_string(),
            body: "boom".to_string(),
        }
    );
}

#[test]
fn adapter_reuses_turn_handoff_renderer_and_workspace_value() {
    let mut harness = ExportHarness::new();
    harness.app.history.push(HistoryCell::User {
        content: "Fix the flaky login test".to_string(),
    });
    harness.app.history.push(HistoryCell::Assistant {
        content: "Fixed the login test.".to_string(),
        streaming: false,
    });
    harness.app.runtime_turn_status = Some("completed".to_string());

    let direct = crate::tui::ui::turn_handoff_markdown(&harness.app);
    let projection = turn_handoff_projection(&mut harness.app);

    // The renderer stamps `generated <wall-clock second>` on every call, so two
    // calls straddling a second boundary legitimately differ in that one field
    // (seen on Windows CI: `10:35:58` vs `10:35:59`). Compare everything else
    // byte-for-byte, and check both calls still carry the stamp.
    assert!(direct.contains(" \u{b7} generated "));
    assert!(projection.markdown.contains(" \u{b7} generated "));
    assert_eq!(
        normalize_turn_generated_at(&projection.markdown),
        normalize_turn_generated_at(&direct),
        "renderer output must not drift"
    );
    assert!(projection.markdown.contains("# Turn handoff"));
    assert_eq!(
        projection.workspace_path,
        harness.app.workspace.to_string_lossy().into_owned()
    );
}

#[test]
fn adapter_projects_absent_restore_points_without_creating_a_repo() {
    let mut harness = ExportHarness::new();
    let workspace = harness.temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    harness.app.workspace = workspace.clone();
    let before = codewhale_snapshot::snapshot_git_dir(&workspace).expect("snapshot path");
    assert!(!before.exists(), "precondition: no side repo yet");

    let projection = conversation_projection(&mut harness.app);

    assert!(matches!(
        projection.restore_points,
        RestorePointProjection::None
    ));
    assert!(
        !codewhale_snapshot::snapshot_git_dir(&workspace)
            .expect("snapshot path")
            .exists(),
        "projection must never create the snapshot repo"
    );
}

#[test]
fn adapter_projects_recorded_restore_points_as_semantic_fields() {
    let mut harness = ExportHarness::new();
    let workspace = harness.temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    harness.app.workspace = workspace.clone();
    let repo = SnapshotRepo::open_or_init(&workspace).expect("open side repo");
    repo.snapshot("pre-turn:2: second prompt")
        .expect("record snapshot");

    let projection = conversation_projection(&mut harness.app);

    let RestorePointProjection::Recorded { snapshots } = &projection.restore_points else {
        panic!("recorded restore points expected");
    };
    assert_eq!(snapshots.len(), 1);
    let snapshot: &RestoreSnapshot = &snapshots[0];
    assert_eq!(snapshot.id.len(), 40, "full id crosses; portable truncates");
    assert_eq!(snapshot.kind, "pre-turn");
    assert_eq!(snapshot.sequence, Some(2));
    assert_eq!(snapshot.prompt_snippet.as_deref(), Some("second prompt"));
    assert_eq!(snapshot.label, "pre-turn:2: second prompt");
    assert!(snapshot.timestamp_unix > 0);
}

#[test]
fn adapter_projects_user_identity_from_the_role_enum_not_the_string() {
    // F6: the baseline compared `message.role != Role::User`. Projecting only
    // the rendered role string would lose that distinction, because
    // `Role::Unrecognized("user")` renders as "user" but is not `Role::User`.
    let mut harness = ExportHarness::new();
    harness.app.api_messages = std::sync::Arc::new(vec![
        Message {
            role: Role::Unrecognized("user".to_string()),
            content: vec![ContentBlock::Text {
                text: "looks like a user turn".to_string(),
                cache_control: None,
            }],
        },
        text_message(Role::User, "actually a user turn"),
    ]);

    let projection = conversation_projection(&mut harness.app);
    let TranscriptProjection::Authoritative(messages) = &projection.transcript else {
        panic!("authoritative transcript expected");
    };
    assert_eq!(
        messages[0].role, "user",
        "the rendered role string is unchanged"
    );
    assert!(
        !messages[0].is_user_role,
        "Role::Unrecognized(\"user\") must not be treated as a user turn"
    );
    assert!(messages[1].is_user_role, "Role::User is a user turn");
}

fn clipboard_facet(app: &mut App) -> bool {
    let mut bundle = app.command_contexts();
    let mut parts = bundle.parts();
    parts
        .export
        .as_mut()
        .expect("export facet")
        .clipboard_requires_terminal_paste()
}

#[test]
fn adapter_exposes_clipboard_mode_recovery_and_delivery_separately() {
    let mut harness = ExportHarness::new();

    harness.app.clipboard = ClipboardHandler::for_test(true, true);
    assert!(clipboard_facet(&mut harness.app));
    harness.app.clipboard = ClipboardHandler::for_test(false, false);
    assert!(!clipboard_facet(&mut harness.app));

    // Recovery write is one operation and returns the shared path.
    let recovery = {
        let mut bundle = harness.app.command_contexts();
        let mut parts = bundle.parts();
        parts
            .export
            .as_mut()
            .expect("export facet")
            .write_recovery_copy("recover me")
    };
    let recovery = recovery.expect("recovery path");
    assert!(recovery.ends_with("exports/last-copy.md"));
    assert_eq!(
        std::fs::read_to_string(&recovery).expect("recovery content"),
        "recover me"
    );

    // Clipboard delivery is a separate operation and records the payload.
    harness.app.clipboard = ClipboardHandler::for_test(false, false);
    {
        let mut bundle = harness.app.command_contexts();
        let mut parts = bundle.parts();
        parts
            .export
            .as_mut()
            .expect("export facet")
            .write_clipboard("deliver me")
            .expect("clipboard write");
    }
    assert_eq!(
        harness.app.clipboard.last_written_text(),
        Some("deliver me")
    );

    // A failing clipboard still returns the raw host error text.
    harness.app.clipboard = ClipboardHandler::unavailable_for_test(false);
    let failure = {
        let mut bundle = harness.app.command_contexts();
        let mut parts = bundle.parts();
        parts
            .export
            .as_mut()
            .expect("export facet")
            .write_clipboard("nope")
    };
    assert!(failure.is_err());
}

fn resolve(app: &mut App, raw: &str) -> Result<PathBuf, String> {
    let mut bundle = app.command_contexts();
    let mut parts = bundle.parts();
    parts
        .export
        .as_mut()
        .expect("export facet")
        .resolve_export_path(raw)
}

/// Create a workspace directory whose path contains no symlink component.
///
/// macOS places `TempDir` under `/var/folders/...`, and `/var` is a symlink to
/// `/private/var`. The protected export writer deliberately rejects any path
/// with a symlink component, so an adapter-level test that calls
/// `write_export_file` directly - bypassing `resolve_export_path`, which
/// canonicalizes the workspace for the real command - must hand it an already
/// resolved path. Linux temp dirs contain no symlink, so only macOS CI sees the
/// difference.
fn canonical_workspace(harness: &ExportHarness) -> PathBuf {
    let root = std::fs::canonicalize(harness.temp.path()).expect("canonical temp root");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    workspace
}

fn write_file(
    app: &mut App,
    path: &std::path::Path,
    contents: &[u8],
    force: bool,
) -> Result<(), String> {
    let mut bundle = app.command_contexts();
    let mut parts = bundle.parts();
    parts
        .export
        .as_mut()
        .expect("export facet")
        .write_export_file(path, contents, force)
}

#[test]
fn adapter_resolves_export_paths_with_baseline_errors() {
    let mut harness = ExportHarness::new();
    let workspace = harness.temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    harness.app.workspace = workspace.clone();

    assert_eq!(
        resolve(&mut harness.app, "transcript.md").expect("workspace relative"),
        std::fs::canonicalize(&workspace)
            .unwrap()
            .join("transcript.md")
    );
    assert_eq!(
        resolve(&mut harness.app, "").unwrap_err(),
        "export path is empty"
    );
    assert!(
        resolve(&mut harness.app, "../outside.md")
            .unwrap_err()
            .contains("may not contain `..`")
    );
    assert!(
        resolve(&mut harness.app, "/")
            .unwrap_err()
            .starts_with("export path must name a file:")
    );
}

#[test]
fn adapter_preserves_protected_file_write_and_overwrite_refusal() {
    let mut harness = ExportHarness::new();
    let workspace = canonical_workspace(&harness);
    harness.app.workspace = workspace.clone();
    let target = workspace.join("transcript.md");

    write_file(&mut harness.app, &target, b"first", false).expect("first write");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "first");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    let refused = write_file(&mut harness.app, &target, b"second", false).unwrap_err();
    assert!(refused.contains("destination already exists"));
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "first");

    write_file(&mut harness.app, &target, b"third", true).expect("forced write");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "third");

    let missing_parent =
        write_file(&mut harness.app, &workspace.join("nope/x.md"), b"x", false).unwrap_err();
    assert!(missing_parent.contains("parent directory"));
}

#[cfg(unix)]
#[test]
fn adapter_rejects_symlink_leaf_and_ancestor_exports() {
    use std::os::unix::fs::symlink;

    let mut harness = ExportHarness::new();
    let workspace = canonical_workspace(&harness);
    harness.app.workspace = workspace.clone();

    let real_file = workspace.join("real.md");
    std::fs::write(&real_file, "keep").expect("fixture file");
    let leaf = workspace.join("leaf.md");
    symlink(&real_file, &leaf).expect("leaf symlink");
    let leaf_result = write_file(&mut harness.app, &leaf, b"replace", true).unwrap_err();
    assert!(leaf_result.contains("symlink component"));
    assert_eq!(std::fs::read_to_string(&real_file).unwrap(), "keep");

    let real_dir = workspace.join("real-dir");
    std::fs::create_dir(&real_dir).expect("real dir");
    let linked_dir = workspace.join("linked-dir");
    symlink(&real_dir, &linked_dir).expect("dir symlink");
    let ancestor_result =
        write_file(&mut harness.app, &linked_dir.join("out.md"), b"x", false).unwrap_err();
    assert!(ancestor_result.contains("symlink component"));
    assert!(!real_dir.join("out.md").exists());
}

/// The protected writer refuses any path whose ancestors include a symlink,
/// which on macOS is true of everything under `/var` (and therefore every
/// `TempDir`) because `/var` is a symlink to `/private/var`. An adapter-level
/// test that calls `write_export_file` directly must therefore hand it a
/// resolved path - `resolve_export_path` canonicalizes the workspace for the
/// real command, and `canonical_workspace` does the same for these tests.
///
/// This test builds the symlink itself, so a Linux run catches the class of bug
/// that macOS CI caught in `adapter_preserves_protected_file_write_and_overwrite_refusal`
/// and `adapter_rejects_directory_destination_and_parent_not_a_directory`.
#[cfg(unix)]
#[test]
fn protected_writer_requires_a_path_free_of_symlink_ancestors() {
    use std::os::unix::fs::symlink;

    let harness = ExportHarness::new();
    let real = harness.temp.path().join("real-root");
    std::fs::create_dir_all(&real).expect("real root");
    let link = harness.temp.path().join("link-root");
    symlink(&real, &link).expect("root symlink");

    // Reached through the symlinked root: refused, and nothing is written.
    let refused =
        crate::commands::session_export_host::write_export_file(&link.join("out.md"), b"x", false)
            .expect_err("a symlinked ancestor must be refused");
    assert!(refused.contains("symlink component"), "{refused}");
    assert!(!real.join("out.md").exists());

    // The same file through the resolved root is accepted, which is exactly what
    // `canonical_workspace` supplies.
    let resolved = std::fs::canonicalize(&link).expect("canonical root");
    crate::commands::session_export_host::write_export_file(&resolved.join("out.md"), b"x", false)
        .expect("a resolved path must be writable");
    assert_eq!(
        std::fs::read_to_string(resolved.join("out.md")).unwrap(),
        "x"
    );
}

#[test]
fn envelope_exposes_export_only_for_the_declared_capability() {
    let mut harness = ExportHarness::new();

    {
        let mut bundle = harness.app.command_contexts();
        let export_only = bundle
            .contexts(CommandCapabilities::SESSION_EXPORT)
            .into_parts();
        // Exhaustive across all sixteen `ContextParts` slots (see the helper),
        // so a newly added facet cannot silently join the export envelope.
        assert_only_export_facet_exposed(export_only);
    }

    {
        let mut bundle = harness.app.command_contexts();
        let unrelated_only = bundle.contexts(CommandCapabilities::SESSION).into_parts();
        assert!(
            unrelated_only.export.is_none(),
            "export must not be exposed without its declared capability"
        );
    }
}

// ---------------------------------------------------------------------------
// FEAT-025 Phase 4: full-command parity regressions relocated from the legacy
// `groups/session/export.rs` tests. They dispatch through the public command
// seam so the portable handler and the TUI export adapter are exercised
// together against real clipboard, filesystem, and snapshot fixtures.
// ---------------------------------------------------------------------------

fn dispatch(app: &mut App, arg: Option<&str>) -> CommandResult {
    match arg {
        Some(arg) => execute(&format!("/export {arg}"), app),
        None => execute("/export", app),
    }
}

// ---------------------------------------------------------------------------
// FEAT-025 audit hardening: full-document goldens captured from the
// pre-refactor implementation at `3f3aa9ed7` (see
// `fixtures/export_conversation_baseline.md` and `export_turn_baseline.md`).
//
// The goldens were produced by dispatching the same fixture through the
// *baseline* public `/export` seam in a scratch worktree (repository state
// `3f3aa9ed7`), not authored from the migrated implementation, so a
// divergence in the adapter projection or the portable renderer fails here
// with an exact byte offset. Only the two host-derived stamps (the export
// `- Exported:` line and the turn-handoff header) are normalised; every other
// byte, including redaction markers and omission text, is compared verbatim.
//
// To re-capture, reproduce the fixture below against a verified baseline and
// replace the fixture files - never regenerate them from the new code, which
// would turn this into a tautology.
// ---------------------------------------------------------------------------

/// Workspace pinned by the captured goldens (a path with no snapshot repo, so
/// the baseline restore-point state is `None`).
const BASELINE_WORKSPACE: &str = "/workspace/example";

/// Session id pinned by the captured goldens.
const BASELINE_SESSION: &str = "session-123456789";

/// The exact transcript fixture the baseline goldens were captured from.
fn baseline_golden_messages() -> Vec<Message> {
    vec![
        Message {
            role: Role::System,
            content: vec![ContentBlock::Text {
                text: "hidden policy must never export".to_string(),
                cache_control: None,
            }],
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "Please inspect this\u{1b}[31m output\u{1b}[0m".to_string(),
                cache_control: None,
            }],
        },
        Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Thinking {
                    thinking: "private chain of thought".to_string(),
                    signature: Some("signature-secret".to_string()),
                    state: None,
                    redacted_data: None,
                },
                ContentBlock::ToolUse {
                    execution_id: None,
                    id: "call-1".to_string(),
                    name: "fetch_url".to_string(),
                    input: serde_json::json!({
                        "url": "https://alice:password@example.com/path?token=very-secret&ok=1",
                        "api_key": "literal-api-secret",
                        "nested": {"authorization": "Bearer abcdefghijklmnop"},
                    }),
                    caller: Some(ToolCaller {
                        caller_type: "code_execution_20250825".to_string(),
                        tool_id: Some("server-tool-1".to_string()),
                    }),
                    thought_signature: None,
                },
            ],
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                execution_id: None,
                tool_use_id: "call-1".to_string(),
                content: "Authorization: Bearer another-secret-token\nresult ok".to_string(),
                is_error: Some(false),
                content_blocks: Some(vec![
                    serde_json::json!({
                        "type": "image",
                        "mime_type": "image/png",
                        "data": "base64verysecretimagedata",
                    }),
                    serde_json::json!({"session_token": "session-secret", "note": "keep me"}),
                ]),
            }],
        },
        Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::ImageUrl {
                    image_url: ImageUrlContent {
                        url: "https://example.com/visible.png?token=very-secret".to_string(),
                    },
                },
                ContentBlock::ImageUrl {
                    image_url: ImageUrlContent {
                        url: "data:image/png;base64,very-secret-image-data".to_string(),
                    },
                },
            ],
        },
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ServerToolUse {
                id: "srv-1".to_string(),
                name: "web_search".to_string(),
                input: serde_json::json!({"query": "secret token"}),
            }],
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolSearchToolResult {
                tool_use_id: "srv-1".to_string(),
                content: serde_json::json!({"results": []}),
            }],
        },
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::CodeExecutionToolResult {
                tool_use_id: "srv-2".to_string(),
                content: serde_json::json!({"stdout": "ok"}),
            }],
        },
    ]
}

#[test]
fn command_clipboard_export_matches_baseline_conversation_golden() {
    let mut harness = ExportHarness::new();
    harness.app.workspace = PathBuf::from(BASELINE_WORKSPACE);
    harness.app.current_session_id = Some(BASELINE_SESSION.to_string());
    harness.app.clipboard = ClipboardHandler::for_test(false, false);
    harness.app.api_messages = std::sync::Arc::new(baseline_golden_messages());

    let result = dispatch(&mut harness.app, None);
    assert!(!result.is_error, "{:?}", result.message);
    let markdown = harness
        .app
        .clipboard
        .last_written_text()
        .expect("clipboard payload")
        .to_string();

    let expected = normalize_export_time(include_str!("fixtures/export_conversation_baseline.md"));
    crate::test_support::assert_byte_identical(
        "baseline conversation export document",
        &normalize_export_time(&markdown),
        &expected,
    );
}

#[test]
fn command_turn_export_matches_baseline_conversation_golden() {
    let mut harness = ExportHarness::new();
    harness.app.workspace = PathBuf::from(BASELINE_WORKSPACE);
    harness.app.clipboard = ClipboardHandler::for_test(false, false);
    harness.app.history.push(HistoryCell::User {
        content: "Fix the flaky login test".to_string(),
    });
    harness.app.history.push(HistoryCell::Assistant {
        content: "Fixed the login test.".to_string(),
        streaming: false,
    });
    harness.app.runtime_turn_status = Some("completed".to_string());

    let result = dispatch(&mut harness.app, Some("turn"));
    assert!(!result.is_error, "{:?}", result.message);
    let markdown = harness
        .app
        .clipboard
        .last_written_text()
        .expect("clipboard payload")
        .to_string();

    let expected = normalize_turn_generated_at(include_str!("fixtures/export_turn_baseline.md"));
    crate::test_support::assert_byte_identical(
        "baseline turn export document",
        &normalize_turn_generated_at(&markdown),
        &expected,
    );
}

// ---------------------------------------------------------------------------
// FEAT-025 audit re-pass (finding G1): the first golden covered only the
// `RestorePointProjection::None` state and the authoritative transcript, so the
// `Recorded` restore table, the correlation lines, and the visible-history
// fallback were still asserted by hand-written expectations - the same class of
// weakness the first audit raised.
//
// These two goldens were captured the same way from baseline `3f3aa9ed7`:
// `fixtures/export_history_fallback_recorded_baseline.md` and
// `fixtures/export_correlation_recorded_baseline.md`. Snapshot commits carry a
// wall-clock author date, so the snapshot id and the `Recorded (UTC)` cell are
// normalised; the table structure, id truncation, correlation wording, the
// ambiguity warning, and the no-match line are compared verbatim.
// ---------------------------------------------------------------------------

/// Workspace **basename** pinned by the recorded-restore goldens. The export
/// header prints only the basename, so a per-test directory with this name
/// reproduces the captured document exactly while staying inside `TempDir`.
const BASELINE_RECORDED_WORKSPACE: &str = "f025-golden-workspace";

/// Seed the three snapshots the recorded goldens were captured with.
///
/// Creation order matters: `SnapshotRepo::list` returns newest first, so this
/// yields `pre-turn:3`, `tool:call-1`, `pre-turn:2` in the table, and two
/// `pre-turn` entries sharing a prompt snippet - which is what makes the
/// correlation line ambiguous.
fn seed_baseline_restore_points(workspace: &Path) {
    let repo = SnapshotRepo::open_or_init(workspace).expect("open side repo");
    repo.snapshot("pre-turn:2: Fix the login test")
        .expect("snapshot 1");
    repo.snapshot("tool:call-1").expect("snapshot 2");
    repo.snapshot("pre-turn:3: Fix the login test")
        .expect("snapshot 3");
}

fn recorded_workspace(harness: &ExportHarness) -> PathBuf {
    let workspace = harness.temp.path().join(BASELINE_RECORDED_WORKSPACE);
    std::fs::create_dir_all(&workspace).expect("workspace");
    seed_baseline_restore_points(&workspace);
    workspace
}

#[test]
fn command_history_fallback_export_matches_baseline_recorded_golden() {
    let mut harness = ExportHarness::new();
    let workspace = recorded_workspace(&harness);
    harness.app.workspace = workspace;
    harness.app.current_session_id = Some("session-fallback".to_string());
    harness.app.clipboard = ClipboardHandler::for_test(false, false);
    harness.app.history.push(HistoryCell::User {
        content: "Fix the login test".to_string(),
    });
    harness.app.history.push(HistoryCell::Assistant {
        content: "Done.".to_string(),
        streaming: false,
    });

    let result = dispatch(&mut harness.app, None);
    assert!(!result.is_error, "{:?}", result.message);
    let markdown = harness
        .app
        .clipboard
        .last_written_text()
        .expect("clipboard payload")
        .to_string();

    let expected = normalize_recorded_export(include_str!(
        "fixtures/export_history_fallback_recorded_baseline.md"
    ));
    crate::test_support::assert_byte_identical(
        "baseline history-fallback recorded export document",
        &normalize_recorded_export(&markdown),
        &expected,
    );
}

#[test]
fn command_correlation_export_matches_baseline_recorded_golden() {
    let mut harness = ExportHarness::new();
    let workspace = recorded_workspace(&harness);
    harness.app.workspace = workspace;
    harness.app.current_session_id = Some("session-correlated".to_string());
    harness.app.clipboard = ClipboardHandler::for_test(false, false);
    harness.app.api_messages = std::sync::Arc::new(vec![
        text_message(Role::User, "Fix the login test"),
        text_message(Role::Assistant, "Working on it."),
        text_message(Role::User, "unrelated question"),
    ]);

    let result = dispatch(&mut harness.app, None);
    assert!(!result.is_error, "{:?}", result.message);
    let markdown = harness
        .app
        .clipboard
        .last_written_text()
        .expect("clipboard payload")
        .to_string();

    let expected = normalize_recorded_export(include_str!(
        "fixtures/export_correlation_recorded_baseline.md"
    ));
    crate::test_support::assert_byte_identical(
        "baseline correlated recorded export document",
        &normalize_recorded_export(&markdown),
        &expected,
    );
}

#[test]
fn export_entry_registers_through_portable_bridge_with_exact_metadata() {
    use codewhale_command_contract::handler::{CommandCapabilities, CommandHandler};

    let command = crate::commands::registry()
        .get("export")
        .expect("/export must be registered");
    assert_eq!(command.info().name, "export");
    assert_eq!(command.info().aliases, &["daochu"]);
    assert_eq!(
        command.info().usage,
        "/export [clipboard|file [--force] <path>|turn [clipboard|file [--force] <path>]]"
    );
    assert!(
        crate::commands::registry().get("daochu").is_some(),
        "the /daochu alias must resolve to /export"
    );
    let handler = command
        .contextual_handler()
        .expect("/export must register through the portable bridge");
    let CommandHandler::Contextual { capabilities, .. } = handler else {
        panic!("/export must be contextual")
    };
    assert_eq!(
        capabilities,
        CommandCapabilities::SESSION_EXPORT,
        "/export declares export authority only"
    );
}

#[test]
fn command_clipboard_export_preserves_structure_and_redacts_secrets() {
    let mut harness = ExportHarness::new();
    let app = &mut harness.app;
    app.current_session_id = Some("session-123456789".to_string());
    app.api_messages = std::sync::Arc::new(vec![
        Message {
            role: Role::System,
            content: vec![ContentBlock::Text {
                text: "hidden policy must never export".to_string(),
                cache_control: None,
            }],
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "Please inspect this\u{1b}[31m output\u{1b}[0m".to_string(),
                cache_control: None,
            }],
        },
        Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Thinking {
                    thinking: "private chain of thought".to_string(),
                    signature: Some("signature-secret".to_string()),
                    state: None,
                    redacted_data: None,
                },
                ContentBlock::ToolUse {
                    execution_id: None,
                    id: "call-1".to_string(),
                    name: "fetch_url".to_string(),
                    input: serde_json::json!({
                        "url": "https://alice:password@example.com/path?token=very-secret&ok=1",
                        "api_key": "literal-api-secret",
                        "nested": {"authorization": "Bearer abcdefghijklmnop"},
                    }),
                    caller: Some(ToolCaller {
                        caller_type: "code_execution_20250825".to_string(),
                        tool_id: Some("server-tool-1".to_string()),
                    }),
                    thought_signature: None,
                },
            ],
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                execution_id: None,
                tool_use_id: "call-1".to_string(),
                content: "Authorization: Bearer another-secret-token\nresult ok".to_string(),
                is_error: Some(false),
                content_blocks: Some(vec![serde_json::json!({
                    "image": "https://example.com/a.png?api_key=hidden",
                    "session_token": "session-secret",
                })]),
            }],
        },
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ImageUrl {
                image_url: ImageUrlContent {
                    url: "data:image/png;base64,very-secret-image-data".to_string(),
                },
            }],
        },
    ]);
    {
        let mut todos = app.todos.try_lock().expect("todos lock");
        todos.add(
            "export projection".to_string(),
            codewhale_engine::tools::todo::TodoStatus::InProgress,
        );
    }
    app.cycle_effort();
    let work_before = app.work_state_snapshot().expect("Work snapshot");

    let result = dispatch(app, None);

    assert!(!result.is_error, "{:?}", result.message);
    assert!(
        result
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("local clipboard")
    );
    let markdown = app
        .clipboard
        .last_written_text()
        .expect("clipboard payload");
    let system = markdown.find("## 1. system").expect("system role");
    let user = markdown.find("## 2. user").expect("user role");
    let assistant = markdown.find("## 3. assistant").expect("assistant role");
    let tool_result = markdown.find("## 4. user").expect("tool-result role");
    assert!(system < user && user < assistant && assistant < tool_result);
    assert!(markdown.contains("[internal context omitted]"));
    assert!(markdown.contains("call-1"));
    assert!(markdown.contains("fetch_url"));
    assert!(markdown.contains("server-tool-1"));
    assert!(markdown.contains("[internal reasoning and signature omitted]"));
    assert!(markdown.contains("[redacted]"));
    assert!(markdown.contains("https://***:***@example.com/path?token=***&ok=1"));
    assert!(markdown.contains("Reference omitted (inline or local image payload)"));
    let workspace_path = harness.temp.path().to_string_lossy().into_owned();
    for forbidden in [
        "hidden policy must never export",
        "private chain of thought",
        "signature-secret",
        "literal-api-secret",
        "very-secret",
        "another-secret-token",
        "session-secret",
        "very-secret-image-data",
        "\u{1b}[31m",
        workspace_path.as_str(),
    ] {
        assert!(
            !markdown.contains(forbidden),
            "leaked {forbidden:?}: {markdown}"
        );
    }
    assert_eq!(
        app.work_state_snapshot()
            .expect("Work snapshot after export"),
        work_before,
        "export must not mutate Work"
    );
}

#[test]
fn command_clipboard_reports_ssh_terminal_client_and_failure_honestly() {
    let mut harness = ExportHarness::new();
    let app = &mut harness.app;
    app.clipboard = ClipboardHandler::for_test(true, true);
    let ssh = dispatch(app, Some("clipboard"));
    assert!(!ssh.is_error, "{:?}", ssh.message);
    assert!(
        ssh.message
            .as_deref()
            .unwrap_or_default()
            .contains("terminal-client clipboard over SSH")
    );
    assert!(
        ssh.message
            .as_deref()
            .unwrap_or_default()
            .contains("last-copy.md"),
        "success must name the backup copy: {:?}",
        ssh.message
    );

    app.clipboard = ClipboardHandler::unavailable_for_test(false);
    let failed = dispatch(app, Some("clipboard"));
    assert!(failed.is_error);
    let message = failed.message.as_deref().unwrap_or_default();
    assert!(
        message.contains("The full export was written to"),
        "{message}"
    );
    assert!(message.contains("last-copy.md"), "{message}");
    assert!(message.contains("/export file <path>"), "{message}");
    assert!(!harness.temp.path().join("chat_export.md").exists());
    assert!(
        harness
            .temp
            .path()
            .join("home/exports/last-copy.md")
            .exists()
    );
}

#[test]
fn command_file_export_is_workspace_relative_private_and_no_overwrite_by_default() {
    let mut harness = ExportHarness::new();
    let workspace = harness.temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let app = &mut harness.app;
    app.workspace = workspace.clone();
    app.api_messages_mut().push(Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "first export".to_string(),
            cache_control: None,
        }],
    });

    let first = dispatch(app, Some("file transcript.md"));
    assert!(!first.is_error, "{:?}", first.message);
    let path = std::fs::canonicalize(&workspace)
        .unwrap()
        .join("transcript.md");
    let original = std::fs::read_to_string(&path).expect("first export");
    assert!(original.contains("first export"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    app.api_messages_mut()[0].content = vec![ContentBlock::Text {
        text: "replacement export".to_string(),
        cache_control: None,
    }];
    let refused = dispatch(app, Some("transcript.md"));
    assert!(refused.is_error);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);

    let forced = dispatch(app, Some("file --force transcript.md"));
    assert!(!forced.is_error, "{:?}", forced.message);
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("replacement export")
    );
}

#[test]
fn command_file_export_rejects_traversal_missing_parent_and_invalid_usage() {
    let mut harness = ExportHarness::new();
    let workspace = harness.temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let app = &mut harness.app;
    app.workspace = workspace.clone();

    for arg in [
        "file ../outside.md",
        "file missing/export.md",
        "file",
        "file --force",
        "clipboard extra.md",
        "turn clipboard extra.md",
    ] {
        let result = dispatch(app, Some(arg));
        assert!(result.is_error, "{arg}: {:?}", result.message);
    }
    assert!(!harness.temp.path().join("outside.md").exists());
}

#[cfg(unix)]
#[test]
fn command_file_export_rejects_symlink_leaf_and_ancestor() {
    use std::os::unix::fs::symlink;

    let mut harness = ExportHarness::new();
    let workspace = harness.temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let app = &mut harness.app;
    app.workspace = workspace.clone();

    let real_file = workspace.join("real.md");
    std::fs::write(&real_file, "keep").expect("fixture file");
    let leaf = workspace.join("leaf.md");
    symlink(&real_file, &leaf).expect("leaf symlink");
    let leaf_result = dispatch(app, Some(&format!("file --force {}", leaf.display())));
    assert!(leaf_result.is_error, "{:?}", leaf_result.message);
    assert_eq!(std::fs::read_to_string(&real_file).unwrap(), "keep");

    let real_dir = workspace.join("real-dir");
    std::fs::create_dir(&real_dir).expect("real dir");
    let linked_dir = workspace.join("linked-dir");
    symlink(&real_dir, &linked_dir).expect("dir symlink");
    let ancestor_result = dispatch(
        app,
        Some(&format!("file {}", linked_dir.join("out.md").display())),
    );
    assert!(ancestor_result.is_error, "{:?}", ancestor_result.message);
    assert!(!real_dir.join("out.md").exists());
}

#[test]
fn command_turn_export_supports_clipboard_and_safe_legacy_file_destination() {
    let mut harness = ExportHarness::new();
    let app = &mut harness.app;
    app.history.push(HistoryCell::User {
        content: "Fix the flaky login test".to_string(),
    });
    app.history.push(HistoryCell::Assistant {
        content: "Fixed the login test.".to_string(),
        streaming: false,
    });
    app.runtime_turn_status = Some("completed".to_string());

    let clipboard = dispatch(app, Some("turn"));
    assert!(!clipboard.is_error, "{:?}", clipboard.message);
    assert!(
        app.clipboard
            .last_written_text()
            .unwrap_or_default()
            .contains("# Turn handoff")
    );

    let path = harness.temp.path().join("handoff.md");
    let file = dispatch(app, Some(&format!("turn {}", path.display())));
    assert!(!file.is_error, "{:?}", file.message);
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("Fix the flaky login test")
    );
    let refused = dispatch(app, Some(&format!("turn {}", path.display())));
    assert!(refused.is_error);
}

#[test]
fn command_export_does_not_create_a_snapshot_repo_for_a_fresh_workspace() {
    let mut harness = ExportHarness::new();
    let workspace = harness.temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let app = &mut harness.app;
    app.workspace = workspace.clone();
    let before = codewhale_snapshot::snapshot_git_dir(&workspace).expect("snapshot path");
    assert!(!before.exists(), "precondition: no side repo yet");

    let result = dispatch(app, Some("clipboard"));
    assert!(!result.is_error, "{:?}", result.message);

    assert!(
        !codewhale_snapshot::snapshot_git_dir(&workspace)
            .expect("snapshot path")
            .exists(),
        "export must never create the side repo"
    );
}

#[test]
fn adapter_projects_metadata_fallbacks_without_session_or_filename() {
    let mut harness = ExportHarness::new();
    // No session id: the baseline label is `unsaved`. A workspace whose final
    // component is `..` has no `file_name()`, so the label falls back too.
    harness.app.current_session_id = None;
    harness.app.workspace = harness.temp.path().join("..");

    let projection = conversation_projection(&mut harness.app);

    assert_eq!(projection.metadata.session_label, "unsaved");
    assert_eq!(projection.metadata.workspace_name, "workspace");
    assert_eq!(projection.metadata.message_count, 0);
}

#[test]
fn adapter_bounds_restore_points_to_the_latest_hundred_newest_first() {
    let mut harness = ExportHarness::new();
    let workspace = harness.temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    harness.app.workspace = workspace.clone();
    let repo = SnapshotRepo::open_or_init(&workspace).expect("open side repo");

    // 101 recorded points prove the adapter's latest-100 window: exactly one
    // entry (the oldest) must be dropped.
    for sequence in 0..=100 {
        repo.snapshot(&format!("pre-turn:{sequence}: prompt {sequence}"))
            .expect("record snapshot");
    }

    let projection = conversation_projection(&mut harness.app);

    let RestorePointProjection::Recorded { snapshots } = &projection.restore_points else {
        panic!("recorded restore points expected");
    };
    assert_eq!(snapshots.len(), 100, "the adapter lists at most 100 points");
    assert_eq!(
        snapshots.first().map(|snapshot| snapshot.sequence),
        Some(Some(100)),
        "newest point is first"
    );
    assert_eq!(
        snapshots.last().map(|snapshot| snapshot.sequence),
        Some(Some(1)),
        "the window stops at the 100th newest point"
    );
    assert!(
        snapshots
            .iter()
            .all(|snapshot| snapshot.sequence != Some(0)),
        "the oldest (101st) point must not cross the window"
    );
}

#[test]
fn adapter_rejects_directory_destination_and_parent_not_a_directory() {
    let mut harness = ExportHarness::new();
    let workspace = canonical_workspace(&harness);
    harness.app.workspace = workspace.clone();

    let directory = workspace.join("existing-dir");
    std::fs::create_dir(&directory).expect("directory target");
    let refused = write_file(&mut harness.app, &directory, b"x", true).unwrap_err();
    assert!(
        refused.contains("refusing to replace a non-regular file"),
        "{refused}"
    );
    assert!(directory.is_dir(), "the directory must be untouched");

    let file_parent = workspace.join("not-a-dir");
    std::fs::write(&file_parent, "file").expect("file parent");
    let parent_error =
        write_file(&mut harness.app, &file_parent.join("out.md"), b"x", false).unwrap_err();
    assert!(
        parent_error.contains("parent is not a directory"),
        "{parent_error}"
    );
    assert!(!file_parent.join("out.md").exists());
}

#[test]
fn adapter_resolves_absolute_paths_and_keeps_the_lexical_fallback() {
    let mut harness = ExportHarness::new();
    let workspace = harness.temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    harness.app.workspace = workspace.clone();
    let canonical_workspace = std::fs::canonicalize(&workspace).expect("canonical workspace");

    // A raw workspace-absolute path rebases onto the resolved workspace.
    assert_eq!(
        resolve(
            &mut harness.app,
            &workspace.join("abs.md").to_string_lossy()
        )
        .expect("raw absolute"),
        canonical_workspace.join("abs.md")
    );
    // A canonicalized workspace-absolute path rebases the same way.
    assert_eq!(
        resolve(
            &mut harness.app,
            &canonical_workspace.join("nested/abs.md").to_string_lossy()
        )
        .expect("canonical absolute"),
        canonical_workspace.join("nested/abs.md")
    );
    // A path outside the workspace stays absolute and is not rebased.
    let outside = harness.temp.path().join("outside.md");
    assert_eq!(
        resolve(&mut harness.app, &outside.to_string_lossy()).expect("outside absolute"),
        outside
    );

    // A workspace that no longer exists falls back to the lexical path instead
    // of failing canonicalization.
    let missing = harness.temp.path().join("gone");
    harness.app.workspace = missing.clone();
    assert_eq!(
        resolve(&mut harness.app, "relative.md").expect("lexical fallback"),
        missing.join("relative.md")
    );
}
