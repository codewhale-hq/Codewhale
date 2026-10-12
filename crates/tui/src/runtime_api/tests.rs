use super::*;
use crate::core::events::{Event as EngineEvent, TurnOutcomeStatus};
use crate::core::ops::{Op, TurnSpec};
use crate::runtime_threads::RuntimeEventRecord;
use crate::test_support::{EnvVarGuard, lock_test_env};
use anyhow::{Context, bail};
use codewhale_models::Role;
use codewhale_models::Usage;
use futures_util::StreamExt;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::time::sleep;
use uuid::Uuid;

mod command_catalog;
mod headless_catalog;
#[cfg(unix)]
mod owner_bootstrap;
mod profile_constitution;
#[cfg(any(unix, windows))]
mod runtime_store_convergence;
mod workspace_instructions;

/// Scale a wait budget for shared CI runners.
///
/// These deadlines are tuned for a developer laptop running one test at a
/// time. CI runs the whole workspace suite on a shared runner, where the same
/// async progress can legitimately take several times longer. Every budget
/// guarded by this helper is a deadline on a poll or a oneshot that resolves
/// as soon as the runtime makes progress, so a larger budget does not slow a
/// passing run down — it only changes how long a genuinely stuck test waits
/// before it fails. Keeping the local value tight preserves fast feedback
/// while the tests stop failing for being unlucky about scheduling.
fn ci_scaled(base: Duration) -> Duration {
    if std::env::var_os("CI").is_some() {
        base * 4
    } else {
        base
    }
}

struct MockExecutor;

#[cfg(unix)]
#[test]
fn runtime_session_fallback_retains_non_unicode_explicit_home_boundary() {
    use std::os::unix::ffi::OsStringExt;

    let _lock = lock_test_env();
    let tmp = tempfile::tempdir().expect("temporary root");
    let home = tmp.path().join("home");
    let explicit = tmp.path().join(std::ffi::OsString::from_vec(
        b"codewhale-\xff-home".to_vec(),
    ));
    let _home = EnvVarGuard::set("HOME", &home);
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", &explicit);

    assert_eq!(fallback_sessions_dir(), explicit.join("sessions"));
}

#[test]
fn thread_route_credential_error_is_bad_request_not_not_found() {
    let credential = map_thread_err(anyhow::anyhow!("DeepSeek API key not found"));
    assert_eq!(credential.status, StatusCode::BAD_REQUEST);

    let missing = map_thread_err(anyhow::anyhow!("thread 'thr_missing' not found"));
    assert_eq!(missing.status, StatusCode::NOT_FOUND);

    let operation_mismatch = map_thread_err(anyhow::anyhow!(
        "operation_key is already bound to a different turn request"
    ));
    assert_eq!(operation_mismatch.status, StatusCode::CONFLICT);
}

#[test]
fn web_launcher_failure_is_a_recoverable_manual_bootstrap_warning() {
    assert!(web_launcher_warning(Ok(())).is_none());
    let warning = web_launcher_warning(Err(anyhow::anyhow!("launcher unavailable")))
        .expect("launcher failure should be reported without failing Runtime startup");
    assert!(warning.contains("could not open the default browser"));
    assert!(warning.contains("open the bootstrap URL above manually"));
}

fn provider_default_model_cases() -> Vec<(&'static str, Config, &'static str)> {
    let deepseek = Config {
        provider: Some("deepseek".to_string()),
        default_text_model: Some("deepseek-v4-flash".to_string()),
        ..Config::default()
    }
    .with_legacy_root(
        Some("deepseek-test-key".to_string()),
        Some("http://127.0.0.1:1/v1".to_string()),
    );

    let mut zai_providers = crate::config::ProvidersConfig::default();
    zai_providers.zai.api_key = Some("zai-test-key".to_string());
    let zai = Config {
        provider: Some("zai".to_string()),
        // A saved DeepSeek root default must not leak onto the active Z.ai route.
        default_text_model: Some(DEFAULT_TEXT_MODEL.to_string()),
        providers: Some(zai_providers),
        ..Config::default()
    };

    let mut custom_providers = crate::config::ProvidersConfig::default();
    custom_providers.custom.insert(
        "acme".to_string(),
        crate::config::ProviderConfig {
            api_key: Some("acme-test-key".to_string()),
            base_url: Some("http://127.0.0.1:1/v1".to_string()),
            model: Some("acme-coder".to_string()),
            kind: Some("openai-compatible".to_string()),
            ..crate::config::ProviderConfig::default()
        },
    );
    let custom = Config {
        provider: Some("acme".to_string()),
        providers: Some(custom_providers),
        ..Config::default()
    };

    vec![
        ("deepseek", deepseek, "deepseek-v4-flash"),
        ("zai", zai, crate::config::DEFAULT_ZAI_MODEL),
        ("custom", custom, "acme-coder"),
    ]
}

#[test]
fn runtime_request_model_uses_the_active_provider_default() {
    for (label, config, expected) in provider_default_model_cases() {
        assert_eq!(
            runtime_request_model(&config, None).expect("configured default"),
            expected,
            "{label} omitted-model resolution"
        );
        assert_eq!(
            runtime_request_model(&config, Some("explicit-model")).expect("explicit model"),
            "explicit-model",
            "{label} explicit model"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn http_and_web_server_thread_manager_installs_configured_workshop_byte_budgets() -> Result<()>
{
    let _workshop_guard = crate::tools::large_output_router::active_workshop_test_guard();
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace)?;
    let config = Config {
        workshop: Some(crate::tools::large_output_router::WorkshopConfig {
            read_result_max_bytes: Some(73_728),
            tool_result_max_bytes: Some(65_536),
            ..crate::tools::large_output_router::WorkshopConfig::default()
        }),
        ..Config::default()
    };

    let (runtime_threads, startup_activation) = open_runtime_threads_for_server(
        &config,
        workspace.clone(),
        RuntimeThreadManagerConfig {
            data_dir: temp.path().join("runtime"),
            task_data_dir: temp.path().join("tasks"),
            sessions_dir: None,
            max_active_threads: 2,
        },
        Arc::new(crate::plugins::PluginRegistry::empty(&workspace)),
    )?;

    assert_eq!(startup_activation.read_result_max_bytes, Some(73_728));
    assert_eq!(startup_activation.tool_result_max_bytes, Some(65_536));
    assert_eq!(
        crate::tools::large_output_router::WorkshopConfig::active_read_result_max_bytes(),
        Some(73_728)
    );
    assert_eq!(
        crate::tools::large_output_router::WorkshopConfig::active_tool_result_max_bytes(),
        Some(65_536)
    );

    let reload_activation = runtime_threads
        .reload_config(Config {
            workshop: Some(crate::tools::large_output_router::WorkshopConfig {
                read_result_max_bytes: Some(81_920),
                tool_result_max_bytes: Some(77_824),
                ..crate::tools::large_output_router::WorkshopConfig::default()
            }),
            ..Config::default()
        })
        .await?;
    assert_eq!(reload_activation.read_result_max_bytes, Some(81_920));
    assert_eq!(reload_activation.tool_result_max_bytes, Some(77_824));
    assert_eq!(
        crate::tools::large_output_router::WorkshopConfig::active_read_result_max_bytes(),
        Some(81_920)
    );
    assert_eq!(
        crate::tools::large_output_router::WorkshopConfig::active_tool_result_max_bytes(),
        Some(77_824)
    );
    crate::tools::large_output_router::WorkshopConfig::install_active(None);
    Ok(())
}

#[test]
fn runtime_tui_settings_reject_legacy_modes_and_do_not_save_env_overlays() -> Result<()> {
    let _lock = lock_test_env();
    let tmp = tempfile::tempdir()?;
    let settings_dir = tmp.path().join(".codewhale");
    fs::create_dir_all(&settings_dir)?;
    fs::write(
        settings_dir.join("settings.toml"),
        "default_mode = \"plan\"\nlow_motion = false\nfancy_animations = true\nauto_compact = true\n",
    )?;
    let _config_path = EnvVarGuard::set(
        "DEEPSEEK_CONFIG_PATH",
        settings_dir.join("config.toml").as_os_str(),
    );
    let _no_animations = EnvVarGuard::set("NO_ANIMATIONS", "1");

    // yolo remains a permission-migration alias, not a startup mode write.
    let error = persist_runtime_tui_setting("default_mode", "yolo")
        .expect_err("yolo must not be saved as a startup mode");
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        crate::settings::Settings::load_persisted()?.default_mode,
        "plan",
        "a rejected write must leave the saved startup mode intact"
    );

    persist_runtime_tui_setting("default_mode", "operate")
        .expect("operate is a valid startup mode");
    assert_eq!(
        crate::settings::Settings::load_persisted()?.default_mode,
        "operate"
    );
    persist_runtime_tui_setting("default_mode", "agent")
        .expect("agent should be a valid startup mode");
    persist_runtime_tui_setting("auto_compact", "false").expect("strict boolean should persist");
    let saved = crate::settings::Settings::load_persisted()?;
    assert_eq!(saved.default_mode, "agent");
    assert!(!saved.auto_compact);
    assert!(!saved.low_motion, "NO_ANIMATIONS is runtime-only");
    assert!(saved.fancy_animations, "NO_ANIMATIONS is runtime-only");
    Ok(())
}

#[async_trait::async_trait]
impl crate::task_manager::TaskExecutor for MockExecutor {
    async fn execute(
        &self,
        _task: crate::task_manager::ExecutionTask,
        events: mpsc::Sender<crate::task_manager::TaskExecutionEvent>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> crate::task_manager::TaskExecutionResult {
        let _ = events
            .send(crate::task_manager::TaskExecutionEvent::Status {
                message: "started".to_string(),
            })
            .await;
        sleep(Duration::from_millis(100)).await;
        if cancel.is_cancelled() {
            return crate::task_manager::TaskExecutionResult {
                status: crate::task_manager::TaskStatus::Canceled,
                result_text: None,
                error: None,
                terminal_reason: crate::task_manager::TaskTerminalReason::Canceled,
            };
        }
        crate::task_manager::TaskExecutionResult {
            status: crate::task_manager::TaskStatus::Completed,
            result_text: Some("ok".to_string()),
            error: None,
            terminal_reason: crate::task_manager::TaskTerminalReason::Completed,
        }
    }
}

fn saved_session_with_blocks(blocks: Vec<codewhale_models::ContentBlock>) -> SavedSession {
    SavedSession {
        schema_version: 1,
        metadata: SessionMetadata {
            id: "session-1".to_string(),
            title: "test session".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            message_count: 1,
            total_tokens: 0,
            model: "test-model".to_string(),
            model_provider: "deepseek".to_string(),
            model_provider_id: None,
            workspace: PathBuf::from("."),
            mode: None,
            cost: Default::default(),
            parent_session_id: None,
            forked_from_message_count: None,
            runtime_store: None,
            cumulative_turn_secs: 0,
            archived: false,
            spawn_depth: 0,
        },
        journal: None,
        leaf_id: None,
        messages: vec![codewhale_models::Message {
            role: Role::Assistant,
            content: blocks,
        }],
        system_prompt: None,
        context_references: Vec::new(),
        artifacts: Vec::new(),
        approval_receipts: Vec::new(),
        work_state: None,
        window_title: None,
        last_auto_route: None,
        turn_outcomes: Vec::new(),
    }
}

fn run_test_git(workspace: &std::path::Path, args: &[&str]) -> Result<()> {
    let output = crate::dependencies::Git::output(args, workspace)
        .with_context(|| format!("git {args:?} failed to spawn"))?;
    if !output.status.success() {
        bail!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

#[test]
fn workspace_status_reports_head_and_dirty_counts() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let repo = tmp.path().join("repo");
    fs::create_dir_all(&repo)?;
    run_test_git(&repo, &["init", "-b", "main"])?;
    run_test_git(&repo, &["config", "core.autocrlf", "false"])?;
    fs::write(repo.join("tracked.txt"), "clean\n")?;
    run_test_git(&repo, &["add", "tracked.txt"])?;
    run_test_git(
        &repo,
        &[
            "-c",
            "user.name=CodeWhale Test",
            "-c",
            "user.email=codewhale@example.invalid",
            "commit",
            "-m",
            "init",
        ],
    )?;

    let clean = collect_workspace_status(&repo);
    assert!(clean.git_repo);
    assert_eq!(clean.branch.as_deref(), Some("main"));
    assert!(clean.head.as_deref().is_some_and(|head| !head.is_empty()));
    assert!(!clean.dirty);

    fs::write(repo.join("tracked.txt"), "dirty\n")?;
    fs::write(repo.join("untracked.txt"), "new\n")?;

    let dirty = collect_workspace_status(&repo);
    assert!(dirty.dirty);
    assert_eq!(dirty.unstaged, 1);
    assert_eq!(dirty.untracked, 1);
    Ok(())
}

#[test]
fn session_detail_scenario() {
    // Scenario consolidation of: session_detail_tool_use_preserves_caller_metadata, session_detail_tool_result_keeps_fallback_content_with_blocks
    // from session_detail_tool_use_preserves_caller_metadata
    {
        let detail = session_to_detail(saved_session_with_blocks(vec![
            codewhale_models::ContentBlock::ToolUse {
                execution_id: Some("host-display-only".into()),
                id: "tool-1".to_string(),
                name: "task_shell_start".to_string(),
                input: json!({ "cmd": "cargo test" }),
                caller: Some(codewhale_models::ToolCaller {
                    caller_type: "subagent".to_string(),
                    tool_id: Some("parent-tool".to_string()),
                }),
                thought_signature: None,
            },
        ]));

        let block = &detail.messages[0]["content"][0];
        assert_eq!(block["type"].as_str(), Some("tool_use"));
        assert!(
            block.get("execution_id").is_none(),
            "detail remains a display projection"
        );
        assert_eq!(block["caller"]["type"].as_str(), Some("subagent"));
        assert_eq!(block["caller"]["tool_id"].as_str(), Some("parent-tool"));
    }
    // from session_detail_tool_result_keeps_fallback_content_with_blocks
    {
        let detail = session_to_detail(saved_session_with_blocks(vec![
            codewhale_models::ContentBlock::ToolResult {
                execution_id: None,
                tool_use_id: "tool-1".to_string(),
                content: "fallback text".to_string(),
                is_error: Some(false),
                content_blocks: Some(vec![json!({
                    "type": "text",
                    "text": "structured text"
                })]),
            },
        ]));

        let block = &detail.messages[0]["content"][0];
        assert_eq!(block["type"].as_str(), Some("tool_result"));
        assert_eq!(block["content"].as_str(), Some("fallback text"));
        assert_eq!(
            block["content_blocks"][0]["text"].as_str(),
            Some("structured text")
        );
        assert_eq!(block["is_error"].as_bool(), Some(false));
    }
}

#[test]
fn messages_from_thread_detail_batches_tool_results() {
    let now = Utc::now();
    let turn_id = "turn_detail".to_string();
    let thread = ThreadRecord {
        schema_version: 2,
        id: "thr_detail".to_string(),
        created_at: now,
        updated_at: now,
        model: DEFAULT_TEXT_MODEL.to_string(),
        model_provider: None,
        model_provider_id: None,
        reasoning_effort: None,
        allowed_tools: None,
        workspace: PathBuf::from("."),
        mode: "agent".to_string(),
        permission_posture: Some("ask".to_string()),
        allow_shell: false,
        trust_mode: false,
        auto_approve: false,
        latest_turn_id: Some(turn_id.clone()),
        latest_response_bookmark: None,
        archived: false,
        system_prompt: None,
        task_id: None,
        title: None,
        session_id: None,
        saved_session_checkpoint: None,
    };
    let turn = TurnRecord {
        decision_receipts: Vec::new(),
        max_output_tokens: None,
        schema_version: 2,
        id: turn_id.clone(),
        thread_id: thread.id.clone(),
        status: RuntimeTurnStatus::Completed,
        input_summary: "check".to_string(),
        created_at: now,
        started_at: Some(now),
        ended_at: Some(now),
        duration_ms: Some(0),
        usage: None,
        model_request_diagnostics: None,
        operation_activity: None,
        routing_settlement: false,
        effective_route_usage: None,
        permission_posture: Some("ask".to_string()),
        mode: None,
        effective_provider: None,
        effective_provider_id: None,
        effective_openrouter_vendor: None,
        effective_billing_surface: None,
        effective_endpoint_fingerprint: None,
        effective_provider_live_pricing: None,
        effective_billing_mode: None,
        effective_dispatched_at: None,
        effective_model: None,
        routed_usage: Vec::new(),
        routed_usage_drop_records: Vec::new(),
        routed_usage_source_ids: Vec::new(),
        routed_usage_dropped_records: 0,
        error: None,
        item_ids: vec![
            "item_user".to_string(),
            "item_reasoning".to_string(),
            "item_tool_use".to_string(),
            "item_result_one".to_string(),
            "item_result_two".to_string(),
            "item_answer".to_string(),
        ],
        steer_count: 0,
        agent_mail_message_id: None,
        artifacts: Vec::new(),
        workspace: None,
        workspace_snapshots: Vec::new(),
    };
    let item = |id: &str,
                kind: TurnItemKind,
                summary: &str,
                detail: Option<&str>,
                metadata: Option<Value>| {
        crate::runtime_threads::TurnItemRecord {
            schema_version: 2,
            id: id.to_string(),
            turn_id: turn_id.clone(),
            kind,
            status: TurnItemLifecycleStatus::Completed,
            summary: summary.to_string(),
            detail: detail.map(str::to_string),
            metadata,
            artifact_refs: Vec::new(),
            artifacts: Vec::new(),
            started_at: Some(now),
            ended_at: Some(now),
        }
    };
    let detail = ThreadDetail {
        thread,
        turns: vec![turn],
        items: vec![
            item(
                "item_user",
                TurnItemKind::UserMessage,
                "check",
                Some("check"),
                None,
            ),
            item(
                "item_reasoning",
                TurnItemKind::AgentReasoning,
                "thinking",
                Some("thinking"),
                None,
            ),
            item(
                "item_tool_use",
                TurnItemKind::ToolCall,
                "shell",
                Some(r#"{"cmd":"pwd"}"#),
                Some(json!({
                    "tool_use_id": "tool-1",
                    "tool_name": "shell"
                })),
            ),
            item(
                "item_result_one",
                TurnItemKind::ToolCall,
                "one",
                Some("one"),
                Some(json!({
                    "tool_result_for": "tool-1",
                    "is_error": false,
                    "content_blocks": [{
                        "type": "text",
                        "text": "structured one"
                    }]
                })),
            ),
            item(
                "item_result_two",
                TurnItemKind::ToolCall,
                "two",
                Some("two"),
                Some(json!({
                    "tool_result_for": "tool-2",
                    "is_error": true
                })),
            ),
            item(
                "item_answer",
                TurnItemKind::AgentMessage,
                "done",
                Some("done"),
                None,
            ),
        ],
        latest_seq: 0,
        pending_approvals: Vec::new(),
        pending_user_inputs: Vec::new(),
        pending_dynamic_tool_calls: Vec::new(),
        approval_grants: Vec::new(),
    };

    let messages = messages_from_thread_detail(&detail).expect("rebuild thread history");
    let roles = messages
        .iter()
        .map(|message| message.role.as_str())
        .collect::<Vec<_>>();
    assert_eq!(roles, vec!["user", "assistant", "user", "assistant"]);
    assert_eq!(messages[2].content.len(), 2);
    match &messages[2].content[0] {
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
            content_blocks,
            ..
        } => {
            assert_eq!(tool_use_id, "tool-1");
            assert_eq!(content, "one");
            assert_eq!(*is_error, None);
            assert_eq!(
                content_blocks
                    .as_ref()
                    .and_then(|blocks| blocks[0].get("text")),
                Some(&json!("structured one"))
            );
        }
        other => panic!("expected first tool result, got {other:?}"),
    }
    match &messages[2].content[1] {
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
            content_blocks,
            ..
        } => {
            assert_eq!(tool_use_id, "tool-2");
            assert_eq!(content, "two");
            assert_eq!(*is_error, Some(true));
            assert!(content_blocks.is_none());
        }
        other => panic!("expected second tool result, got {other:?}"),
    }
}

#[test]
fn legacy_exact_thread_export_normalizes_provider_kind_and_id() {
    let now = Utc::now();
    let detail = ThreadDetail {
        thread: ThreadRecord {
            schema_version: 2,
            id: "thr_legacy_custom".to_string(),
            created_at: now,
            updated_at: now,
            model: "local-model".to_string(),
            // Pre-additive records overloaded this legacy field with the exact id.
            model_provider: Some("lm-studio".to_string()),
            model_provider_id: None,
            reasoning_effort: None,
            allowed_tools: None,
            workspace: PathBuf::from("."),
            mode: "agent".to_string(),
            permission_posture: None,
            allow_shell: false,
            trust_mode: false,
            auto_approve: false,
            latest_turn_id: None,
            latest_response_bookmark: None,
            archived: false,
            system_prompt: None,
            task_id: None,
            title: None,
            session_id: None,
            saved_session_checkpoint: None,
        },
        turns: Vec::new(),
        items: Vec::new(),
        latest_seq: 0,
        pending_approvals: Vec::new(),
        pending_user_inputs: Vec::new(),
        pending_dynamic_tool_calls: Vec::new(),
        approval_grants: Vec::new(),
    };
    let config = Config {
        provider: Some("lm-studio".to_string()),
        providers: Some(crate::config::ProvidersConfig {
            custom: std::collections::HashMap::from([(
                "lm-studio".to_string(),
                crate::config::ProviderConfig {
                    kind: Some("openai-compatible".to_string()),
                    base_url: Some("http://127.0.0.1:1234/v1".to_string()),
                    model: Some("local-model".to_string()),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut session = crate::session_manager::create_saved_session_with_mode(
        &[],
        "local-model",
        std::path::Path::new("."),
        0,
        None,
        Some("agent"),
    );

    sessions::stamp_session_provider_from_thread(&config, &detail, &mut session.metadata)
        .expect("normalize legacy exact provider");

    assert_eq!(session.metadata.model_provider, "custom");
    assert_eq!(
        session.metadata.model_provider_id.as_deref(),
        Some("lm-studio")
    );
}

#[test]
fn runtime_auth_scenario() {
    // Scenario consolidation of: runtime_auth_generates_token_by_default, runtime_auth_status_does_not_render_generated_token, runtime_auth_requires_explicit_insecure_for_no_token, runtime_auth_prefers_cli_token_over_env_token, runtime_auth_ignores_blank_configured_tokens
    // from runtime_auth_generates_token_by_default
    {
        let auth = resolve_runtime_auth(None, None, false);
        assert!(auth.generated);
        let token = auth.token.expect("generated token");
        assert!(token.starts_with("cwrt_"));
        assert!(token.len() > 32);
    }
    // from runtime_auth_status_does_not_render_generated_token
    {
        let auth = ResolvedRuntimeAuth {
            token: Some("cwrt_super_secret_test_token".to_string()),
            generated: true,
        };
        let rendered = runtime_auth_status_lines(&auth).join("\n");

        assert!(!rendered.contains("cwrt_super_secret_test_token"));
        assert!(rendered.contains("not printed"));
    }
    // from runtime_auth_requires_explicit_insecure_for_no_token
    {
        let auth = resolve_runtime_auth(None, None, true);
        assert_eq!(
            auth,
            ResolvedRuntimeAuth {
                token: None,
                generated: false,
            }
        );
    }
    // from runtime_auth_prefers_cli_token_over_env_token
    {
        let auth = resolve_runtime_auth(
            Some(" cli-token ".to_string()),
            Some("env-token".to_string()),
            false,
        );
        assert_eq!(
            auth,
            ResolvedRuntimeAuth {
                token: Some("cli-token".to_string()),
                generated: false,
            }
        );
    }
    // from runtime_auth_ignores_blank_configured_tokens
    {
        let auth = resolve_runtime_auth(Some(" ".to_string()), Some("\t".to_string()), false);
        assert!(auth.generated);
        assert!(auth.token.is_some());
    }
}

#[test]
fn runtime_token_scenario() {
    // Scenario consolidation of: runtime_token_environment_prefers_the_codewhale_name, runtime_token_environment_falls_through_a_blank_primary_to_the_legacy_alias
    // from runtime_token_environment_prefers_the_codewhale_name
    {
        let environment = runtime_token_environment(&|name| match name {
            RUNTIME_TOKEN_ENV => Some(" canonical-token ".to_string()),
            LEGACY_RUNTIME_TOKEN_ENV => Some("legacy-token".to_string()),
            _ => None,
        });

        assert_eq!(environment.token.as_deref(), Some("canonical-token"));
        assert!(!environment.legacy_alias_used);
        assert!(runtime_token_alias_warning(None, &environment).is_none());
    }
    // from runtime_token_environment_falls_through_a_blank_primary_to_the_legacy_alias
    {
        let environment = runtime_token_environment(&|name| match name {
            RUNTIME_TOKEN_ENV => Some(" \t ".to_string()),
            LEGACY_RUNTIME_TOKEN_ENV => Some(" legacy-token ".to_string()),
            _ => None,
        });

        assert_eq!(environment.token.as_deref(), Some("legacy-token"));
        assert!(environment.legacy_alias_used);
    }
}

/// Port 0 lets a supervised loopback Runtime own an ephemeral endpoint and
/// report it. Anything a client is handed a fixed address for keeps a real port.
#[test]
fn ephemeral_port_is_only_for_a_plain_loopback_runtime() {
    let ephemeral = |host: &str, web: bool, mobile: bool| RuntimeApiOptions {
        host: host.to_string(),
        port: 0,
        web,
        mobile,
        ..RuntimeApiOptions::default()
    };
    assert!(validate_runtime_listener_security(&ephemeral("127.0.0.1", false, false)).is_ok());
    for refused in [
        ephemeral("0.0.0.0", false, false),
        ephemeral("127.0.0.1", true, false),
        ephemeral("127.0.0.1", false, true),
    ] {
        let err = validate_runtime_listener_security(&refused).unwrap_err();
        assert!(err.to_string().contains("Port must be > 0"), "{err}");
    }
}

#[test]
fn mobile_listener_fails_closed_outside_loopback_without_verified_transport() {
    let non_loopback = RuntimeApiOptions {
        host: "0.0.0.0".to_string(),
        mobile: true,
        insecure_no_auth: true,
        ..RuntimeApiOptions::default()
    };
    let err = validate_runtime_listener_security(&non_loopback).unwrap_err();
    assert!(err.to_string().contains("mobile is loopback-only"));

    for host in ["127.0.0.1", "::1"] {
        let loopback = RuntimeApiOptions {
            host: host.to_string(),
            mobile: true,
            ..RuntimeApiOptions::default()
        };
        assert!(
            validate_runtime_listener_security(&loopback).is_ok(),
            "host={host}"
        );
    }
    assert_eq!(
        runtime_bind_address("::1", 7878)
            .expect("IPv6 loopback bind address")
            .to_string(),
        "[::1]:7878"
    );

    let ordinary_http = RuntimeApiOptions {
        host: "0.0.0.0".to_string(),
        mobile: false,
        ..RuntimeApiOptions::default()
    };
    assert!(validate_runtime_listener_security(&ordinary_http).is_ok());
}

#[test]
fn insecure_runtime_listener_is_loopback_only() {
    for host in [
        "0.0.0.0",
        "::",
        "192.168.1.2",
        "8.8.8.8",
        "localhost",
        "::ffff:127.0.0.1",
    ] {
        let mut options = RuntimeApiOptions {
            host: host.into(),
            insecure_no_auth: true,
            ..RuntimeApiOptions::default()
        };
        assert!(
            validate_runtime_listener_security(&options).is_err(),
            "{host}"
        );
        options.insecure_no_auth = false;
        assert!(
            validate_runtime_listener_security(&options).is_ok(),
            "authenticated {host}"
        );
    }
    for host in ["127.0.0.1", "127.0.0.2", "::1"] {
        let options = RuntimeApiOptions {
            host: host.into(),
            insecure_no_auth: true,
            ..RuntimeApiOptions::default()
        };
        assert!(
            validate_runtime_listener_security(&options).is_ok(),
            "{host}"
        );
    }
}

#[test]
fn consumed_legacy_runtime_token_reports_one_value_free_deprecation_line() {
    let secret = "legacy-super-secret-token";
    let environment = runtime_token_environment(&|name| {
        (name == LEGACY_RUNTIME_TOKEN_ENV).then(|| secret.to_string())
    });
    let warning = runtime_token_alias_warning(None, &environment).expect("legacy warning");

    assert_eq!(warning.lines().count(), 1);
    assert!(warning.contains(LEGACY_RUNTIME_TOKEN_ENV));
    assert!(warning.contains(RUNTIME_TOKEN_ENV));
    assert!(warning.contains("0.10.0"));
    assert!(!warning.contains(secret));
}

#[test]
fn explicit_cli_runtime_token_does_not_warn_about_an_unused_legacy_alias() {
    let environment = runtime_token_environment(&|name| {
        (name == LEGACY_RUNTIME_TOKEN_ENV).then(|| "legacy-token".to_string())
    });

    assert!(runtime_token_alias_warning(Some("cli-token"), &environment).is_none());
    assert!(runtime_token_alias_warning(Some(" \t"), &environment).is_some());
}

async fn spawn_test_server_with_root(
    root: PathBuf,
    sessions_dir: PathBuf,
) -> Result<
    Option<(
        SocketAddr,
        SharedRuntimeThreadManager,
        tokio::task::JoinHandle<()>,
    )>,
> {
    spawn_test_server_with_root_and_token(root, sessions_dir, None).await
}

async fn spawn_test_server_with_root_and_token(
    root: PathBuf,
    sessions_dir: PathBuf,
    runtime_token: Option<String>,
) -> Result<
    Option<(
        SocketAddr,
        SharedRuntimeThreadManager,
        tokio::task::JoinHandle<()>,
    )>,
> {
    spawn_test_server_with_root_token_and_mobile(root, sessions_dir, runtime_token, false).await
}

async fn spawn_test_server_with_root_token_and_mobile(
    root: PathBuf,
    sessions_dir: PathBuf,
    runtime_token: Option<String>,
    mobile_enabled: bool,
) -> Result<
    Option<(
        SocketAddr,
        SharedRuntimeThreadManager,
        tokio::task::JoinHandle<()>,
    )>,
> {
    spawn_test_server_with_root_token_mobile_workspace(
        root,
        sessions_dir,
        runtime_token,
        mobile_enabled,
        PathBuf::from("."),
    )
    .await
}

async fn spawn_test_server_with_root_token_mobile_workspace(
    root: PathBuf,
    sessions_dir: PathBuf,
    runtime_token: Option<String>,
    mobile_enabled: bool,
    workspace: PathBuf,
) -> Result<
    Option<(
        SocketAddr,
        SharedRuntimeThreadManager,
        tokio::task::JoinHandle<()>,
    )>,
> {
    spawn_test_server_with_root_token_mobile_workspace_and_subagents(
        root,
        sessions_dir,
        runtime_token,
        mobile_enabled,
        workspace,
        None,
        None,
    )
    .await
}

#[derive(Default)]
struct TestServerOverrides {
    automation_handles: Option<
        oneshot::Sender<(
            crate::automation_manager::SharedAutomationManager,
            crate::task_manager::SharedTaskManager,
        )>,
    >,
    /// Publish the exact manager using the production owner/frontend factory.
    owner_socket: Option<PathBuf>,
    /// Capture the actual owner's service table for cache/lifetime assertions.
    workspace_scopes_handle: Option<oneshot::Sender<Arc<RuntimeWorkspaceScopes>>>,
    sub_agent_manager: Option<SharedSubAgentManager>,
    fleet_codewhale_binary: Option<String>,
    config: Option<Config>,
    config_path: Option<PathBuf>,
    /// Observe the exact loaded config shared with the HTTP handlers.
    config_handle: Option<Arc<parking_lot::RwLock<Config>>>,
    config_profile: Option<String>,
    mobile: Option<mobile::RuntimeMobileState>,
    web: Option<web::RuntimeWebState>,
    compat_stream_test_hook: Option<mpsc::UnboundedSender<CompatStreamTestPoint>>,
    plugin_discovery: Option<Arc<crate::plugins::PluginDiscoveryContext>>,
    /// Pre-seeded workspace LSP manager (test transports, disabled configs).
    lsp_manager: Option<Arc<crate::lsp::LspManager>>,
    /// Engine LRU capacity; `None` keeps the fixture default of 8.
    max_active_threads: Option<usize>,
    /// Stops the server through the product's own graceful-shutdown path.
    shutdown: Option<RuntimeServerShutdown>,
}

async fn spawn_test_server_with_root_token_mobile_workspace_and_subagents(
    root: PathBuf,
    sessions_dir: PathBuf,
    runtime_token: Option<String>,
    mobile_enabled: bool,
    workspace: PathBuf,
    sub_agent_manager: Option<SharedSubAgentManager>,
    fleet_codewhale_binary: Option<String>,
) -> Result<
    Option<(
        SocketAddr,
        SharedRuntimeThreadManager,
        tokio::task::JoinHandle<()>,
    )>,
> {
    spawn_test_server_with_root_token_mobile_workspace_and_overrides(
        root,
        sessions_dir,
        runtime_token,
        mobile_enabled,
        workspace,
        TestServerOverrides {
            sub_agent_manager,
            fleet_codewhale_binary,
            ..TestServerOverrides::default()
        },
    )
    .await
}

async fn spawn_test_server_with_root_token_mobile_workspace_and_overrides(
    root: PathBuf,
    sessions_dir: PathBuf,
    runtime_token: Option<String>,
    mobile_enabled: bool,
    workspace: PathBuf,
    overrides: TestServerOverrides,
) -> Result<
    Option<(
        SocketAddr,
        SharedRuntimeThreadManager,
        tokio::task::JoinHandle<()>,
    )>,
> {
    let (setup_tx, setup_rx) = oneshot::channel();
    // If this test sealed the process environment (`lock_test_env`), the
    // server thread must join that scope: its `Config::load` reads env through
    // `with_test_env_lock`, which would otherwise block on the mutex the test
    // holds while awaiting this very setup — a deadlock.
    let env_ticket = crate::test_support::env_scope_ticket();
    spawn_product_stack_server(
        root,
        sessions_dir,
        runtime_token,
        mobile_enabled,
        workspace,
        overrides,
        env_ticket,
        setup_tx,
    );
    let Some((addr, runtime_threads, shutdown_tx)) = setup_rx
        .await
        .context("runtime-api test server thread ended during setup")??
    else {
        return Ok(None);
    };
    // Owns the shutdown side for as long as the test keeps the handle alive:
    // aborting it (or the test runtime dropping it) closes the listener and
    // ends the server thread.
    let handle = tokio::spawn(async move {
        let _shutdown = shutdown_tx;
        std::future::pending::<()>().await
    });
    Ok(Some((addr, runtime_threads, handle)))
}

/// What the server thread hands back once the router is bound: everything the
/// test body needs, plus the shutdown side of the server.
type TestServerSetup = (SocketAddr, SharedRuntimeThreadManager, oneshot::Sender<()>);

/// Builds and serves the Runtime API router where the product builds and
/// serves it: on a thread with the product's `CODEWHALE_MAIN_STACK_BYTES`
/// stack.
///
/// `#[tokio::test]` drives its current-thread runtime on the 2 MiB libtest
/// thread, so a harness that built its state and spawned its server onto that
/// runtime ran every product path — config load and reload (the serde
/// `toml::de::visit_map` frames for the full `Config`), manager construction,
/// thread lifecycle, streaming — on a stack the product never gives it
/// (`lib.rs` sizes the runtime workers with `CODEWHALE_MAIN_STACK_BYTES`).
/// Config load under a profile and the thread-lifecycle path marginally
/// overflowed 2 MiB in debug builds (`has overflowed its stack`, SIGABRT for
/// the whole lib suite), which CI masked with `RUST_MIN_STACK`. Running setup
/// *and* serving on one product-sized thread removes the class: the libtest
/// thread keeps only the test body and its HTTP client, and no product frame
/// depth can overflow it.
///
/// Setup results come back through `setup_tx`; the caller wraps the shutdown
/// sender in the `JoinHandle` the call sites expect. Nothing here runs on the
/// test's runtime, so the server thread must outlive setup: it serves until
/// the shutdown sender is dropped.
///
/// `env_ticket` adopts the thread into the calling test's sealed env scope
/// (`test_env_lock::join_env_scope`), so its `Config::load` env reads see the
/// test's environment instead of blocking on the mutex the test holds while
/// awaiting setup. `None` when the caller sealed nothing.
fn spawn_product_stack_server(
    root: PathBuf,
    sessions_dir: PathBuf,
    runtime_token: Option<String>,
    mobile_enabled: bool,
    workspace: PathBuf,
    mut overrides: TestServerOverrides,
    env_ticket: Option<crate::test_support::EnvScopeTicket>,
    setup_tx: oneshot::Sender<Result<Option<TestServerSetup>>>,
) {
    let shutdown = overrides
        .shutdown
        .get_or_insert_with(RuntimeServerShutdown::default)
        .clone();
    std::thread::Builder::new()
        .name("runtime-api-test-server".to_string())
        .stack_size(codewhale_runtime::CODEWHALE_MAIN_STACK_BYTES)
        .spawn(move || {
            // Adopted for the thread's lifetime; the scope's generation check
            // refuses enrollment once the sealing test has ended.
            let _membership = crate::test_support::join_env_scope(env_ticket);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime-api test server runtime");
            runtime.block_on(async move {
                match build_test_server(
                    root,
                    sessions_dir,
                    runtime_token,
                    mobile_enabled,
                    workspace,
                    overrides,
                )
                .await
                {
                    Ok(Some((listener, app, addr, runtime_threads, owner_frontend))) => {
                        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
                        if setup_tx
                            .send(Ok(Some((addr, runtime_threads, shutdown_tx))))
                            .is_err()
                        {
                            // The test gave up waiting; do not serve.
                            return;
                        }
                        listener
                            .set_nonblocking(true)
                            .expect("nonblocking test listener");
                        let listener =
                            TcpListener::from_std(listener).expect("register test listener");
                        // Stop the owner the way the product does: trigger its
                        // shutdown and await it while this runtime is alive, so
                        // the owner receipt is retired before the thread exits.
                        let owner_handle =
                            owner_frontend.as_ref().map(|owner| owner.shutdown_handle());
                        let mut owner_task =
                            owner_frontend.map(|owner| tokio::spawn(owner.serve()));
                        tokio::select! {
                            _ = serve_runtime_api(listener, app, shutdown) => {}
                            _ = shutdown_rx => {}
                            _ = async {
                                match owner_task.as_mut() {
                                    Some(task) => { let _ = task.await; }
                                    None => std::future::pending::<()>().await,
                                }
                            } => {
                                owner_task = None;
                            }
                        }
                        if let Some(handle) = owner_handle {
                            handle.trigger();
                        }
                        if let Some(task) = owner_task {
                            let _ = task.await;
                        }
                    }
                    Ok(None) => {
                        let _ = setup_tx.send(Ok(None));
                    }
                    Err(err) => {
                        let _ = setup_tx.send(Err(err));
                    }
                }
            });
        })
        .expect("spawn runtime-api test server thread");
}

/// The whole harness body — config load, managers, router build — formerly
/// inline in `spawn_test_server_with_root_token_mobile_workspace_and_overrides`.
/// Runs on the product-stack server thread (see `spawn_product_stack_server`).
async fn build_test_server(
    root: PathBuf,
    sessions_dir: PathBuf,
    runtime_token: Option<String>,
    mobile_enabled: bool,
    workspace: PathBuf,
    overrides: TestServerOverrides,
) -> Result<
    Option<(
        std::net::TcpListener,
        axum::Router,
        SocketAddr,
        SharedRuntimeThreadManager,
        Option<codewhale_app_server::daemon_socket::DaemonSocket>,
    )>,
> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    fs::create_dir_all(&sessions_dir)?;
    fs::create_dir_all(&workspace)?;
    let mut config = if let Some(config) = overrides.config.clone() {
        config
    } else if let Some(path) = overrides.config_path.clone() {
        Config::load(Some(path), None)?
    } else {
        Config {
            ..Config::default()
        }
        .with_legacy_root(
            Some("runtime-api-test-key".to_string()),
            Some("http://127.0.0.1:1/v1".to_string()),
        )
    };
    config.mcp_config_path = Some(root.join("mcp.json").to_string_lossy().to_string());
    let manager = TaskManager::start_with_executor(
        TaskManagerConfig {
            data_dir: root.join("tasks"),
            worker_count: 1,
            default_workspace: workspace.clone(),
            default_model: DEFAULT_TEXT_MODEL.to_string(),
            default_mode: "agent".to_string(),
            allow_shell: false,
            trust_mode: false,
            execution_limits: crate::task_manager::TaskExecutionLimits::default(),
        },
        Arc::new(MockExecutor),
    )
    .await?;
    let runtime_threads: SharedRuntimeThreadManager = Arc::new(RuntimeThreadManager::open(
        config.clone(),
        workspace.clone(),
        // A neighboring libtest may temporarily set CODEWHALE_RUNTIME_DIR.
        // This fixture owns an explicit root and must never borrow that
        // other test's live store or contend for its process-owner lock.
        RuntimeThreadManagerConfig {
            data_dir: root.join("runtime/runtime"),
            task_data_dir: root.join("runtime"),
            sessions_dir: Some(sessions_dir.clone()),
            max_active_threads: overrides.max_active_threads.unwrap_or(8),
        },
    )?);
    let sessions_dir = runtime_threads.sessions_dir().to_path_buf();
    runtime_threads.attach_task_manager(manager.clone());
    let automations = Arc::new(Mutex::new(AutomationManager::open_for_test(
        root.join("automations"),
    )?));
    runtime_threads.attach_automation_manager(automations.clone());
    if let Some(sender) = overrides.automation_handles {
        let _ = sender.send((automations.clone(), manager.clone()));
    }

    let auth_required = runtime_token.is_some();
    let sub_agent_manager = overrides
        .sub_agent_manager
        .unwrap_or_else(|| runtime_api_sub_agent_manager(&workspace, 2));
    // A std listener: the server thread registers it with its own runtime
    // after setup is reported (see `spawn_product_stack_server`).
    let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    let addr = listener.local_addr()?;
    let mobile = if mobile_enabled && runtime_token.is_some() {
        Some(
            overrides
                .mobile
                .unwrap_or_else(|| mobile::RuntimeMobileState::new().0),
        )
    } else {
        None
    };
    let config = if let Some(handle) = overrides.config_handle {
        *handle.write() = config;
        handle
    } else {
        Arc::new(parking_lot::RwLock::new(config))
    };
    let workspace_scopes =
        RuntimeWorkspaceScopes::new(runtime_threads.clone(), sub_agent_manager.clone());
    let workspace_scope = workspace_scopes.admit(workspace.clone()).await?;
    if let Some(sender) = overrides.workspace_scopes_handle {
        let _ = sender.send(workspace_scopes.clone());
    }
    if let Some(manager) = overrides.lsp_manager {
        let _ = workspace_scope.lsp.set(manager);
    }
    let state = RuntimeApiState {
        config,
        workspace,
        plugin_discovery: overrides
            .plugin_discovery
            .unwrap_or_else(crate::plugins::PluginDiscoveryContext::capture_pre_dotenv),
        task_manager: manager,
        runtime_threads: runtime_threads.clone(),
        cors_origins: Vec::new(),
        sessions_dir,
        config_path: overrides.config_path.clone(),
        config_profile: overrides.config_profile,
        workspace_scopes,
        workspace_scope,
        automations,
        sub_agent_manager,
        runtime_token,
        skill_state: Arc::new(Mutex::new(
            SkillStateStore::load_from(root.join("skills_state.toml")).unwrap(),
        )),
        auth_required,
        bind_host: "127.0.0.1".to_string(),
        bind_port: addr.port(),
        mobile_enabled,
        mobile,
        web: overrides.web,
        fleet_codewhale_binary: overrides
            .fleet_codewhale_binary
            .unwrap_or_else(configured_codewhale_binary),
        computer: super::computer_display::ComputerState::from_env(),
        shutdown: overrides.shutdown.clone().unwrap_or_default(),
        git_writes: Arc::new(tokio::sync::Mutex::new(())),
        provider_switches: Arc::new(tokio::sync::Mutex::new(())),
        compat_stream_test_hook: overrides.compat_stream_test_hook,
    };
    let (owner_frontend, compatibility) = if let Some(socket) = overrides.owner_socket {
        let model = runtime_request_model(&state.config.read(), None)
            .map_err(|error| anyhow::anyhow!(error.message))?;
        let (owner, compatibility) =
            bind_captured_runtime_frontends(&state, Some(socket), model, 1, false, None).await?;
        (Some(owner), Some(compatibility))
    } else {
        (None, None)
    };
    let mut app = build_router(state.clone());
    if let Some(compatibility) = compatibility {
        app = app.merge(codewhale_app_server::runtime_compatibility_router(
            compatibility,
            &state.cors_origins,
            state.runtime_token.clone(),
            Some(state.workspace.clone()),
        ));
    }
    Ok(Some((listener, app, addr, runtime_threads, owner_frontend)))
}

async fn spawn_test_server() -> Result<
    Option<(
        SocketAddr,
        SharedRuntimeThreadManager,
        tokio::task::JoinHandle<()>,
    )>,
> {
    let root = std::env::temp_dir().join(format!("deepseek-runtime-api-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    spawn_test_server_with_root(root, sessions_dir).await
}

async fn spawn_test_server_with_config_path(
    config_path: PathBuf,
) -> Result<
    Option<(
        SocketAddr,
        SharedRuntimeThreadManager,
        tokio::task::JoinHandle<()>,
    )>,
> {
    let root = std::env::temp_dir().join(format!("codewhale-config-api-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    let workspace = root.join("workspace");
    fs::create_dir_all(&root)?;
    spawn_test_server_with_root_token_mobile_workspace_and_overrides(
        root,
        sessions_dir,
        None,
        false,
        workspace,
        TestServerOverrides {
            config_path: Some(config_path),
            ..TestServerOverrides::default()
        },
    )
    .await
}

async fn spawn_test_server_with_config_path_and_profile(
    config_path: PathBuf,
    config_profile: String,
) -> Result<
    Option<(
        SocketAddr,
        SharedRuntimeThreadManager,
        tokio::task::JoinHandle<()>,
    )>,
> {
    let root = std::env::temp_dir().join(format!("codewhale-config-api-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    let workspace = root.join("workspace");
    fs::create_dir_all(&root)?;
    spawn_test_server_with_root_token_mobile_workspace_and_overrides(
        root,
        sessions_dir,
        None,
        false,
        workspace,
        TestServerOverrides {
            config_path: Some(config_path),
            config_profile: Some(config_profile),
            ..TestServerOverrides::default()
        },
    )
    .await
}

async fn read_first_sse_frame(resp: reqwest::Response) -> Result<String> {
    let mut stream = resp.bytes_stream();
    let mut buf = Vec::new();
    loop {
        let next = tokio::time::timeout(ci_scaled(Duration::from_secs(2)), stream.next())
            .await
            .context("timed out waiting for SSE frame")?
            .context("SSE stream ended unexpectedly")??;
        buf.extend_from_slice(&next);

        let text = String::from_utf8_lossy(&buf);
        if let Some(idx) = text.find("\n\n").or_else(|| text.find("\r\n\r\n")) {
            return Ok(text[..idx].to_string());
        }

        if buf.len() > 64 * 1024 {
            bail!("SSE frame exceeded 64KB without delimiter");
        }
    }
}

fn take_complete_sse_frame(buffer: &mut Vec<u8>) -> Result<Option<String>> {
    let text = String::from_utf8_lossy(buffer);
    let lf = text.find("\n\n").map(|index| (index, 2));
    let crlf = text.find("\r\n\r\n").map(|index| (index, 4));
    let delimiter = match (lf, crlf) {
        (Some(left), Some(right)) => Some(if left.0 <= right.0 { left } else { right }),
        (Some(found), None) | (None, Some(found)) => Some(found),
        (None, None) => None,
    };
    let Some((index, delimiter_len)) = delimiter else {
        return Ok(None);
    };
    let frame = String::from_utf8(buffer[..index].to_vec())?;
    buffer.drain(..index + delimiter_len);
    Ok(Some(frame))
}

async fn collect_sse_frames(
    response: reqwest::Response,
    frame_tx: mpsc::UnboundedSender<(String, serde_json::Value)>,
) -> Result<Vec<(String, serde_json::Value)>> {
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();
    let mut frames = Vec::new();
    while let Some(chunk) = stream.next().await {
        buffer.extend_from_slice(&chunk?);
        while let Some(raw) = take_complete_sse_frame(&mut buffer)? {
            if raw.trim().is_empty() || raw.trim_start().starts_with(':') {
                continue;
            }
            let frame = parse_sse_frame(&raw)?;
            frame_tx
                .send(frame.clone())
                .map_err(|_| anyhow::anyhow!("SSE frame observer closed"))?;
            frames.push(frame);
        }
        if buffer.len() > 64 * 1024 {
            bail!("SSE frame exceeded 64KB without delimiter");
        }
    }
    Ok(frames)
}

#[cfg(unix)]
fn write_fake_fleet_binary(root: &Path, marker: &Path) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    let binary = root.join("fake-codewhale");
    fs::write(
        &binary,
        format!(
            "#!/bin/sh\ntouch '{}'\nprintf '{{\"type\":\"content\",\"content\":\"restarted through Runtime API\"}}\\n'\nexit 0\n",
            marker.display()
        ),
    )?;
    let mut permissions = fs::metadata(&binary)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&binary, permissions)?;
    Ok(binary)
}

#[cfg(windows)]
fn write_fake_fleet_binary(root: &Path, marker: &Path) -> Result<PathBuf> {
    // Exercise the same executable/Job Object path as a released Windows
    // Codewhale binary. A `.cmd` fake introduces an extra `cmd.exe` wrapper
    // whose lifetime can end before the Fleet host attaches its Job Object,
    // making the test race a process topology production does not use.
    let source = root.join("fake-codewhale.rs");
    let binary = root.join("fake-codewhale.exe");
    let helper = format!(
        r##"fn main() {{
    std::fs::File::create({marker:?}).expect("create Fleet restart marker");
    println!("{{}}", r#"{{"type":"content","content":"restarted through Runtime API"}}"#);
    std::thread::sleep(std::time::Duration::from_millis(750));
}}
"##,
        marker = marker.to_string_lossy().as_ref(),
    );
    fs::write(&source, helper)?;
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = std::process::Command::new(rustc)
        .arg("--edition=2024")
        .arg("--crate-name=codewhale_fleet_test_helper")
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .output()
        .context("compile Windows Fleet restart helper")?;
    if !output.status.success() {
        bail!(
            "failed to compile Windows Fleet restart helper: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(binary)
}

fn parse_sse_frame(frame: &str) -> Result<(String, serde_json::Value)> {
    let mut event_name: Option<String> = None;
    let mut data_lines = Vec::new();
    for line in frame.lines() {
        if let Some(rest) = line.strip_prefix("event:") {
            event_name = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.trim_start().to_string());
        }
    }
    let event_name = event_name.context("missing SSE event field")?;
    let payload = if data_lines.is_empty() {
        json!({})
    } else {
        serde_json::from_str(&data_lines.join("\n"))
            .with_context(|| format!("invalid SSE data payload: {}", data_lines.join("\n")))?
    };
    Ok((event_name, payload))
}

/// Polls until the turn is terminal. `timeout` only bounds how long a stuck
/// turn takes to fail the test, so keep it generous: a loaded shared-process
/// `cargo test` run can take seconds to settle a mock turn (#6698).
async fn wait_for_terminal_turn_status(
    client: &reqwest::Client,
    addr: SocketAddr,
    thread_id: &str,
    turn_id: &str,
    timeout: Duration,
) -> Result<String> {
    let deadline = tokio::time::Instant::now() + ci_scaled(timeout);
    loop {
        let detail: serde_json::Value = client
            .get(format!("http://{addr}/v1/threads/{thread_id}"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let status = detail["turns"]
            .as_array()
            .and_then(|turns| turns.iter().find(|turn| turn["id"] == turn_id))
            .and_then(|turn| turn.get("status"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if matches!(
            status.as_str(),
            "completed" | "failed" | "interrupted" | "canceled"
        ) {
            return Ok(status);
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("timed out waiting for terminal turn status for {turn_id}");
        }
        sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_for_in_progress_item(
    client: &reqwest::Client,
    addr: SocketAddr,
    thread_id: &str,
    timeout: Duration,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + ci_scaled(timeout);
    loop {
        let detail: serde_json::Value = client
            .get(format!("http://{addr}/v1/threads/{thread_id}"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if detail["items"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["status"] == "in_progress"))
        {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("timed out waiting for in-progress item in thread {thread_id}");
        }
        sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn health_and_tasks_endpoints_work() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let health: serde_json::Value = client
        .get(format!("http://{addr}/health"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(health["status"], "ok");
    assert_eq!(health["service"], "codewhale-runtime-api");

    let created: serde_json::Value = client
        .post(format!("http://{addr}/v1/tasks"))
        .json(&json!({ "prompt": "hello task" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let id = created["id"].as_str().expect("task id").to_string();

    let listed: serde_json::Value = client
        .get(format!("http://{addr}/v1/tasks"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert!(
        listed["tasks"]
            .as_array()
            .is_some_and(|tasks| !tasks.is_empty())
    );

    let detail: serde_json::Value = client
        .get(format!("http://{addr}/v1/tasks/{id}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(detail["id"], id);

    let _cancelled: serde_json::Value = client
        .post(format!("http://{addr}/v1/tasks/{id}/cancel"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    handle.abort();
    Ok(())
}

/// Created tasks keep their caller-given name through get and list; unnamed
/// tasks omit the field so queues can fall back to the prompt summary.
#[tokio::test]
async fn created_tasks_keep_their_given_name() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let named: serde_json::Value = client
        .post(format!("http://{addr}/v1/tasks"))
        .json(&json!({ "prompt": "migrate the widget", "name": "Widget migration" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(named["name"], "Widget migration");
    let id = named["id"].as_str().expect("task id").to_string();

    let unnamed: serde_json::Value = client
        .post(format!("http://{addr}/v1/tasks"))
        .json(&json!({ "prompt": "unnamed work" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert!(unnamed.get("name").is_none());

    let detail: serde_json::Value = client
        .get(format!("http://{addr}/v1/tasks/{id}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(detail["name"], "Widget migration");

    let listed: serde_json::Value = client
        .get(format!("http://{addr}/v1/tasks"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let rows = listed["tasks"].as_array().expect("task rows");
    let row = rows
        .iter()
        .find(|row| row["id"] == id)
        .expect("named row in list");
    assert_eq!(row["name"], "Widget migration");

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn omitted_runtime_models_use_the_active_provider_default() -> Result<()> {
    let _env = lock_test_env();
    for (label, config, expected) in provider_default_model_cases() {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join(label);
        let sessions_dir = root.join("sessions");
        let workspace = root.join("workspace");
        let (hook_tx, mut hook_rx) = mpsc::unbounded_channel();
        let Some((addr, runtime_threads, handle)) =
            spawn_test_server_with_root_token_mobile_workspace_and_overrides(
                root,
                sessions_dir,
                None,
                false,
                workspace,
                TestServerOverrides {
                    config: Some(config),
                    compat_stream_test_hook: Some(hook_tx),
                    ..TestServerOverrides::default()
                },
            )
            .await?
        else {
            return Ok(());
        };

        // A provider pin cannot borrow the active provider's default model.
        // It must reach the same explicit-model guard as direct task creation.
        for pin in [
            json!({"model_provider": "openai"}),
            json!({"model_provider_id": "other-route"}),
        ] {
            let mut request = pin;
            request["prompt"] = json!("reject missing pinned model");
            let rejected = crate::tls::reqwest_client()
                .post(format!("http://{addr}/v1/tasks"))
                .json(&request)
                .send()
                .await?;
            assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
            assert!(
                rejected
                    .text()
                    .await?
                    .contains("pinned task provider requires an explicit model")
            );
        }

        let created: serde_json::Value = crate::tls::reqwest_client()
            .post(format!("http://{addr}/v1/tasks"))
            .json(&json!({ "prompt": format!("{label} omitted model") }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_eq!(created["model"], expected, "{label} durable task model");

        let stream_client = crate::tls::reqwest_client();
        let mut stream_task = tokio::spawn(async move {
            stream_client
                .post(format!("http://{addr}/v1/stream"))
                .json(&json!({ "prompt": format!("{label} omitted stream model") }))
                .send()
                .await
        });
        let created = tokio::time::timeout(ci_scaled(Duration::from_secs(2)), async {
            tokio::select! {
                created = hook_rx.recv() => created
                    .with_context(|| format!("{label} compatibility stream test hook closed")),
                response = &mut stream_task => {
                    let response = response
                        .with_context(|| format!("{label} compatibility stream HTTP task failed"))?
                        .with_context(|| format!("{label} compatibility stream HTTP request failed"))?;
                    let status = response.status();
                    let body = if status.is_success() {
                        "unexpected successful response".to_string()
                    } else {
                        response.text().await.unwrap_or_default()
                    };
                    bail!("{label} compatibility stream returned HTTP {status} before ThreadCreated: {body}");
                }
            }
        })
            .await
            .with_context(|| format!("{label} compatibility stream made no HTTP/hook progress before thread creation"))??;
        let (thread_id, _resume) = match created {
            CompatStreamTestPoint::ThreadCreated { thread_id, resume } => (thread_id, resume),
            CompatStreamTestPoint::SubscribedBeforeReplay { .. }
            | CompatStreamTestPoint::ReplayLoaded { .. } => {
                bail!("{label} compatibility stream advanced before thread inspection")
            }
        };
        let thread = runtime_threads.get_thread(&thread_id).await?;
        assert_eq!(thread.model, expected, "{label} compatibility stream model");
        stream_task.abort();
        handle.abort();
    }
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn mcp_tools_endpoint_is_passive_until_connect_requested() -> Result<()> {
    let root = std::env::temp_dir().join(format!("codewhale-mcp-tools-api-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    fs::create_dir_all(&root)?;
    let sentinel = root.join("mcp-spawned");
    fs::write(
        root.join("mcp.json"),
        serde_json::json!({
            "servers": {
                "sentinel": {
                    "command": "sh",
                    "args": [
                        "-c",
                        "printf spawned > \"$1\"",
                        "sh",
                        sentinel
                    ]
                }
            }
        })
        .to_string(),
    )?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root(root.clone(), sessions_dir).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let passive: serde_json::Value = client
        .get(format!("http://{addr}/v1/apps/mcp/tools"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(passive["tools"].as_array().map(Vec::len), Some(0));
    assert!(
        !sentinel.exists(),
        "passive MCP tool listing must not spawn stdio servers"
    );

    let _live: serde_json::Value = client
        .get(format!("http://{addr}/v1/apps/mcp/tools?connect=true"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    for _ in 0..20 {
        if sentinel.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        sentinel.exists(),
        "explicit MCP connect should spawn configured stdio servers"
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn runtime_token_guard_protects_v1_routes() -> Result<()> {
    let root = std::env::temp_dir().join(format!("deepseek-runtime-api-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    let token = "local-test-token".to_string();
    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root_and_token(root, sessions_dir, Some(token.clone())).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let health = client
        .get(format!("http://{addr}/health"))
        .send()
        .await?
        .error_for_status()?;
    assert_eq!(health.status(), StatusCode::OK);

    let unauthorized = client
        .get(format!("http://{addr}/v1/threads/summary"))
        .send()
        .await?;
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    let refresh = client
        .post(format!(
            "http://{addr}/v1/providers/codewhale/models/refresh"
        ))
        .send()
        .await?;
    assert_eq!(refresh.status(), StatusCode::UNAUTHORIZED);

    let bearer = client
        .get(format!("http://{addr}/v1/threads/summary"))
        .bearer_auth(&token)
        .send()
        .await?
        .error_for_status()?;
    assert_eq!(bearer.status(), StatusCode::OK);

    let query_token = client
        .get(format!("http://{addr}/v1/threads/summary?token={token}"))
        .send()
        .await?;
    assert_eq!(query_token.status(), StatusCode::UNAUTHORIZED);

    let cookie_token = client
        .get(format!("http://{addr}/v1/threads/summary"))
        .header(header::COOKIE, format!("codewhale_runtime_token={token}"))
        .send()
        .await?;
    assert_eq!(cookie_token.status(), StatusCode::UNAUTHORIZED);

    let codewhale_header = client
        .get(format!("http://{addr}/v1/threads/summary"))
        .header("x-codewhale-runtime-token", &token)
        .send()
        .await?
        .error_for_status()?;
    assert_eq!(codewhale_header.status(), StatusCode::OK);

    let deepseek_header = client
        .get(format!("http://{addr}/v1/threads/summary"))
        .header("x-deepseek-runtime-token", &token)
        .send()
        .await?
        .error_for_status()?;
    assert_eq!(deepseek_header.status(), StatusCode::OK);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn web_bootstrap_sets_strict_cookie_once_and_preserves_v1_auth() -> Result<()> {
    let root = std::env::temp_dir().join(format!("codewhale-web-api-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    let workspace = root.join("workspace");
    let token = "cwrt_runtime_secret_never_in_browser_storage".to_string();
    let (web, nonce) = web::RuntimeWebState::new();
    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root_token_mobile_workspace_and_overrides(
            root,
            sessions_dir,
            Some(token.clone()),
            false,
            workspace,
            TestServerOverrides {
                web: Some(web),
                ..TestServerOverrides::default()
            },
        )
        .await?
    else {
        tracing::warn!("needs socket run: loopback listener is unavailable");
        return Ok(());
    };
    let client = crate::tls::reqwest_client_builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;

    let page = client.get(format!("http://{addr}/")).send().await?;
    assert_eq!(page.status(), StatusCode::OK);
    assert_eq!(
        page.headers()
            .get(header::CONTENT_SECURITY_POLICY)
            .and_then(|value| value.to_str().ok()),
        Some(
            "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'; object-src 'none'"
        )
    );
    let page_body = page.text().await?;
    assert!(!page_body.contains(&token));
    assert!(!page_body.contains(&nonce));

    let icon = client
        .get(format!("http://{addr}/assets/codewhale-192.png"))
        .send()
        .await?;
    assert_eq!(icon.status(), StatusCode::OK);
    assert_eq!(
        icon.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("image/png")
    );
    assert!(icon.bytes().await?.starts_with(b"\x89PNG\r\n\x1a\n"));

    let wrong = client
        .get(format!(
            "http://{addr}/__codewhale/bootstrap/cwwb_00000000000000000000000000000000"
        ))
        .send()
        .await?;
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);

    let exchange = client
        .get(format!("http://{addr}/__codewhale/bootstrap/{nonce}"))
        .send()
        .await?;
    assert_eq!(exchange.status(), StatusCode::SEE_OTHER);
    let proof = exchange
        .headers()
        .get(header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|location| location.strip_prefix("/#p="))
        .context("missing bootstrap request proof")?
        .to_string();
    let set_cookie = exchange
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .context("missing bootstrap Set-Cookie")?
        .to_string();
    assert!(set_cookie.starts_with("codewhale_web_session=cwws_"));
    assert!(set_cookie.ends_with("; HttpOnly; SameSite=Strict; Path=/"));
    assert!(!set_cookie.contains(&token));
    assert!(!set_cookie.contains(&proof));

    let unauthorized = client
        .get(format!("http://{addr}/v1/threads/summary"))
        .send()
        .await?;
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let cookie_pair = set_cookie
        .split(';')
        .next()
        .context("missing web session cookie pair")?;
    for site in ["same-origin", "none"] {
        let reloaded = client
            .get(format!("http://{addr}/"))
            .header(header::COOKIE, cookie_pair)
            .header("sec-fetch-site", site)
            .send()
            .await?;
        assert_eq!(reloaded.headers()[header::CACHE_CONTROL], "no-store");
        assert!(reloaded.text().await?.contains(&format!(
            "name=\"codewhale-web-request\" content=\"{proof}\""
        )));
    }
    for method in [reqwest::Method::GET, reqwest::Method::POST] {
        for metadata in [false, true] {
            for presented in [None, Some("wrong-proof")] {
                let mut request = client
                    .request(method.clone(), format!("http://{addr}/v1/threads"))
                    .header(header::COOKIE, cookie_pair);
                if metadata {
                    request = request
                        .header(header::ORIGIN, format!("http://{addr}"))
                        .header("sec-fetch-site", "same-origin");
                }
                if let Some(proof) = presented {
                    request = request.header(web::WEB_REQUEST_HEADER, proof);
                }
                assert_eq!(request.send().await?.status(), StatusCode::UNAUTHORIZED);
            }
        }
    }
    let authorized = client
        .get(format!("http://{addr}/v1/threads/summary"))
        .header(header::COOKIE, cookie_pair)
        .header(web::WEB_REQUEST_HEADER, &proof)
        .send()
        .await?;
    assert_eq!(authorized.status(), StatusCode::OK);

    let same_origin_cookie_post = client
        .post(format!("http://{addr}/v1/threads"))
        .header(header::COOKIE, cookie_pair)
        .header(web::WEB_REQUEST_HEADER, &proof)
        .header(header::ORIGIN, format!("http://{addr}"))
        .header("sec-fetch-site", "same-origin")
        .json(&json!({}))
        .send()
        .await?;
    assert_eq!(same_origin_cookie_post.status(), StatusCode::CREATED);

    let created: Value = same_origin_cookie_post.json().await?;
    let thread_id = created["id"].as_str().context("thread id")?;
    let ticket_response = client
        .post(format!("http://{addr}/__codewhale/web/stream-ticket"))
        .header(header::COOKIE, cookie_pair)
        .header(web::WEB_REQUEST_HEADER, &proof)
        .header(header::ORIGIN, format!("http://{addr}"))
        .send()
        .await?;
    assert_eq!(ticket_response.status(), StatusCode::OK);
    let ticket: Value = ticket_response.json().await?;
    let ticket = ticket["stream_ticket"].as_str().context("stream ticket")?;
    let stream_url =
        format!("http://{addr}/v1/threads/{thread_id}/events?web_stream_ticket={ticket}");
    for expected in [StatusCode::OK, StatusCode::UNAUTHORIZED] {
        let stream = client
            .get(&stream_url)
            .header(header::COOKIE, cookie_pair)
            .send()
            .await?;
        assert_eq!(stream.status(), expected);
    }

    let cross_origin_cookie_post = client
        .post(format!("http://{addr}/v1/threads"))
        .header(header::COOKIE, cookie_pair)
        .header(web::WEB_REQUEST_HEADER, &proof)
        .header(header::ORIGIN, "http://127.0.0.1:3000")
        .header("sec-fetch-site", "same-site")
        .json(&json!({}))
        .send()
        .await?;
    assert_eq!(cross_origin_cookie_post.status(), StatusCode::UNAUTHORIZED);

    let originless_cookie_post = client
        .post(format!("http://{addr}/v1/threads"))
        .header(header::COOKIE, cookie_pair)
        .json(&json!({}))
        .send()
        .await?;
    assert_eq!(originless_cookie_post.status(), StatusCode::UNAUTHORIZED);

    let bearer_post = client
        .post(format!("http://{addr}/v1/threads"))
        .bearer_auth(&token)
        .header(header::ORIGIN, "http://127.0.0.1:3000")
        .json(&json!({}))
        .send()
        .await?;
    assert_eq!(bearer_post.status(), StatusCode::CREATED);

    let reused = client
        .get(format!("http://{addr}/__codewhale/bootstrap/{nonce}"))
        .send()
        .await?;
    assert_eq!(reused.status(), StatusCode::UNAUTHORIZED);

    let mobile = client.get(format!("http://{addr}/mobile")).send().await?;
    assert_eq!(mobile.status(), StatusCode::NOT_FOUND);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn web_assets_are_absent_outside_web_mode() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    for path in [
        "/",
        "/assets/codewhale-web.css",
        "/assets/codewhale-web.js",
        "/assets/codewhale-192.png",
    ] {
        let response = client.get(format!("http://{addr}{path}")).send().await?;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "path={path}");
    }
    handle.abort();
    Ok(())
}

#[tokio::test]
async fn thread_summary_includes_workspace_branch_metadata() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("runtime");
    let sessions_dir = root.join("sessions");
    let repo = tmp.path().join("repo");
    fs::create_dir_all(&repo)?;
    run_test_git(&repo, &["init", "-b", "feature/agent"])?;
    run_test_git(&repo, &["config", "core.autocrlf", "false"])?;
    fs::write(repo.join("README.md"), "branch visibility\n")?;
    run_test_git(&repo, &["add", "README.md"])?;
    run_test_git(
        &repo,
        &[
            "-c",
            "user.name=CodeWhale Test",
            "-c",
            "user.email=codewhale@example.invalid",
            "commit",
            "-m",
            "init",
        ],
    )?;

    let non_git = tmp.path().join("non-git");
    fs::create_dir_all(&non_git)?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root(root, sessions_dir).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let git_thread: serde_json::Value = client
        .post(format!("http://{addr}/v1/threads"))
        .json(&json!({
            "title": "Git workspace",
            "workspace": repo,
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let git_thread_id = git_thread["id"]
        .as_str()
        .context("missing git thread id")?
        .to_string();
    fs::write(
        repo.join("dirty.txt"),
        "worktree changed after thread spawn\n",
    )?;

    let plain_thread: serde_json::Value = client
        .post(format!("http://{addr}/v1/threads"))
        .json(&json!({
            "title": "Plain workspace",
            "workspace": non_git,
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let plain_thread_id = plain_thread["id"]
        .as_str()
        .context("missing plain thread id")?
        .to_string();

    let summary: serde_json::Value = client
        .get(format!("http://{addr}/v1/threads/summary?limit=100"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let summaries = summary.as_array().context("summary should be an array")?;
    let git_summary = summaries
        .iter()
        .find(|item| item["id"] == git_thread_id)
        .context("missing git workspace summary")?;
    assert_eq!(git_summary["branch"], "feature/agent");
    assert!(
        git_summary["head"]
            .as_str()
            .is_some_and(|head| !head.is_empty())
    );
    assert_eq!(git_summary["dirty"], true);
    assert_eq!(git_summary["workspace"], repo.to_string_lossy().as_ref());

    let plain_summary = summaries
        .iter()
        .find(|item| item["id"] == plain_thread_id)
        .context("missing plain workspace summary")?;
    assert_eq!(plain_summary["branch"], serde_json::Value::Null);
    assert_eq!(plain_summary["head"], serde_json::Value::Null);
    assert_eq!(plain_summary["dirty"], false);
    assert_eq!(
        plain_summary["workspace"],
        non_git.to_string_lossy().as_ref()
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn workspace_file_search_auth_matching_and_bounds() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let workspace = tmp.path().join("workspace");
    fs::create_dir_all(workspace.join("src"))?;
    fs::create_dir_all(workspace.join("match-directory"))?;
    fs::write(
        workspace.join("match.rs"),
        "contents must never be returned",
    )?;
    fs::write(workspace.join("src/match.rs"), "private file contents")?;
    for index in 0..105 {
        fs::write(workspace.join(format!("bounded-{index:03}.rs")), "")?;
    }
    let (addr, _, handle) = spawn_test_server_with_root_token_mobile_workspace(
        tmp.path().join("runtime"),
        tmp.path().join("sessions"),
        Some("file-search-token".to_string()),
        false,
        workspace,
    )
    .await?
    .context("file search test requires a loopback listener")?;
    let client = crate::tls::reqwest_client();
    let url = format!("http://{addr}/v1/workspace/files/search");
    let search_url = |pairs: &[(&str, &str)]| {
        let mut url = reqwest::Url::parse(&url).unwrap();
        url.query_pairs_mut().extend_pairs(pairs.iter().copied());
        url
    };
    for token in [None, Some("wrong-token")] {
        let mut request = client.get(search_url(&[("query", "match")]));
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        assert_eq!(request.send().await?.status(), StatusCode::UNAUTHORIZED);
    }
    for (query, expected) in [
        ("MATCH", json!(["match.rs", "src/match.rs"])),
        ("src/ma", json!(["src/match.rs"])),
        ("", json!([])),
        ("   ", json!([])),
        ("no-such-file", json!([])),
        ("../", json!([])),
        ("/etc/passwd", json!([])),
    ] {
        let body: Value = client
            .get(search_url(&[("query", query)]))
            .bearer_auth("file-search-token")
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_eq!(body, json!({"paths": expected}), "query={query:?}");
    }
    for (limit, expected_count) in [(None, 20), (Some("1"), 1), (Some("100"), 100)] {
        let mut url = search_url(&[("query", "bounded")]);
        if let Some(limit) = limit {
            url.query_pairs_mut().append_pair("limit", limit);
        }
        let body: Value = client
            .get(url)
            .bearer_auth("file-search-token")
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_eq!(body["paths"].as_array().unwrap().len(), expected_count);
    }
    for (key, value) in [
        ("limit", "0".to_string()),
        ("limit", "101".to_string()),
        ("limit", "-1".to_string()),
        ("limit", "abc".to_string()),
        ("query", "é".repeat(129)),
        ("workspace", tmp.path().display().to_string()),
    ] {
        assert_eq!(
            client
                .get(search_url(&[(key, &value)]))
                .bearer_auth("file-search-token")
                .send()
                .await?
                .status(),
            StatusCode::BAD_REQUEST,
            "invalid {key}"
        );
    }
    let missing_query: Value = client
        .get(&url)
        .bearer_auth("file-search-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(missing_query, json!({"paths": []}));
    handle.abort();
    Ok(())
}

#[tokio::test]
async fn workspace_and_automation_endpoints_work() -> Result<()> {
    let root = tempfile::tempdir()?;
    let (automation_tx, automation_rx) = oneshot::channel();
    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root_token_mobile_workspace_and_overrides(
            root.path().to_path_buf(),
            root.path().join("sessions"),
            None,
            false,
            root.path().join("workspace"),
            TestServerOverrides {
                automation_handles: Some(automation_tx),
                ..Default::default()
            },
        )
        .await?
    else {
        return Ok(());
    };
    let (automations, tasks) = automation_rx.await?;
    let client = crate::tls::reqwest_client();

    let workspace: serde_json::Value = client
        .get(format!("http://{addr}/v1/workspace/status"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert!(workspace.get("workspace").is_some());

    let created: serde_json::Value = client
        .post(format!("http://{addr}/v1/automations"))
        .json(&json!({
            "name": "Smoke automation",
            "prompt": "automation smoke test",
            "rrule": "FREQ=HOURLY;INTERVAL=2",
            "status": "active"
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let automation_id = created["id"]
        .as_str()
        .context("missing automation id")?
        .to_string();

    let listed: serde_json::Value = client
        .get(format!("http://{addr}/v1/automations"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert!(
        listed
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["id"] == automation_id))
    );

    let run_now: serde_json::Value = client
        .post(format!("http://{addr}/v1/automations/{automation_id}/run"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(run_now["automation_id"], automation_id);

    let paused: serde_json::Value = client
        .post(format!(
            "http://{addr}/v1/automations/{automation_id}/pause"
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(paused["status"], "paused");

    let resumed: serde_json::Value = client
        .post(format!(
            "http://{addr}/v1/automations/{automation_id}/resume"
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(resumed["status"], "active");

    let updated: serde_json::Value = client
        .patch(format!("http://{addr}/v1/automations/{automation_id}"))
        .json(&json!({
            "name": "Smoke automation edited",
            "rrule": "FREQ=WEEKLY;BYDAY=MO,WE;BYHOUR=10;BYMINUTE=15"
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(updated["name"], "Smoke automation edited");

    let runs: serde_json::Value = client
        .get(format!(
            "http://{addr}/v1/automations/{automation_id}/runs?limit=5"
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert!(
        runs.as_array().is_some_and(|items| !items.is_empty()),
        "expected at least one run entry"
    );

    // The admitted occurrence must survive deletion until its real task and
    // the production scheduler have settled its durable run receipt.
    let refused = client
        .delete(format!("http://{addr}/v1/automations/{automation_id}"))
        .send()
        .await?;
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    assert!(refused.text().await?.contains("still active"));
    let task_id = run_now["task_id"].as_str().context("missing task id")?;
    let run_id = run_now["id"].as_str().context("missing run id")?;
    let task = crate::task_manager::wait_for_terminal_state(
        &tasks,
        task_id,
        ci_scaled(Duration::from_secs(15)),
    )
    .await?;
    assert_eq!(task.status, crate::task_manager::TaskStatus::Completed);
    let cancel = tokio_util::sync::CancellationToken::new();
    let _cancel_on_drop = cancel.clone().drop_guard();
    let scheduler = spawn_scheduler(
        automations.clone(),
        tasks,
        cancel.clone(),
        crate::automation_manager::AutomationSchedulerConfig::default(),
    );
    tokio::time::timeout(ci_scaled(Duration::from_secs(15)), async {
        loop {
            let runs = automations.lock().await.list_runs(&automation_id, None)?;
            if runs.iter().any(|run| {
                run.id == run_id
                    && run.status == crate::automation_manager::AutomationRunStatus::Completed
            }) {
                return Ok::<_, anyhow::Error>(());
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .context("automation scheduler did not settle the completed task")??;
    cancel.cancel();
    scheduler.await?;

    let _deleted: serde_json::Value = client
        .delete(format!("http://{addr}/v1/automations/{automation_id}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let missing_status = client
        .get(format!("http://{addr}/v1/automations/{automation_id}"))
        .send()
        .await?
        .status();
    assert_eq!(missing_status, StatusCode::NOT_FOUND);
    let archived = automations
        .lock()
        .await
        .list_archived_runs(&automation_id)?;
    assert_eq!(archived.len(), 1);
    assert_eq!(archived[0].task_id.as_deref(), Some(task_id));

    handle.abort();
    Ok(())
}

fn test_fleet_route_config() -> crate::config::Config {
    let mut providers = crate::config::ProvidersConfig::default();
    providers.deepseek.api_key = Some("test-key".to_string());
    providers.xai.api_key = Some("test-key".to_string());
    providers.zai.api_key = Some("test-key".to_string());
    crate::config::Config {
        provider: Some("deepseek".to_string()),
        providers: Some(providers),
        ..crate::config::Config::default()
    }
    .with_legacy_root(Some("test-key".to_string()), None)
}

#[tokio::test]
async fn fleet_status_runtime_api_exposes_state_and_actions() -> Result<()> {
    let root = std::env::temp_dir().join(format!("codewhale-fleet-api-{}", Uuid::new_v4()));
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace)?;
    let sub_agent_manager = runtime_api_sub_agent_manager(&workspace, 2);
    let manager = FleetManager::open(&workspace)?
        .with_sub_agent_manager(sub_agent_manager.clone())
        .with_session_model(DEFAULT_TEXT_MODEL)
        .with_route_config(test_fleet_route_config());
    let task = codewhale_protocol::fleet::FleetTaskSpec {
        id: "task-a".to_string(),
        name: "Task A".to_string(),
        description: None,
        objective: Some("Inspect fleet status through Runtime API".to_string()),
        instructions: "Stay running for inspection.".to_string(),
        worker: Some(codewhale_protocol::fleet::FleetTaskWorkerProfile {
            agent_profile: None,
            role: Some("reviewer".to_string()),
            loadout: None,
            model_class: None,
            model: None,
            tool_profile: Some("read-only".to_string()),
            tools: vec!["rg".to_string()],
            capabilities: vec!["fleet".to_string()],
        }),
        workspace: None,
        input_files: Vec::new(),
        context: Vec::new(),
        budget: None,
        tags: Vec::new(),
        expected_artifacts: vec![FleetArtifactKind::Log],
        scorer: None,
        retry_policy: None,
        alert_policy: None,
        timeout_seconds: None,
        metadata: std::collections::BTreeMap::new(),
    };
    let report = manager.create_run(
        crate::fleet::task_spec::FleetTaskSpecDocument {
            name: Some("api smoke".to_string()),
            labels: std::collections::BTreeMap::new(),
            security_policy: None,
            workers: Vec::new(),
            tasks: vec![task],
            usage_ceiling: None,
        },
        1,
    )?;
    let restarted_marker = root.join("restarted-worker-ran");
    let fake_codewhale = write_fake_fleet_binary(&root, &restarted_marker)?;
    let worker_id = report.worker_ids[0].clone();
    let sessions_dir = root.join("sessions");
    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root_token_mobile_workspace_and_subagents(
            root.clone(),
            sessions_dir,
            None,
            false,
            workspace,
            Some(sub_agent_manager),
            Some(fake_codewhale.display().to_string()),
        )
        .await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let runs: serde_json::Value = client
        .get(format!("http://{addr}/v1/fleet/runs"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(runs["status"]["running"], 1);
    assert_eq!(runs["runs"][0]["id"], report.run_id.0);

    let profiles: serde_json::Value = client
        .get(format!("http://{addr}/v1/fleet/profiles"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let profile_ids: Vec<&str> = profiles["profiles"]
        .as_array()
        .expect("profiles array")
        .iter()
        .map(|member| member["id"].as_str().expect("profile id"))
        .collect();
    assert!(
        profile_ids.contains(&"manager"),
        "built-in roster member 'manager' missing from /v1/fleet/profiles: {profile_ids:?}"
    );
    assert!(
        profiles["profiles"]
            .as_array()
            .expect("profiles array")
            .iter()
            .all(|member| member["origin"].is_string()),
        "every profile must carry its roster origin"
    );

    let worker: serde_json::Value = client
        .get(format!("http://{addr}/v1/fleet/workers/{worker_id}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        worker["objective"],
        "Inspect fleet status through Runtime API"
    );
    assert_eq!(worker["role"], "reviewer");
    assert_eq!(worker["host"], "local");
    assert_eq!(worker["artifacts"][0]["kind"], "log");
    assert_eq!(worker["runtime_state"]["agent_status"], "starting");
    assert_eq!(worker["runtime_state"]["steps_taken"], 0);
    assert_eq!(worker["runtime_state"]["has_session"], true);

    let interrupted: serde_json::Value = client
        .post(format!(
            "http://{addr}/v1/fleet/workers/{worker_id}/interrupt"
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(interrupted["action"], "interrupt");
    assert_eq!(interrupted["worker"]["last_error"], "cancelled by operator");

    let restarted: serde_json::Value = client
        .post(format!(
            "http://{addr}/v1/fleet/workers/{worker_id}/restart"
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(restarted["action"], "restart");
    assert_eq!(restarted["execution"], "scheduled");
    assert_eq!(restarted["worker"]["status"], "busy");

    let terminal_status = tokio::time::timeout(ci_scaled(Duration::from_secs(15)), async {
        loop {
            let status = manager.run_status(&report.run_id).unwrap();
            if status.queued == 0 && status.running == 0 {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .context("Runtime API restart never drove the replacement attempt to completion")?;
    assert_eq!(
        terminal_status.completed, 1,
        "replacement attempt did not complete successfully: {terminal_status:?}"
    );
    assert_eq!(
        terminal_status.failed, 0,
        "replacement attempt failed: {terminal_status:?}"
    );
    assert!(
        restarted_marker.is_file(),
        "Runtime API reported a restart without launching its Fleet worker"
    );
    let ledger_state = manager.rebuild_state()?;
    let restarted_task = ledger_state
        .tasks
        .values()
        .find(|task| task.entry.run_id == report.run_id && task.entry.task_id == "task-a")
        .context("missing restarted task")?;
    assert_eq!(restarted_task.entry.attempts, 2);
    assert_eq!(restarted_task.status, FleetTaskLedgerStatus::Completed);
    let receipt = ledger_state
        .receipts
        .values()
        .find(|receipt| receipt.run_id == report.run_id && receipt.task_id == "task-a")
        .context("missing restarted receipt")?;
    assert_eq!(receipt.attempt, Some(2));
    assert!(receipt.terminal_seq.is_some());

    let stopped: serde_json::Value = client
        .post(format!(
            "http://{addr}/v1/fleet/runs/{}/stop",
            report.run_id.0
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(stopped["action"], "stop");
    assert_eq!(stopped["stopped"], 0);
    assert_eq!(stopped["status"]["completed"], 1);

    handle.abort();
    Ok(())
}

/// A Parallel Workflow task may claim the whole workspace by normalizing its
/// write scope to `"."`. `managed_paths_overlap` compared normalized strings
/// only, so `"."` never matched a sibling task's `"crates/tui"` claim and the
/// admission gate let two workers write the same tree concurrently.
#[test]
fn workspace_root_write_claim_collides_with_a_subdirectory_claim() {
    fn task(id: &str, writable: &str) -> FleetTaskSpec {
        serde_json::from_value(json!({
            "id": id,
            "name": id,
            "instructions": "work",
            "workspace": { "writable_paths": [writable] },
        }))
        .expect("fleet task spec fixture")
    }

    let error = reject_parallel_write_collisions(&[task("root", "."), task("sub", "crates/tui")])
        .expect_err("a whole-workspace claim overlaps every subdirectory claim");
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(
        error.message.contains("write scope collision"),
        "unexpected message: {}",
        error.message
    );

    // The reverse order is the same collision.
    assert!(
        reject_parallel_write_collisions(&[task("sub", "crates/tui"), task("root", ".")]).is_err()
    );
    // Disjoint subdirectories still admit.
    assert!(
        reject_parallel_write_collisions(&[task("a", "crates/tui"), task("b", "crates/cli")])
            .is_ok()
    );
}

#[test]
fn fleet_worker_json_includes_runtime_state_projection() {
    let inspection = FleetWorkerInspection {
        worker_id: "fleet-worker-1".to_string(),
        status: FleetWorkerStatus::Busy,
        current_run_id: Some(FleetRunId::from("fleet-run-1")),
        current_task_id: Some("task-a".to_string()),
        objective: Some("Inspect runtime projection".to_string()),
        role: Some("reviewer".to_string()),
        host: Some("local".to_string()),
        latest_heartbeat_at: None,
        latest_event: None,
        artifacts: Vec::new(),
        receipt_summary: None,
        last_error: None,
        alert_state: None,
        runtime_state: Some(FleetWorkerRuntimeProjection {
            agent_status: "running".to_string(),
            steps_taken: 3,
            latest_message: Some("reading files".to_string()),
            error: None,
            result_summary: None,
            has_session: true,
        }),
    };

    let worker = fleet_worker_json(&inspection);

    assert_eq!(worker["runtime_state"]["agent_status"], "running");
    assert_eq!(worker["runtime_state"]["steps_taken"], 3);
    assert_eq!(worker["runtime_state"]["latest_message"], "reading files");
    assert_eq!(worker["runtime_state"]["has_session"], true);
}

#[tokio::test]
async fn agent_runs_runtime_api_exposes_persisted_worker_receipts() -> Result<()> {
    use crate::tools::subagent::{
        AgentRunArtifactRef, AgentRunFollowUpTarget, AgentRunRecommendedAction,
        AgentRunTakeoverTarget, AgentRunUsage, AgentRunVerificationSummary, AgentWorkerEvent,
        AgentWorkerRecord, AgentWorkerSpec, AgentWorkerStatus, AgentWorkerToolProfile, FleetRole,
    };
    use crate::worker_profile::{ModelRoute, ToolScope, WorkerRuntimeProfile};
    use std::collections::VecDeque;

    let root = std::env::temp_dir().join(format!("codewhale-agent-runs-api-{}", Uuid::new_v4()));
    let workspace = root.join("workspace");
    fs::create_dir_all(workspace.join(".codewhale/state"))?;

    let record = AgentWorkerRecord {
        spec: AgentWorkerSpec {
            worker_id: "agent_receipt".to_string(),
            run_id: "run_receipt".to_string(),
            parent_run_id: Some("parent_run".to_string()),
            workflow_run_id: None,
            session_name: Some("receipt_lane".to_string()),
            objective: "Verify run receipt projection".to_string(),
            role: Some("verifier".to_string()),
            agent_type: FleetRole::Verifier,
            model: "deepseek-v4-flash".to_string(),
            workspace: workspace.clone(),
            git_branch: Some("codex/v0.8.60".to_string()),
            context_mode: "fresh".to_string(),
            fork_context: false,
            tool_profile: AgentWorkerToolProfile::Explicit(vec!["read_file".to_string()]),
            runtime_profile: {
                let mut profile = WorkerRuntimeProfile::for_role(FleetRole::Verifier);
                profile.tools = ToolScope::Explicit(vec!["read_file".to_string()]);
                profile.model = ModelRoute::Fixed("deepseek-v4-flash".to_string());
                profile.max_spawn_depth = crate::tools::subagent::DEFAULT_MAX_SPAWN_DEPTH;
                profile.spawn_depth = 1;
                profile
            },
            max_steps: 4,
            spawn_depth: 1,
            max_spawn_depth: crate::tools::subagent::DEFAULT_MAX_SPAWN_DEPTH,
            child_route: None,
            launch_manifest: None,
        },
        owner_session_id: "session-receipt".to_string(),
        actor_kind: "subagent".to_string(),
        parent_run_id: Some("parent_run".to_string()),
        follow_up: AgentRunFollowUpTarget {
            tool: "handle_read".to_string(),
            agent_id: "agent_receipt".to_string(),
            session_name: Some("receipt_lane".to_string()),
            accepted_statuses: vec!["running".to_string(), "interrupted_continuable".to_string()],
            latest_delivery: None,
        },
        takeover: AgentRunTakeoverTarget {
            kind: "local_subagent_session".to_string(),
            supported: true,
            agent_id: "agent_receipt".to_string(),
            session_name: Some("receipt_lane".to_string()),
            instructions: "Use handle_read on the transcript_handle for agent_receipt.".to_string(),
            unsupported_reason: None,
        },
        artifacts: vec![AgentRunArtifactRef {
            kind: "transcript".to_string(),
            name: "transcript_handle".to_string(),
            target: "agent:agent_receipt".to_string(),
            description: "Read with handle_read from a live projection.".to_string(),
        }],
        usage: AgentRunUsage {
            status: "unknown".to_string(),
            input_tokens: None,
            output_tokens: None,
            total_tokens: None,
            cost_microusd: None,
            note: "not reported".to_string(),
        },
        usage_source_fingerprints: Default::default(),
        missing_usage_sources: Default::default(),
        missing_usage_overflowed: false,
        legacy_unreported_usage: false,
        has_unreported_usage: false,
        delivery_evidence: Default::default(),
        verification: AgentRunVerificationSummary {
            status: "self_report_only".to_string(),
            summary: "no verified receipt attached".to_string(),
            deliverables: Vec::new(),
        },
        recommended_action: AgentRunRecommendedAction {
            action: "verify_self_report".to_string(),
            tool: Some("handle_read".to_string()),
            reason: "Worker agent_receipt completed; verify its self-report.".to_string(),
        },
        pending_request: None,
        status: AgentWorkerStatus::Completed,
        created_at_ms: 1,
        updated_at_ms: 2,
        started_at_ms: Some(1),
        completed_at_ms: Some(2),
        latest_message: Some("completed".to_string()),
        result_summary: Some("receipt complete".to_string()),
        error: None,
        steps_taken: 2,
        parked_at_turn_end: false,
        events: VecDeque::from([AgentWorkerEvent {
            seq: 1,
            worker_id: "agent_receipt".to_string(),
            status: AgentWorkerStatus::Completed,
            timestamp_ms: 2,
            message: Some("completed".to_string()),
            step: Some(2),
            tool_name: None,
        }]),
    };
    let state_payload = json!({
        "schema_version": 1,
        "agents": [],
        "workers": [record],
    });
    fs::write(
        workspace.join(".codewhale/state/subagents.v1.json"),
        serde_json::to_vec_pretty(&state_payload)?,
    )?;

    let sessions_dir = root.join("sessions");
    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root_token_mobile_workspace(
            root.clone(),
            sessions_dir,
            None,
            false,
            workspace,
        )
        .await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let runs: serde_json::Value = client
        .get(format!("http://{addr}/v1/agent-runs"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(runs["runs"][0]["spec"]["run_id"], "run_receipt");
    // F5: the payload carries the launch governor; a calm fleet has no line.
    assert_eq!(runs["governor"]["paused"], false);
    assert_eq!(runs["governor"]["recent_rate_limits"], 0);
    assert_eq!(
        runs["governor"]["launch_slots"],
        runs["governor"]["max_launch_slots"]
    );
    assert!(runs["governor"].get("status").is_none());
    assert_eq!(runs["runs"][0]["follow_up"]["tool"], "handle_read");
    assert_eq!(
        runs["runs"][0]["verification"]["status"],
        "self_report_only"
    );

    let run: serde_json::Value = client
        .get(format!("http://{addr}/v1/agent-runs/run_receipt"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(run["spec"]["worker_id"], "agent_receipt");
    assert_eq!(run["takeover"]["supported"], true);
    assert_eq!(run["artifacts"][0]["kind"], "transcript");

    let missing = client
        .get(format!("http://{addr}/v1/agent-runs/missing"))
        .send()
        .await?
        .status();
    assert_eq!(missing, StatusCode::NOT_FOUND);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn agent_run_cancel_stops_a_live_child_and_returns_its_receipt() -> Result<()> {
    let root = std::env::temp_dir().join(format!("codewhale-agent-run-cancel-{}", Uuid::new_v4()));
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace)?;
    let manager = crate::tools::subagent::new_shared_subagent_manager(workspace.clone(), 2);
    let agent_id = {
        let mut guard = manager.write().await;
        let id = guard.insert_test_running_agent("stoppable", &workspace);
        guard.assign_test_session_owner(&id, "session-stop");
        id
    };

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root_token_mobile_workspace_and_subagents(
            root.clone(),
            root.join("sessions"),
            None,
            false,
            workspace,
            Some(manager.clone()),
            None,
        )
        .await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let response = client
        .post(format!("http://{addr}/v1/agent-runs/{agent_id}/cancel"))
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let receipt: serde_json::Value = response.json().await?;
    assert_eq!(receipt["spec"]["worker_id"], agent_id.as_str());
    assert_eq!(receipt["status"], "cancelled");
    assert_eq!(
        manager.read().await.get_result(&agent_id)?.status,
        crate::tools::subagent::SubAgentStatus::Cancelled
    );

    // Stopping a stopped run is a no-op that answers with the same receipt.
    let again = client
        .post(format!("http://{addr}/v1/agent-runs/{agent_id}/cancel"))
        .send()
        .await?;
    assert_eq!(again.status(), StatusCode::OK);
    let again: serde_json::Value = again.json().await?;
    assert_eq!(again["status"], "cancelled");

    let missing = client
        .post(format!("http://{addr}/v1/agent-runs/missing/cancel"))
        .send()
        .await?
        .status();
    assert_eq!(missing, StatusCode::NOT_FOUND);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn agent_run_cancel_remains_idempotent_before_receipt_persistence() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().to_path_buf();
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace)?;
    // A memory-only manager deterministically models a disk projection that
    // has not caught up. Repeated stops must use its authoritative receipt.
    let manager = Arc::new(tokio::sync::RwLock::new(
        crate::tools::subagent::SubAgentManager::new(workspace.clone(), 2),
    ));
    let agent_id = {
        let mut guard = manager.write().await;
        let id = guard.insert_test_running_agent("not-yet-persisted", &workspace);
        guard.assign_test_session_owner(&id, "session-stop");
        id
    };
    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root_token_mobile_workspace_and_subagents(
            root.clone(),
            root.join("sessions"),
            None,
            false,
            workspace.clone(),
            Some(manager),
            None,
        )
        .await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    for _ in 0..2 {
        let response = client
            .post(format!("http://{addr}/v1/agent-runs/{agent_id}/cancel"))
            .send()
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let receipt: serde_json::Value = response.json().await?;
        assert_eq!(receipt["spec"]["worker_id"], agent_id.as_str());
        assert_eq!(receipt["status"], "cancelled");
    }
    assert!(
        !workspace
            .join(".codewhale/state/subagents.v1.json")
            .exists()
    );
    handle.abort();
    Ok(())
}

#[tokio::test]
async fn agent_run_cancel_refuses_a_run_owned_by_a_session_it_does_not_host() -> Result<()> {
    let root = std::env::temp_dir().join(format!(
        "codewhale-agent-run-cancel-foreign-{}",
        Uuid::new_v4()
    ));
    let workspace = root.join("workspace");
    fs::create_dir_all(workspace.join(".codewhale/state"))?;
    // A child parked on a question in another terminal session: in flight on
    // disk, but no engine in this runtime owns it.
    let mut record = {
        let manager = crate::tools::subagent::new_shared_subagent_manager(workspace.clone(), 1);
        let mut guard = manager.write().await;
        let id = guard.insert_test_running_agent("elsewhere", &workspace);
        guard.assign_test_session_owner(&id, "terminal-session");
        guard
            .list_worker_records()
            .into_iter()
            .find(|record| record.spec.worker_id == id)
            .expect("seeded record")
    };
    record.status = crate::tools::subagent::AgentWorkerStatus::WaitingForUser;
    fs::write(
        workspace.join(".codewhale/state/subagents.v1.json"),
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "agents": [],
            "workers": [record],
        }))?,
    )?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root_token_mobile_workspace(
            root.clone(),
            root.join("sessions"),
            None,
            false,
            workspace,
        )
        .await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let response = client
        .post(format!(
            "http://{addr}/v1/agent-runs/agent_elsewhere/cancel"
        ))
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = response.text().await?;
    assert!(body.contains("not hosting"), "{body}");

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn stream_requires_prompt() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let resp = client
        .post(format!("http://{addr}/v1/stream"))
        .json(&json!({ "prompt": "" }))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    handle.abort();
    Ok(())
}

#[tokio::test]
async fn compatibility_stream_closes_losslessly_across_replay_live_handoff() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("server");
    let sessions_dir = root.join("sessions");
    let workspace = root.join("workspace");
    let (hook_tx, mut hook_rx) = mpsc::unbounded_channel();
    let Some((addr, runtime_threads, handle)) =
        spawn_test_server_with_root_token_mobile_workspace_and_overrides(
            root,
            sessions_dir,
            None,
            false,
            workspace,
            TestServerOverrides {
                compat_stream_test_hook: Some(hook_tx),
                ..TestServerOverrides::default()
            },
        )
        .await?
    else {
        return Ok(());
    };

    let client = crate::tls::reqwest_client();
    let stream_client = client.clone();
    let stream_task = tokio::spawn(async move {
        let response = stream_client
            .post(format!("http://{addr}/v1/stream"))
            .json(&json!({ "prompt": "cross the replay handoff" }))
            .send()
            .await?
            .error_for_status()?;
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let body = response.text().await?;
        Ok::<_, anyhow::Error>((content_type, body))
    });

    let created = tokio::time::timeout(ci_scaled(Duration::from_secs(2)), hook_rx.recv())
        .await
        .context("compatibility stream did not create its thread")?
        .context("compatibility stream test hook closed")?;
    let (thread_id, resume_created) = match created {
        CompatStreamTestPoint::ThreadCreated { thread_id, resume } => (thread_id, resume),
        CompatStreamTestPoint::SubscribedBeforeReplay { .. }
        | CompatStreamTestPoint::ReplayLoaded { .. } => {
            bail!("compatibility stream loaded replay before its thread was prepared")
        }
    };

    let harness = crate::core::engine::mock_engine_handle();
    runtime_threads
        .install_test_engine(&thread_id, harness.handle.clone())
        .await?;
    let mut rx_op = harness.rx_op;
    let tx_event = harness.tx_event;
    let (release_overlap, wait_for_overlap_release) = oneshot::channel();
    let (release_terminal, wait_for_terminal_release) = oneshot::channel();
    let engine_task = tokio::spawn(async move {
        if !matches!(rx_op.recv().await, Some(Op::SendMessage(TurnSpec { .. }))) {
            return;
        }
        let _ = wait_for_overlap_release.await;
        let _ = tx_event
            .send(EngineEvent::TurnStarted {
                turn_id: "mock_compat_handoff".to_string(),
                created_at: chrono::Utc::now(),
                route: None,
                submission_id: None,
            })
            .await;
        let _ = tx_event
            .send(EngineEvent::MessageStarted { index: 0 })
            .await;
        let _ = tx_event
            .send(EngineEvent::MessageDelta {
                index: 0,
                content: "handoff".to_string(),
            })
            .await;
        let _ = wait_for_terminal_release.await;
        let _ = tx_event
            .send(EngineEvent::MessageComplete { index: 0 })
            .await;
        let _ = tx_event
            .send(EngineEvent::TurnComplete {
                usage: Usage {
                    input_tokens: 3,
                    output_tokens: 1,
                    ..Usage::default()
                },
                parent_route_usage: Usage {
                    input_tokens: 3,
                    output_tokens: 1,
                    ..Usage::default()
                },
                routed_usage_dropped_records: 0,
                status: TurnOutcomeStatus::Completed,
                error: None,
                tool_catalog: None,
                base_url: None,
            })
            .await;
    });
    resume_created
        .send(())
        .map_err(|_| anyhow::anyhow!("compatibility stream dropped thread-create handoff"))?;

    let subscribed = tokio::time::timeout(ci_scaled(Duration::from_secs(2)), hook_rx.recv())
        .await
        .context("compatibility stream did not subscribe before replay")?
        .context("compatibility stream test hook closed")?;
    let (subscribed_thread_id, subscribed_turn_id, resume_subscribed) = match subscribed {
        CompatStreamTestPoint::SubscribedBeforeReplay {
            thread_id,
            turn_id,
            resume,
        } => (thread_id, turn_id, resume),
        CompatStreamTestPoint::ThreadCreated { .. }
        | CompatStreamTestPoint::ReplayLoaded { .. } => {
            bail!("compatibility stream did not expose its subscribe-before-replay boundary")
        }
    };
    assert_eq!(subscribed_thread_id, thread_id);

    release_overlap
        .send(())
        .map_err(|_| anyhow::anyhow!("mock engine dropped overlap release"))?;
    tokio::time::timeout(ci_scaled(Duration::from_secs(2)), async {
        loop {
            if runtime_threads
                .events_since(&thread_id, None)
                .is_ok_and(|events| {
                    events.iter().any(|event| {
                        event.turn_id.as_deref() == Some(&subscribed_turn_id)
                            && event.event == "item.delta"
                    })
                })
            {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("overlap event was not persisted before compatibility replay")?;
    resume_subscribed
        .send(())
        .map_err(|_| anyhow::anyhow!("compatibility stream dropped subscribe handoff"))?;

    let replay_loaded = tokio::time::timeout(ci_scaled(Duration::from_secs(2)), hook_rx.recv())
        .await
        .context("compatibility stream did not reach its replay/live handoff")?
        .context("compatibility stream test hook closed")?;
    let (replay_thread_id, turn_id, resume_replay) = match replay_loaded {
        CompatStreamTestPoint::ReplayLoaded {
            thread_id,
            turn_id,
            resume,
        } => (thread_id, turn_id, resume),
        CompatStreamTestPoint::ThreadCreated { .. }
        | CompatStreamTestPoint::SubscribedBeforeReplay { .. } => {
            bail!("compatibility stream created more than one thread")
        }
    };
    assert_eq!(replay_thread_id, thread_id);
    assert_eq!(turn_id, subscribed_turn_id);

    release_terminal
        .send(())
        .map_err(|_| anyhow::anyhow!("mock engine dropped terminal release"))?;
    tokio::time::timeout(ci_scaled(Duration::from_secs(2)), async {
        loop {
            if runtime_threads
                .events_since(&thread_id, None)
                .is_ok_and(|events| {
                    events.iter().any(|event| {
                        event.turn_id.as_deref() == Some(&turn_id)
                            && event.event == "turn.completed"
                    })
                })
            {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("terminal event was not persisted during compatibility handoff")?;
    resume_replay
        .send(())
        .map_err(|_| anyhow::anyhow!("compatibility stream dropped replay handoff"))?;

    let (content_type, body) = tokio::time::timeout(ci_scaled(Duration::from_secs(3)), stream_task)
        .await
        .context("compatibility stream hung after its terminal event")?
        .context("compatibility stream request task panicked")??;
    engine_task.await.context("mock engine task panicked")?;

    assert!(content_type.starts_with("text/event-stream"));
    assert_eq!(body.matches("event: message.delta").count(), 1, "{body}");
    assert_eq!(body.matches("event: turn.completed").count(), 1, "{body}");
    assert_eq!(body.matches("event: done").count(), 1, "{body}");

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn compatibility_stream_exposes_and_resolves_user_input_without_answer_echo() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("server");
    let sessions_dir = root.join("sessions");
    let workspace = root.join("workspace");
    let (hook_tx, mut hook_rx) = mpsc::unbounded_channel();
    let Some((addr, runtime_threads, handle)) =
        spawn_test_server_with_root_token_mobile_workspace_and_overrides(
            root.clone(),
            sessions_dir,
            None,
            false,
            workspace,
            TestServerOverrides {
                compat_stream_test_hook: Some(hook_tx),
                ..TestServerOverrides::default()
            },
        )
        .await?
    else {
        return Ok(());
    };

    let client = crate::tls::reqwest_client();
    let stream_client = client.clone();
    let request_task = tokio::spawn(async move {
        let response = stream_client
            .post(format!("http://{addr}/v1/stream"))
            .json(&json!({ "prompt": "ask before continuing" }))
            .send()
            .await?
            .error_for_status()?;
        Ok::<_, anyhow::Error>(response)
    });

    let created = tokio::time::timeout(ci_scaled(Duration::from_secs(2)), hook_rx.recv())
        .await
        .context("compatibility stream did not create its interaction thread")?
        .context("compatibility stream interaction hook closed")?;
    let (thread_id, resume_created) = match created {
        CompatStreamTestPoint::ThreadCreated { thread_id, resume } => (thread_id, resume),
        CompatStreamTestPoint::SubscribedBeforeReplay { .. }
        | CompatStreamTestPoint::ReplayLoaded { .. } => {
            bail!("compatibility interaction stream advanced before engine installation")
        }
    };

    let mut harness = crate::core::engine::mock_engine_handle();
    runtime_threads
        .install_test_engine(&thread_id, harness.handle.clone())
        .await?;
    let (submission_tx, submission_rx) = oneshot::channel();
    let (release_completion, wait_for_completion_release) = oneshot::channel();
    let engine_task = tokio::spawn(async move {
        if !matches!(
            harness.rx_op.recv().await,
            Some(Op::SendMessage(TurnSpec { .. }))
        ) {
            bail!("compatibility interaction engine did not receive a prompt");
        }
        harness
            .tx_event
            .send(EngineEvent::TurnStarted {
                turn_id: "mock_compat_input".to_string(),
                created_at: chrono::Utc::now(),
                route: None,
                submission_id: None,
            })
            .await?;
        let request = crate::tools::user_input::UserInputRequest {
            questions: vec![crate::tools::user_input::UserInputQuestion {
                header: "Continue".to_string(),
                id: "choice".to_string(),
                question: "Continue the compatibility turn?".to_string(),
                options: vec![
                    crate::tools::user_input::UserInputOption {
                        label: "Continue".to_string(),
                        description: "Finish the turn".to_string(),
                    },
                    crate::tools::user_input::UserInputOption {
                        label: "Stop".to_string(),
                        description: "Cancel the turn".to_string(),
                    },
                ],
                allow_free_text: false,
                multi_select: false,
            }],
        };
        harness
            .tx_event
            .send(EngineEvent::ToolCallStarted {
                model_call: Some(crate::core::events::ModelToolCall {
                    provider_id: "input_compat".to_string(),
                    caller: None,
                    thought_signature: None,
                }),
                id: "input_compat".to_string(),
                name: "request_user_input".to_string(),
                input: serde_json::to_value(&request)?,
            })
            .await?;
        harness
            .tx_event
            .send(EngineEvent::UserInputRequired {
                id: "input_compat".to_string(),
                request,
            })
            .await?;
        let submission = harness.recv_user_input_submission().await;
        let tool_result = submission
            .as_ref()
            .map(|(_, response)| crate::tools::spec::ToolResult::json(response))
            .transpose()?
            .context("compatibility user input was canceled before tool completion")?;
        let _ = submission_tx.send(submission);
        wait_for_completion_release
            .await
            .context("compatibility interaction test dropped completion release")?;
        harness
            .tx_event
            .send(EngineEvent::ToolCallComplete {
                model_call: None,
                id: "input_compat".to_string(),
                name: "request_user_input".to_string(),
                result: Ok(tool_result),
            })
            .await?;
        harness
            .tx_event
            .send(EngineEvent::MessageStarted { index: 0 })
            .await?;
        harness
            .tx_event
            .send(EngineEvent::MessageDelta {
                index: 0,
                content: "continued".to_string(),
            })
            .await?;
        harness
            .tx_event
            .send(EngineEvent::MessageComplete { index: 0 })
            .await?;
        harness
            .tx_event
            .send(EngineEvent::TurnComplete {
                usage: Usage::default(),
                parent_route_usage: Usage::default(),
                routed_usage_dropped_records: 0,
                status: TurnOutcomeStatus::Completed,
                error: None,
                tool_catalog: None,
                base_url: None,
            })
            .await?;
        Ok::<_, anyhow::Error>(())
    });
    resume_created
        .send(())
        .map_err(|_| anyhow::anyhow!("compatibility interaction stream dropped create hook"))?;

    let subscribed = tokio::time::timeout(ci_scaled(Duration::from_secs(2)), hook_rx.recv())
        .await
        .context("compatibility interaction stream did not subscribe")?
        .context("compatibility stream interaction hook closed")?;
    let (subscribed_thread_id, turn_id, resume_subscribed) = match subscribed {
        CompatStreamTestPoint::SubscribedBeforeReplay {
            thread_id,
            turn_id,
            resume,
        } => (thread_id, turn_id, resume),
        CompatStreamTestPoint::ThreadCreated { .. }
        | CompatStreamTestPoint::ReplayLoaded { .. } => {
            bail!("compatibility interaction stream missed subscribe-before-replay hook")
        }
    };
    assert_eq!(subscribed_thread_id, thread_id);
    resume_subscribed
        .send(())
        .map_err(|_| anyhow::anyhow!("compatibility interaction stream dropped subscribe hook"))?;

    let replay_loaded = tokio::time::timeout(ci_scaled(Duration::from_secs(2)), hook_rx.recv())
        .await
        .context("compatibility interaction stream did not load replay")?
        .context("compatibility stream interaction hook closed")?;
    let (replay_thread_id, replay_turn_id, resume_replay) = match replay_loaded {
        CompatStreamTestPoint::ReplayLoaded {
            thread_id,
            turn_id,
            resume,
        } => (thread_id, turn_id, resume),
        CompatStreamTestPoint::ThreadCreated { .. }
        | CompatStreamTestPoint::SubscribedBeforeReplay { .. } => {
            bail!("compatibility interaction stream missed replay-loaded hook")
        }
    };
    assert_eq!(replay_thread_id, thread_id);
    assert_eq!(replay_turn_id, turn_id);
    resume_replay
        .send(())
        .map_err(|_| anyhow::anyhow!("compatibility interaction stream dropped replay hook"))?;

    let response = tokio::time::timeout(ci_scaled(Duration::from_secs(2)), request_task)
        .await
        .context("compatibility interaction request did not return SSE headers")?
        .context("compatibility interaction request task panicked")??;
    let (frame_tx, mut frame_rx) = mpsc::unbounded_channel();
    let body_task = tokio::spawn(collect_sse_frames(response, frame_tx));

    let required_payload = tokio::time::timeout(ci_scaled(Duration::from_secs(2)), async {
        loop {
            let (event, payload) = frame_rx
                .recv()
                .await
                .context("compatibility interaction stream ended before user input")?;
            if event == "user_input.required" {
                break Ok::<_, anyhow::Error>(payload);
            }
        }
    })
    .await
    .context("compatibility stream did not expose required user input")??;
    assert_eq!(required_payload["id"], "input_compat");
    assert_eq!(required_payload["input_id"], "input_compat");
    assert_eq!(required_payload["thread_id"], thread_id);
    assert_eq!(required_payload["turn_id"], turn_id);
    assert_eq!(required_payload["status"], "required");
    assert_eq!(required_payload["request"]["questions"][0]["id"], "choice");
    assert!(required_payload.get("answers").is_none());

    const SECRET_ANSWER: &str = "compat-answer-must-not-be-echoed";
    let submitted: serde_json::Value = client
        .post(format!(
            "http://{addr}/v1/user-input/{thread_id}/input_compat"
        ))
        .json(&json!({
            "answers": [{
                "id": "choice",
                "label": "Continue",
                "value": SECRET_ANSWER,
            }],
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(submitted["delivered"], true);
    let (submitted_id, submitted_response) =
        tokio::time::timeout(ci_scaled(Duration::from_secs(2)), submission_rx)
            .await
            .context("mock engine did not receive compatibility user input")?
            .context("mock engine dropped compatibility user input")?
            .context("compatibility user input was canceled instead of submitted")?;
    assert_eq!(submitted_id, "input_compat");
    assert_eq!(submitted_response.answers[0].value, SECRET_ANSWER);
    release_completion
        .send(())
        .map_err(|_| anyhow::anyhow!("mock interaction engine dropped completion release"))?;

    let frames = tokio::time::timeout(ci_scaled(Duration::from_secs(3)), body_task)
        .await
        .context("compatibility interaction stream did not terminate")?
        .context("compatibility interaction body task panicked")??;
    engine_task
        .await
        .context("compatibility interaction engine task panicked")??;

    let answered = frames
        .iter()
        .find(|(event, _)| event == "user_input.answered")
        .context("compatibility stream omitted submitted user-input lifecycle")?;
    assert_eq!(answered.1["id"], "input_compat");
    assert_eq!(answered.1["status"], "submitted");
    assert!(answered.1.get("answers").is_none());
    assert_eq!(
        frames
            .iter()
            .filter(|(event, _)| event == "user_input.required")
            .count(),
        1
    );
    assert_eq!(
        frames
            .iter()
            .filter(|(event, _)| event == "user_input.answered")
            .count(),
        1
    );
    assert!(
        !frames
            .iter()
            .any(|(event, _)| event == "user_input.canceled")
    );
    assert!(frames.iter().any(|(event, _)| event == "turn.completed"));
    assert_eq!(
        frames.iter().filter(|(event, _)| event == "done").count(),
        1
    );
    assert!(
        !serde_json::to_string(&frames)?.contains(SECRET_ANSWER),
        "submitted answer leaked into compatibility SSE"
    );
    let detail = runtime_threads.get_thread_detail(&thread_id).await?;
    let serialized_detail = serde_json::to_string(&detail)?;
    assert!(
        !serialized_detail.contains(SECRET_ANSWER),
        "submitted answer leaked into the thread snapshot"
    );
    let redacted_item = detail
        .items
        .iter()
        .find(|item| {
            item.metadata
                .as_ref()
                .and_then(|metadata| metadata.get("tool_name"))
                .and_then(Value::as_str)
                == Some("request_user_input")
        })
        .context("request_user_input Runtime receipt was not persisted")?;
    assert_eq!(
        redacted_item.detail.as_deref(),
        Some("User input submitted")
    );
    assert_eq!(
        redacted_item
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("response_redacted"))
            .and_then(Value::as_bool),
        Some(true)
    );
    let durable_events = runtime_threads.events_since(&thread_id, None)?;
    assert!(
        !serde_json::to_string(&durable_events)?.contains(SECRET_ANSWER),
        "submitted answer leaked into the durable Runtime event log"
    );
    let leaked_file = ignore::WalkBuilder::new(root.join("runtime"))
        .hidden(false)
        .build()
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .find_map(|entry| {
            fs::read_to_string(entry.path())
                .ok()
                .filter(|contents| contents.contains(SECRET_ANSWER))
                .map(|_| entry.path().to_path_buf())
        });
    assert!(
        leaked_file.is_none(),
        "submitted answer leaked into Runtime file {}",
        leaked_file
            .as_deref()
            .map(std::path::Path::display)
            .map(|path| path.to_string())
            .unwrap_or_default()
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn thread_endpoints_expose_lifecycle_contract() -> Result<()> {
    let Some((addr, runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let created: serde_json::Value = client
        .post(format!("http://{addr}/v1/threads"))
        .json(&json!({}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let thread_id = created["id"]
        .as_str()
        .context("missing thread id")?
        .to_string();

    let archived: serde_json::Value = client
        .patch(format!("http://{addr}/v1/threads/{thread_id}"))
        .json(&json!({ "archived": true }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(archived["id"], thread_id);
    assert_eq!(archived["archived"], true);

    let listed: serde_json::Value = client
        .get(format!("http://{addr}/v1/threads"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert!(
        listed
            .as_array()
            .is_some_and(|threads| threads.iter().all(|t| t["id"] != thread_id))
    );

    let listed_all: serde_json::Value = client
        .get(format!(
            "http://{addr}/v1/threads/summary?include_archived=true&limit=100"
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert!(
        listed_all
            .as_array()
            .is_some_and(|threads| threads.iter().any(|t| t["id"] == thread_id))
    );

    let unarchived: serde_json::Value = client
        .patch(format!("http://{addr}/v1/threads/{thread_id}"))
        .json(&json!({ "archived": false }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(unarchived["archived"], false);

    let invalid_patch = client
        .patch(format!("http://{addr}/v1/threads/{thread_id}"))
        .json(&json!({}))
        .send()
        .await?;
    assert_eq!(invalid_patch.status(), StatusCode::BAD_REQUEST);

    let missing_patch = client
        .patch(format!("http://{addr}/v1/threads/thr_missing"))
        .json(&json!({ "archived": true }))
        .send()
        .await?;
    assert_eq!(missing_patch.status(), StatusCode::NOT_FOUND);

    let detail: serde_json::Value = client
        .get(format!("http://{addr}/v1/threads/{thread_id}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(detail["thread"]["id"], thread_id);

    let resumed: serde_json::Value = client
        .post(format!("http://{addr}/v1/threads/{thread_id}/resume"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(resumed["id"], thread_id);

    let forked: serde_json::Value = client
        .post(format!("http://{addr}/v1/threads/{thread_id}/fork"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let forked_id = forked["id"].as_str().context("missing forked id")?;
    assert_ne!(forked_id, thread_id);

    // Install a mock engine so the turn completes without calling the real API.
    // The mock handles both SendMessage and CompactContext ops so the
    // compact endpoint tested later also works.
    let harness = crate::core::engine::mock_engine_handle();
    runtime_threads
        .install_test_engine(&thread_id, harness.handle.clone())
        .await?;
    let mut rx_op = harness.rx_op;
    let tx_event = harness.tx_event;
    tokio::spawn(async move {
        while let Some(op) = rx_op.recv().await {
            match op {
                Op::SendMessage(TurnSpec { .. }) => {
                    let _ = tx_event
                        .send(EngineEvent::TurnStarted {
                            turn_id: "mock_lifecycle".to_string(),
                            created_at: chrono::Utc::now(),
                            route: None,
                            submission_id: None,
                        })
                        .await;
                    let _ = tx_event
                        .send(EngineEvent::MessageStarted { index: 0 })
                        .await;
                    let _ = tx_event
                        .send(EngineEvent::MessageDelta {
                            index: 0,
                            content: "mock reply".to_string(),
                        })
                        .await;
                    let _ = tx_event
                        .send(EngineEvent::MessageComplete { index: 0 })
                        .await;
                    let _ = tx_event
                        .send(EngineEvent::TurnComplete {
                            usage: Usage {
                                input_tokens: 10,
                                output_tokens: 5,
                                ..Usage::default()
                            },
                            parent_route_usage: Usage {
                                input_tokens: 10,
                                output_tokens: 5,
                                ..Usage::default()
                            },
                            routed_usage_dropped_records: 0,
                            status: TurnOutcomeStatus::Completed,
                            error: None,
                            tool_catalog: None,
                            base_url: None,
                        })
                        .await;
                }
                Op::CompactContext { .. } => {
                    let _ = tx_event
                        .send(EngineEvent::TurnComplete {
                            usage: Usage {
                                input_tokens: 0,
                                output_tokens: 0,
                                ..Usage::default()
                            },
                            parent_route_usage: Usage::default(),
                            routed_usage_dropped_records: 0,
                            status: TurnOutcomeStatus::Completed,
                            error: None,
                            tool_catalog: None,
                            base_url: None,
                        })
                        .await;
                }
                _ => {}
            }
        }
    });

    let turn_start: serde_json::Value = client
        .post(format!("http://{addr}/v1/threads/{thread_id}/turns"))
        .json(&json!({ "prompt": "thread endpoint test" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let turn_id = turn_start["turn"]["id"]
        .as_str()
        .context("missing turn id")?
        .to_string();

    let _ =
        wait_for_terminal_turn_status(&client, addr, &thread_id, &turn_id, Duration::from_secs(10))
            .await?;

    let steer_resp = client
        .post(format!(
            "http://{addr}/v1/threads/{thread_id}/turns/{turn_id}/steer"
        ))
        .json(&json!({ "prompt": "late steer" }))
        .send()
        .await?;
    assert_eq!(steer_resp.status(), StatusCode::CONFLICT);

    let interrupt_resp = client
        .post(format!(
            "http://{addr}/v1/threads/{thread_id}/turns/{turn_id}/interrupt"
        ))
        .send()
        .await?;
    assert_eq!(interrupt_resp.status(), StatusCode::CONFLICT);

    let compact_start: serde_json::Value = client
        .post(format!("http://{addr}/v1/threads/{thread_id}/compact"))
        .json(&json!({ "reason": "test manual compact" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(compact_start["thread"]["id"], thread_id);

    let events_resp = client
        .get(format!(
            "http://{addr}/v1/threads/{thread_id}/events?since_seq=0"
        ))
        .send()
        .await?
        .error_for_status()?;
    let content_type = events_resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(content_type.starts_with("text/event-stream"));
    let chunk_text = read_first_sse_frame(events_resp).await?;
    assert!(
        chunk_text.contains("event:"),
        "expected SSE event chunk, got: {chunk_text}"
    );
    let (event_name, payload) = parse_sse_frame(&chunk_text)?;
    assert_eq!(event_name, "thread.started");
    assert!(
        event_name.starts_with("item.")
            || event_name.starts_with("turn.")
            || event_name.starts_with("thread.")
            || event_name == "turn.completed"
            || event_name == "turn.started"
            || event_name == "thread.started",
        "unexpected first event name: {event_name}"
    );
    assert_eq!(payload["event"], payload["kind"]);
    assert!(payload.get("turn_id").is_some());
    assert!(payload.get("item_id").is_some());
    assert!(payload["turn_id"].is_null());
    assert!(payload["item_id"].is_null());
    assert_eq!(payload["thread_id"], thread_id);
    assert!(
        payload["schema_version"]
            .as_u64()
            .is_some_and(|version| version >= 1)
    );
    assert!(payload.get("seq").and_then(Value::as_u64).is_some());
    assert!(payload["payload"].is_object() || payload["payload"].is_array());

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn turn_endpoint_operation_key_returns_original_and_conflicts_on_mismatch() -> Result<()> {
    let Some((addr, runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let created: serde_json::Value = client
        .post(format!("http://{addr}/v1/threads"))
        .json(&json!({}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let thread_id = created["id"].as_str().context("thread id")?.to_string();

    let mut harness = crate::core::engine::mock_engine_handle();
    runtime_threads
        .install_test_engine(&thread_id, harness.handle.clone())
        .await?;
    let send_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = send_count.clone();
    let tx_event = harness.tx_event.clone();
    tokio::spawn(async move {
        while let Some(op) = harness.rx_op.recv().await {
            if !matches!(op, Op::SendMessage(TurnSpec { .. })) {
                continue;
            }
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            sleep(Duration::from_millis(20)).await;
            let _ = tx_event
                .send(EngineEvent::TurnComplete {
                    usage: Usage::default(),
                    parent_route_usage: Usage::default(),
                    routed_usage_dropped_records: 0,
                    status: TurnOutcomeStatus::Completed,
                    error: None,
                    tool_catalog: None,
                    base_url: None,
                })
                .await;
        }
    });

    let url = format!("http://{addr}/v1/threads/{thread_id}/turns");
    let request = json!({
        "prompt": "one HTTP operation",
        "operation_key": "cwc-http-operation-1",
        "reasoning_effort": "high",
        "allowed_tools": []
    });
    let first_response = client.post(&url).json(&request).send().await?;
    assert_eq!(first_response.status(), StatusCode::CREATED);
    let first: serde_json::Value = first_response.json().await?;
    let first_turn_id = first["turn"]["id"].as_str().context("turn id")?;
    assert!(!serde_json::to_string(&first)?.contains("cwc-http-operation-1"));

    let replay_response = client.post(&url).json(&request).send().await?;
    // A replay acknowledges work already accepted rather than admitting new
    // work: 200 plus an explicit flag, so a client that retried an ambiguous
    // submit can tell it is looking at the turn it already started (#76).
    assert_eq!(replay_response.status(), StatusCode::OK);
    let replay: serde_json::Value = replay_response.json().await?;
    assert_eq!(replay["turn"]["id"], first_turn_id);
    assert_eq!(replay["idempotent_replay"], true);
    // A fresh admission carries no flag, so the response every existing client
    // already parses is byte-identical to before.
    assert!(
        first.get("idempotent_replay").is_none(),
        "only a replay is marked as one: {first}"
    );

    let mismatch = client
        .post(&url)
        .json(&json!({
            "prompt": "changed HTTP operation",
            "operation_key": "cwc-http-operation-1",
            "reasoning_effort": "high",
            "allowed_tools": []
        }))
        .send()
        .await?;
    assert_eq!(mismatch.status(), StatusCode::CONFLICT);

    sleep(Duration::from_millis(40)).await;
    assert_eq!(
        send_count.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "exact replay and mismatch must not emit another SendMessage"
    );
    assert_eq!(
        runtime_threads
            .test_store()
            .list_turns_for_thread(&thread_id)?
            .len(),
        1
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn turn_operation_lookup_is_authenticated_read_only_and_survives_restart() -> Result<()> {
    fn file_bytes(root: &Path) -> Result<std::collections::BTreeMap<PathBuf, Vec<u8>>> {
        let mut files = std::collections::BTreeMap::new();
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                files.extend(file_bytes(&entry.path())?);
            } else {
                // Match the Runtime-store lookup fixture: Windows locks can
                // deny reads even on empty claim files. Their length proves
                // their bytes; retain every path so a new lock still fails.
                let bytes = if entry.metadata()?.len() == 0 {
                    Vec::new()
                } else {
                    fs::read(entry.path()).with_context(|| {
                        format!("read Runtime fixture file {:?}", entry.file_name())
                    })?
                };
                files.insert(entry.path(), bytes);
            }
        }
        Ok(files)
    }

    let _env = lock_test_env();
    let dir = tempfile::tempdir()?;
    let home = dir.path().join("home");
    fs::create_dir_all(&home)?;
    let _home = EnvVarGuard::set("CODEWHALE_HOME", &home);
    let root = dir.path().join("server");
    let store_root = root.join("runtime/runtime");
    let sessions = dir.path().join("sessions");
    let workspace = dir.path().join("workspace");
    let token = "turn-lookup-local-fixture";
    let _backend = EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    let _proxy = EnvVarGuard::set("NO_PROXY", "*");
    let (addr, manager, server) = spawn_test_server_with_root_token_mobile_workspace(
        root.clone(),
        sessions.clone(),
        Some(token.into()),
        false,
        workspace.clone(),
    )
    .await?
    .context("loopback listener required for turn operation lookup proof")?;
    let client = crate::tls::reqwest_client_builder()
        .timeout(ci_scaled(Duration::from_secs(2)))
        .build()?;
    let thread = manager.create_thread(Default::default()).await?;
    let other_thread = manager.create_thread(Default::default()).await?;
    let mut engine = crate::core::engine::mock_engine_handle();
    manager
        .install_test_engine(&thread.id, engine.handle.clone())
        .await?;
    let key = "cwc-http-operation-lookup";
    let accepted = client
        .post(format!("http://{addr}/v1/threads/{}/turns", thread.id))
        .bearer_auth(token)
        .json(&json!({"prompt":"one accepted lookup fixture", "operation_key":key}))
        .send()
        .await?;
    assert_eq!(accepted.status(), StatusCode::CREATED);
    let accepted: Value = accepted.json().await?;
    let turn_id = accepted["turn"]["id"]
        .as_str()
        .context("accepted turn id")?
        .to_string();
    assert!(matches!(
        tokio::time::timeout(ci_scaled(Duration::from_secs(2)), engine.rx_op.recv())
            .await?
            .context("accepted mock Engine operation")?,
        Op::SendMessage(TurnSpec { .. })
    ));

    let endpoint = format!(
        "http://{addr}/v1/threads/{}/turn-operations/{key}",
        thread.id
    );
    let before = file_bytes(&store_root)?;
    let response = client.get(&endpoint).bearer_auth(token).send().await?;
    assert_eq!(response.status(), StatusCode::OK);
    let found: Value = response.json().await?;
    assert_eq!(
        found,
        serde_json::to_value(manager.test_store().load_turn(&turn_id)?)?,
        "lookup returns the existing bare TurnRecord"
    );
    assert_eq!(found["id"], accepted["turn"]["id"]);
    assert!(found.get("turn").is_none());
    for bearer in [None, Some("wrong-lookup-token")] {
        let mut request = client.get(&endpoint);
        if let Some(bearer) = bearer {
            request = request.bearer_auth(bearer);
        }
        assert_eq!(request.send().await?.status(), StatusCode::UNAUTHORIZED);
    }
    for (thread_id, operation_key) in [
        (thread.id.as_str(), "absent-operation"),
        (other_thread.id.as_str(), key),
    ] {
        let response = client
            .get(format!(
                "http://{addr}/v1/threads/{thread_id}/turn-operations/{operation_key}"
            ))
            .bearer_auth(token)
            .send()
            .await?;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
    let overlong = "x".repeat(crate::runtime_threads::MAX_RUNTIME_TURN_OPERATION_KEY_BYTES + 1);
    for (thread_id, operation_key) in [
        (thread.id.as_str(), "%20leading"),
        (thread.id.as_str(), "control%0Acharacter"),
        (thread.id.as_str(), overlong.as_str()),
        ("invalid%20thread", key),
    ] {
        let response = client
            .get(format!(
                "http://{addr}/v1/threads/{thread_id}/turn-operations/{operation_key}"
            ))
            .bearer_auth(token)
            .send()
            .await?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    assert_eq!(
        file_bytes(&store_root)?,
        before,
        "GET must not create a lock or rewrite records"
    );
    assert!(
        engine.rx_op.try_recv().is_err(),
        "GET must not dispatch an Engine operation"
    );

    for event in [
        EngineEvent::TurnStarted {
            turn_id: "lookup-fixture-engine-turn".into(),
            created_at: chrono::Utc::now(),
            route: None,
            submission_id: None,
        },
        EngineEvent::MessageStarted { index: 0 },
        EngineEvent::MessageDelta {
            index: 0,
            content: "lookup fixture completed".into(),
        },
        EngineEvent::MessageComplete { index: 0 },
    ] {
        engine.tx_event.send(event).await?;
    }
    engine
        .tx_event
        .send(EngineEvent::TurnComplete {
            usage: Default::default(),
            parent_route_usage: Default::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;
    tokio::time::timeout(ci_scaled(Duration::from_secs(10)), async {
        loop {
            if manager.test_store().load_turn(&turn_id)?.status == RuntimeTurnStatus::Completed {
                return Ok::<_, anyhow::Error>(());
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("mock turn did not settle before restart")??;
    tokio::time::timeout(
        ci_scaled(Duration::from_secs(10)),
        manager.shutdown_and_wait(),
    )
    .await??;
    let settled = serde_json::to_value(manager.test_store().load_turn(&turn_id)?)?;
    let released = Arc::downgrade(&manager);
    server.abort();
    let _ = server.await;
    drop(manager);
    drop(engine);
    tokio::time::timeout(ci_scaled(Duration::from_secs(10)), async {
        while released.upgrade().is_some() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("old Runtime did not release its store before restart")?;

    // The old server tears its runtime down on its own thread after the
    // abort, so its mock TaskManager can hold the execution-scope owner lock
    // a moment longer than the thread manager above. A second owner is
    // correctly refused; wait for the release instead of racing it.
    let deadline = tokio::time::Instant::now() + ci_scaled(Duration::from_secs(10));
    let (addr, _manager, server) = loop {
        match spawn_test_server_with_root_token_mobile_workspace(
            root.clone(),
            sessions.clone(),
            Some(token.into()),
            false,
            workspace.clone(),
        )
        .await
        {
            Err(error)
                if format!("{error:#}").contains("execution scope is already owned")
                    && tokio::time::Instant::now() < deadline =>
            {
                sleep(Duration::from_millis(20)).await;
            }
            started => {
                break started?.context("loopback listener required for restarted lookup proof")?;
            }
        }
    };
    // Startup recovery is complete. No Engine is installed in this Runtime.
    let before = file_bytes(&store_root)?;
    let response = client
        .get(format!(
            "http://{addr}/v1/threads/{}/turn-operations/{key}",
            thread.id
        ))
        .bearer_auth(token)
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.json::<Value>().await?, settled);
    assert_eq!(
        file_bytes(&store_root)?,
        before,
        "restart lookup must remain read-only"
    );
    server.abort();
    Ok(())
}

#[tokio::test]
async fn events_endpoint_respects_since_seq_cursor() -> Result<()> {
    let Some((addr, runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let created: serde_json::Value = client
        .post(format!("http://{addr}/v1/threads"))
        .json(&json!({}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let thread_id = created["id"]
        .as_str()
        .context("missing thread id")?
        .to_string();

    // Install a mock engine so the turn completes without calling the real API.
    let harness = crate::core::engine::mock_engine_handle();
    runtime_threads
        .install_test_engine(&thread_id, harness.handle.clone())
        .await?;
    let mut rx_op = harness.rx_op;
    let tx_event = harness.tx_event;
    tokio::spawn(async move {
        if !matches!(rx_op.recv().await, Some(Op::SendMessage(TurnSpec { .. }))) {
            return;
        }
        let _ = tx_event
            .send(EngineEvent::TurnStarted {
                turn_id: "mock_cursor".to_string(),
                created_at: chrono::Utc::now(),
                route: None,
                submission_id: None,
            })
            .await;
        let _ = tx_event
            .send(EngineEvent::MessageStarted { index: 0 })
            .await;
        let _ = tx_event
            .send(EngineEvent::MessageComplete { index: 0 })
            .await;
        let _ = tx_event
            .send(EngineEvent::TurnComplete {
                usage: Usage {
                    input_tokens: 5,
                    output_tokens: 3,
                    ..Usage::default()
                },
                parent_route_usage: Usage {
                    input_tokens: 5,
                    output_tokens: 3,
                    ..Usage::default()
                },
                routed_usage_dropped_records: 0,
                status: TurnOutcomeStatus::Completed,
                error: None,
                tool_catalog: None,
                base_url: None,
            })
            .await;
    });

    let started: serde_json::Value = client
        .post(format!("http://{addr}/v1/threads/{thread_id}/turns"))
        .json(&json!({ "prompt": "cursor replay test" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let turn_id = started["turn"]["id"]
        .as_str()
        .context("missing turn id")?
        .to_string();

    let _ =
        wait_for_terminal_turn_status(&client, addr, &thread_id, &turn_id, Duration::from_secs(10))
            .await?;

    let resp_a = client
        .get(format!(
            "http://{addr}/v1/threads/{thread_id}/events?since_seq=0"
        ))
        .send()
        .await?
        .error_for_status()?;
    let frame_a = read_first_sse_frame(resp_a).await?;
    let (event_a, payload_a) = parse_sse_frame(&frame_a)?;
    assert_eq!(event_a, "thread.started");
    assert!(payload_a.get("turn_id").is_some());
    assert!(payload_a.get("item_id").is_some());
    assert!(payload_a["turn_id"].is_null());
    assert!(payload_a["item_id"].is_null());
    assert!(payload_a.get("schema_version").is_some());
    assert_eq!(payload_a["event"], payload_a["kind"]);
    assert_eq!(payload_a["thread_id"], thread_id);
    let seq_a = payload_a
        .get("seq")
        .and_then(Value::as_u64)
        .context("missing seq in first replay frame")?;

    let resp_b = client
        .get(format!(
            "http://{addr}/v1/threads/{thread_id}/events?since_seq={seq_a}"
        ))
        .send()
        .await?
        .error_for_status()?;
    let frame_b = read_first_sse_frame(resp_b).await?;
    let (_event_b, payload_b) = parse_sse_frame(&frame_b)?;
    assert!(payload_b.get("schema_version").is_some());
    assert_eq!(payload_b["event"], payload_b["kind"]);
    assert_eq!(payload_b["thread_id"], thread_id);
    let seq_b = payload_b
        .get("seq")
        .and_then(Value::as_u64)
        .context("missing seq in second replay frame")?;
    assert!(
        seq_b > seq_a,
        "expected seq after cursor: {seq_b} <= {seq_a}"
    );

    handle.abort();
    Ok(())
}

/// The SSE `id:` a browser `EventSource` resumes from, and the `Last-Event-ID`
/// header it replays with, against the same durable cursor `since_seq` uses.
#[tokio::test]
async fn thread_event_frames_carry_the_id_a_reconnect_resumes_from() -> Result<()> {
    let Some((addr, runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let thread = runtime_threads
        .create_thread(CreateThreadRequest::default())
        .await?;

    // Every journal frame carries its durable seq as the SSE id.
    let first = client
        .get(format!(
            "http://{addr}/v1/threads/{}/events?since_seq=0",
            thread.id
        ))
        .send()
        .await?
        .error_for_status()?;
    let frame = read_first_sse_frame(first).await?;
    let (_event, payload) = parse_sse_frame(&frame)?;
    let first_seq = payload
        .get("seq")
        .and_then(Value::as_u64)
        .context("missing seq in first frame")?;
    let id = frame
        .lines()
        .find_map(|line| line.strip_prefix("id:"))
        .map(str::trim)
        .context("SSE frames must carry an id, or nothing can resume")?
        .to_string();
    assert_eq!(
        id.parse::<u64>()?,
        first_seq,
        "the id is the durable seq, not a frame counter"
    );

    // A second durable event, so a resume has somewhere to land.
    let second_seq = runtime_threads
        .emit_event_for_test(
            &thread.id,
            None,
            "approval.required",
            json!({"approval_id": "resume-proof", "tool_name": "exec_command"}),
        )
        .await?
        .seq;
    assert!(second_seq > first_seq, "the second event is later");

    // The header alone is enough: a browser cannot set a query cursor.
    let resumed = client
        .get(format!("http://{addr}/v1/threads/{}/events", thread.id))
        .header("Last-Event-ID", &id)
        .send()
        .await?
        .error_for_status()?;
    let frame = read_first_sse_frame(resumed).await?;
    let (_event, payload) = parse_sse_frame(&frame)?;
    assert_eq!(
        payload.get("seq").and_then(Value::as_u64),
        Some(second_seq),
        "Last-Event-ID must resume past the acknowledged frame"
    );

    // An explicit `since_seq` outranks the header, so a deliberate
    // replay-from-zero is never silently overridden by a stale id.
    let explicit = client
        .get(format!(
            "http://{addr}/v1/threads/{}/events?since_seq=0",
            thread.id
        ))
        .header("Last-Event-ID", &id)
        .send()
        .await?
        .error_for_status()?;
    let frame = read_first_sse_frame(explicit).await?;
    let (_event, payload) = parse_sse_frame(&frame)?;
    assert_eq!(
        payload.get("seq").and_then(Value::as_u64),
        Some(first_seq),
        "the query cursor wins over the header"
    );

    handle.abort();
    Ok(())
}

#[test]
fn last_event_id_accepts_only_decimal_cursors() {
    use super::last_event_id;

    let mut headers = axum::http::HeaderMap::new();
    assert_eq!(last_event_id(&headers), None, "absent header is no cursor");

    headers.insert("last-event-id", "42".parse().unwrap());
    assert_eq!(last_event_id(&headers), Some(42));

    headers.insert("last-event-id", " 7 ".parse().unwrap());
    assert_eq!(last_event_id(&headers), Some(7), "whitespace is trimmed");

    // An opaque id from a proxy or an older client opens the stream from the
    // durable head instead of refusing to open it at all.
    headers.insert("last-event-id", "fev1_abcdef".parse().unwrap());
    assert_eq!(last_event_id(&headers), None);
}

#[tokio::test]
async fn event_handoff_replays_and_dedupes_interaction_prompts_without_a_gap() -> Result<()> {
    let Some((_addr, runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let thread = runtime_threads
        .create_thread(CreateThreadRequest::default())
        .await?;
    let initial_seq = runtime_threads
        .events_since(&thread.id, None)?
        .last()
        .context("thread creation should emit an event")?
        .seq;

    // Deterministically place approval.required in the old vulnerable window:
    // the receiver exists, but durable replay has not been read yet.
    let live = runtime_threads.subscribe_events();
    let approval = runtime_threads
        .emit_event_for_test(
            &thread.id,
            None,
            "approval.required",
            json!({
                "approval_id": "approval-handoff",
                "tool_name": "exec_command",
                "description": "Run a local check",
            }),
        )
        .await?;
    let backlog = runtime_threads.events_since(&thread.id, Some(initial_seq))?;
    let (backlog_tx, backlog_rx) = mpsc::channel(1);
    backlog_tx
        .send(Ok(backlog))
        .await
        .map_err(|_| anyhow::anyhow!("failed to seed replay backlog"))?;
    drop(backlog_tx);

    // This request lands after the replay read and is therefore live-only.
    let input = runtime_threads
        .emit_event_for_test(
            &thread.id,
            None,
            "user_input.required",
            json!({
                "id": "input-handoff",
                "request": {
                    "questions": [{
                        "id": "choice",
                        "question": "Continue?",
                        "options": [],
                    }],
                },
            }),
        )
        .await?;

    let stream = replay_live_thread_events(
        runtime_threads.clone(),
        thread.id.clone(),
        initial_seq,
        backlog_rx,
        live,
        false,
        CancellationToken::new(),
    )
    .take(2);
    let body =
        axum::body::to_bytes(Sse::new(stream).into_response().into_body(), usize::MAX).await?;
    let rendered = String::from_utf8(body.to_vec())?;
    let frames = rendered
        .split("\n\n")
        .map(str::trim)
        .filter(|frame| !frame.is_empty())
        .map(parse_sse_frame)
        .collect::<Result<Vec<_>>>()?;

    assert_eq!(frames.len(), 2, "unexpected SSE frames: {rendered}");
    assert_eq!(frames[0].0, "approval.required");
    assert_eq!(frames[0].1["seq"], approval.seq);
    assert_eq!(frames[0].1["previous_seq"], initial_seq);
    assert_eq!(frames[1].0, "user_input.required");
    assert_eq!(frames[1].1["seq"], input.seq);
    assert_eq!(frames[1].1["previous_seq"], approval.seq);
    assert_eq!(rendered.matches("approval-handoff").count(), 1);
    assert_eq!(rendered.matches("input-handoff").count(), 1);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn pet_stream_progress_drains_queued_answers_before_becoming_live() -> Result<()> {
    let (_addr, runtime_threads, handle) = spawn_test_server()
        .await?
        .context("Runtime socket unavailable for pet progress acceptance")?;
    let thread = runtime_threads
        .create_thread(CreateThreadRequest::default())
        .await?;
    let initial = runtime_threads
        .events_since(&thread.id, None)?
        .last()
        .unwrap()
        .seq;
    let live = runtime_threads.subscribe_events();
    let required = runtime_threads
        .emit_event_for_test(
            &thread.id,
            None,
            "user_input.required",
            json!({"id":"old-request"}),
        )
        .await?;
    let backlog = runtime_threads.events_since(&thread.id, Some(initial))?;
    let (tx, rx) = mpsc::channel(1);
    tx.send(Ok(backlog)).await?;
    drop(tx);
    let answered = runtime_threads
        .emit_event_for_test(
            &thread.id,
            None,
            "user_input.answered",
            json!({"id":"old-request"}),
        )
        .await?;
    let stream = replay_live_thread_events(
        runtime_threads.clone(),
        thread.id.clone(),
        initial,
        rx,
        live,
        true,
        CancellationToken::new(),
    )
    .take(4);
    let body = tokio::time::timeout(
        ci_scaled(Duration::from_secs(2)),
        axum::body::to_bytes(Sse::new(stream).into_response().into_body(), 64 * 1024),
    )
    .await??;
    let rendered = String::from_utf8(body.to_vec())?;
    let frames = rendered
        .split("\n\n")
        .filter(|s| !s.trim().is_empty())
        .map(parse_sse_frame)
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(frames.len(), 4);
    assert_eq!(
        frames[0].1,
        json!({"schema_version":RUNTIME_EVENT_ENVELOPE_SCHEMA_VERSION,"event":"stream.progress","kind":"stream.progress","thread_id":thread.id,"seq":initial,"state":"replaying"})
    );
    assert_eq!(frames[1].1["seq"], required.seq);
    assert_eq!(frames[2].1["seq"], answered.seq);
    assert_eq!(
        frames[3].1,
        json!({"schema_version":RUNTIME_EVENT_ENVELOPE_SCHEMA_VERSION,"event":"stream.progress","kind":"stream.progress","thread_id":thread.id,"seq":answered.seq,"state":"live"})
    );
    assert_eq!(
        runtime_threads
            .events_since(&thread.id, Some(initial))?
            .len(),
        2,
        "Progress is not journal data"
    );
    handle.abort();
    Ok(())
}

#[tokio::test]
async fn pet_stream_progress_is_opt_in_and_advertised_by_the_actual_endpoint() -> Result<()> {
    let (addr, runtime_threads, handle) = spawn_test_server()
        .await?
        .context("Runtime socket unavailable for pet progress acceptance")?;
    let thread = runtime_threads
        .create_thread(CreateThreadRequest::default())
        .await?;
    let client = crate::tls::reqwest_client();
    let response = client
        .get(format!(
            "http://{addr}/v1/threads/{}/events?progress=true",
            thread.id
        ))
        .send()
        .await?
        .error_for_status()?;
    assert_eq!(
        response
            .headers()
            .get("x-codewhale-event-progress")
            .unwrap(),
        "1"
    );
    let frame = parse_sse_frame(&read_first_sse_frame(response).await?)?;
    assert_eq!(frame.0, "stream.progress");
    assert_eq!(frame.1["state"], "replaying");
    let response = client
        .get(format!("http://{addr}/v1/threads/{}/events", thread.id))
        .send()
        .await?
        .error_for_status()?;
    assert!(
        response
            .headers()
            .get("x-codewhale-event-progress")
            .is_none()
    );
    let frame = parse_sse_frame(&read_first_sse_frame(response).await?)?;
    assert_eq!(frame.0, "thread.started");
    handle.abort();
    Ok(())
}

#[tokio::test]
async fn pet_stream_progress_returns_to_replay_while_recovering_broadcast_lag() -> Result<()> {
    let (_addr, runtime_threads, handle) = spawn_test_server()
        .await?
        .context("Runtime socket unavailable for pet progress acceptance")?;
    let thread = runtime_threads
        .create_thread(CreateThreadRequest::default())
        .await?;
    let initial = runtime_threads
        .events_since(&thread.id, None)?
        .last()
        .unwrap()
        .seq;
    let (backlog_tx, backlog) = mpsc::channel(1);
    drop(backlog_tx);
    let (tx, live) = tokio::sync::broadcast::channel(1);
    let mut stream = Box::pin(replay_live_thread_events(
        runtime_threads.clone(),
        thread.id.clone(),
        initial,
        backlog,
        live,
        true,
        CancellationToken::new(),
    ));
    assert_eq!(
        sse_frame_payload(stream.next().await.unwrap().unwrap()).await?["state"],
        "replaying"
    );
    assert_eq!(
        sse_frame_payload(stream.next().await.unwrap().unwrap()).await?["state"],
        "live"
    );
    let required = runtime_threads
        .emit_event_for_test(
            &thread.id,
            None,
            "user_input.required",
            json!({"id":"queued-request"}),
        )
        .await?;
    let answered = runtime_threads
        .emit_event_for_test(
            &thread.id,
            None,
            "user_input.answered",
            json!({"id":"queued-request"}),
        )
        .await?;
    tx.send(required.clone())?;
    tx.send(answered.clone())?;
    let frames = tokio::time::timeout(ci_scaled(Duration::from_secs(2)), async {
        let mut frames = Vec::new();
        for _ in 0..4 {
            frames.push(sse_frame_payload(stream.next().await.unwrap().unwrap()).await?);
        }
        Ok::<_, anyhow::Error>(frames)
    })
    .await??;
    assert_eq!(frames[0]["state"], "replaying");
    assert_eq!(frames[0]["seq"], initial);
    assert_eq!(frames[1]["seq"], required.seq);
    assert_eq!(frames[2]["seq"], answered.seq);
    assert_eq!(frames[3]["state"], "live");
    assert_eq!(frames[3]["seq"], answered.seq);
    assert_eq!(
        runtime_threads
            .events_since(&thread.id, Some(initial))?
            .len(),
        2
    );
    handle.abort();
    Ok(())
}

/// One rendered frame of a thread event stream: its SSE `id:` (if any), event
/// name and JSON payload.
type RenderedThreadFrame = (Option<String>, String, Value);

fn parse_thread_frame(raw: &str) -> Result<RenderedThreadFrame> {
    let id = raw
        .lines()
        .find_map(|line| line.strip_prefix("id:"))
        .map(|id| id.trim().to_string());
    let (event, payload) = parse_sse_frame(raw)?;
    Ok((id, event, payload))
}

/// Renders a thread event stream that must end on its own; a hang fails.
async fn render_thread_stream(
    stream: impl futures_util::Stream<Item = Result<SseEvent, Infallible>> + Send + 'static,
) -> Result<Vec<RenderedThreadFrame>> {
    let body = tokio::time::timeout(
        ci_scaled(Duration::from_secs(5)),
        axum::body::to_bytes(Sse::new(stream).into_response().into_body(), usize::MAX),
    )
    .await
    .context("a server-ended thread stream must close by itself")??;
    String::from_utf8(body.to_vec())?
        .split("\n\n")
        .filter(|raw| !raw.trim().is_empty())
        .map(parse_thread_frame)
        .collect()
}

/// Reads an HTTP thread stream to EOF; a hang fails.
async fn read_thread_stream_to_eof(
    response: reqwest::Response,
) -> Result<Vec<RenderedThreadFrame>> {
    let body = tokio::time::timeout(ci_scaled(Duration::from_secs(10)), response.text())
        .await
        .context("the server must close the stream after stream.end")??;
    body.split("\n\n")
        .filter(|raw| !raw.trim().is_empty() && !raw.trim_start().starts_with(':'))
        .map(parse_thread_frame)
        .collect()
}

async fn next_thread_frame<S>(stream: &mut S) -> Result<Value>
where
    S: futures_util::Stream<Item = Result<SseEvent, Infallible>> + Unpin,
{
    let Ok(event) = tokio::time::timeout(ci_scaled(Duration::from_secs(5)), stream.next())
        .await
        .context("thread stream stalled")?
        .context("thread stream ended without stream.end")?;
    sse_frame_payload(event).await
}

fn expected_stream_end(thread_id: &str, reason: &str, last_seq: u64) -> Value {
    json!({
        "schema_version": RUNTIME_EVENT_ENVELOPE_SCHEMA_VERSION,
        "event": "stream.end",
        "kind": "stream.end",
        "thread_id": thread_id,
        "reason": reason,
        "last_seq": last_seq,
        "retryable": true,
    })
}

#[tokio::test]
async fn replay_failure_ends_stream_at_last_delivered_seq_and_resumes_from_it() -> Result<()> {
    let (addr, runtime_threads, handle) = spawn_test_server()
        .await?
        .context("Runtime socket unavailable for stream end acceptance")?;
    let thread = runtime_threads
        .create_thread(CreateThreadRequest::default())
        .await?;
    let initial = runtime_threads
        .events_since(&thread.id, None)?
        .last()
        .context("thread creation should emit an event")?
        .seq;
    let first = runtime_threads
        .emit_event_for_test(&thread.id, None, "item.completed", json!({"n": 1}))
        .await?;
    let second = runtime_threads
        .emit_event_for_test(&thread.id, None, "item.completed", json!({"n": 2}))
        .await?;

    // History delivers two events, then the durable read fails mid-replay.
    let (backlog_tx, backlog) = mpsc::channel(2);
    backlog_tx
        .send(Ok(vec![first.clone(), second.clone()]))
        .await?;
    backlog_tx.send(Err("disk read failed".to_string())).await?;
    drop(backlog_tx);
    let (_live_tx, live) = tokio::sync::broadcast::channel(4);
    let frames = render_thread_stream(replay_live_thread_events(
        runtime_threads.clone(),
        thread.id.clone(),
        initial,
        backlog,
        live,
        false,
        CancellationToken::new(),
    ))
    .await?;

    assert_eq!(frames.len(), 3, "unexpected frames: {frames:?}");
    assert_eq!(frames[0].0, Some(first.seq.to_string()));
    assert_eq!(frames[0].2["previous_seq"], initial);
    assert_eq!(frames[1].0, Some(second.seq.to_string()));
    let (end_id, end_name, end_payload) = &frames[2];
    assert_eq!(end_name, "stream.end");
    assert_eq!(
        end_id, &None,
        "stream.end must not move a browser's Last-Event-ID"
    );
    assert_eq!(
        end_payload,
        &expected_stream_end(&thread.id, "replay_failed", second.seq),
        "no seq, no error text: only the reason and the resume cursor"
    );

    // The resume rule: reconnect with since_seq = last_seq, or let a browser
    // resend its Last-Event-ID; both land on the next event exactly once.
    let third = runtime_threads
        .emit_event_for_test(&thread.id, None, "item.completed", json!({"n": 3}))
        .await?;
    let last_seq = end_payload["last_seq"].as_u64().context("last_seq")?;
    let client = crate::tls::reqwest_client();
    for request in [
        client.get(format!(
            "http://{addr}/v1/threads/{}/events?since_seq={last_seq}",
            thread.id
        )),
        client
            .get(format!("http://{addr}/v1/threads/{}/events", thread.id))
            .header("Last-Event-ID", last_seq.to_string()),
    ] {
        let resumed = request.send().await?.error_for_status()?;
        let (id, _event, payload) = parse_thread_frame(&read_first_sse_frame(resumed).await?)?;
        assert_eq!(payload["seq"], third.seq);
        assert_eq!(payload["previous_seq"], last_seq);
        assert_eq!(id, Some(third.seq.to_string()));
    }

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn replay_failure_over_http_sends_stream_end_then_eof() -> Result<()> {
    let (addr, runtime_threads, handle) = spawn_test_server()
        .await?
        .context("Runtime socket unavailable for stream end acceptance")?;
    let thread = runtime_threads
        .create_thread(CreateThreadRequest::default())
        .await?;
    // A newline-terminated malformed record: the journal read fails closed
    // after the stream has already been answered with 200.
    let path = runtime_threads.events_path_for_test(&thread.id)?;
    let mut journal = fs::OpenOptions::new().append(true).open(&path)?;
    std::io::Write::write_all(&mut journal, b"{malformed-but-terminated}\n")?;
    drop(journal);

    let response = crate::tls::reqwest_client()
        .get(format!(
            "http://{addr}/v1/threads/{}/events?since_seq=0",
            thread.id
        ))
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-codewhale-stream-end")
            .context("every stream advertises its explicit end")?,
        "1"
    );
    let frames = read_thread_stream_to_eof(response).await?;
    // The failing batch is discarded whole, so nothing was delivered and the
    // resume cursor is the requested one.
    assert_eq!(
        frames,
        vec![(
            None,
            "stream.end".to_string(),
            expected_stream_end(&thread.id, "replay_failed", 0)
        )]
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn catch_up_failure_ends_stream_with_catch_up_failed() -> Result<()> {
    let (_addr, runtime_threads, handle) = spawn_test_server()
        .await?
        .context("Runtime socket unavailable for stream end acceptance")?;
    let thread = runtime_threads
        .create_thread(CreateThreadRequest::default())
        .await?;
    let initial = runtime_threads
        .events_since(&thread.id, None)?
        .last()
        .context("thread creation should emit an event")?
        .seq;
    let (backlog_tx, backlog) = mpsc::channel(1);
    drop(backlog_tx);
    let (live_tx, live) = tokio::sync::broadcast::channel(1);
    let mut stream = Box::pin(replay_live_thread_events(
        runtime_threads.clone(),
        thread.id.clone(),
        initial,
        backlog,
        live,
        false,
        CancellationToken::new(),
    ));
    let delivered = runtime_threads
        .emit_event_for_test(&thread.id, None, "item.completed", json!({"n": 1}))
        .await?;
    live_tx.send(delivered.clone())?;
    assert_eq!(next_thread_frame(&mut stream).await?["seq"], delivered.seq);

    // Two sends into a one-slot channel lag the receiver, forcing a durable
    // catch-up; a closed replay hook makes that re-read fail.
    let (hook_tx, hook_rx) = mpsc::unbounded_channel();
    drop(hook_rx);
    runtime_threads.set_replay_test_hook(hook_tx);
    for n in [2, 3] {
        let event = runtime_threads
            .emit_event_for_test(&thread.id, None, "item.completed", json!({"n": n}))
            .await?;
        live_tx.send(event)?;
    }
    assert_eq!(
        next_thread_frame(&mut stream).await?,
        expected_stream_end(&thread.id, "catch_up_failed", delivered.seq)
    );
    assert!(
        tokio::time::timeout(ci_scaled(Duration::from_secs(5)), stream.next())
            .await?
            .is_none(),
        "stream.end is the last frame"
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn closed_live_source_still_ends_stream_explicitly() -> Result<()> {
    // Unreachable in the product while the stream holds the manager that owns
    // the sender; pinned so no future ownership change can make it silent.
    let (_addr, runtime_threads, handle) = spawn_test_server()
        .await?
        .context("Runtime socket unavailable for stream end acceptance")?;
    let thread = runtime_threads
        .create_thread(CreateThreadRequest::default())
        .await?;
    let initial = runtime_threads
        .events_since(&thread.id, None)?
        .last()
        .context("thread creation should emit an event")?
        .seq;
    let (backlog_tx, backlog) = mpsc::channel(1);
    drop(backlog_tx);
    let (live_tx, live) = tokio::sync::broadcast::channel(4);
    let event = runtime_threads
        .emit_event_for_test(&thread.id, None, "item.completed", json!({"n": 1}))
        .await?;
    live_tx.send(event.clone())?;
    drop(live_tx);
    let frames = render_thread_stream(replay_live_thread_events(
        runtime_threads.clone(),
        thread.id.clone(),
        initial,
        backlog,
        live,
        false,
        CancellationToken::new(),
    ))
    .await?;
    assert_eq!(frames.len(), 2, "unexpected frames: {frames:?}");
    assert_eq!(frames[0].2["seq"], event.seq);
    assert_eq!(
        frames[1].2,
        expected_stream_end(&thread.id, "runtime_shutdown", event.seq)
    );
    handle.abort();
    Ok(())
}

/// A deliberate stop goes through the product's serve path: the open stream
/// gets `stream.end {runtime_shutdown}` at its cursor, then the server drains.
#[tokio::test]
async fn runtime_shutdown_ends_open_streams_through_graceful_serve() -> Result<()> {
    let root = std::env::temp_dir().join(format!("codewhale-stream-end-{}", Uuid::new_v4()));
    let shutdown = RuntimeServerShutdown::default();
    let (addr, runtime_threads, handle) =
        spawn_test_server_with_root_token_mobile_workspace_and_overrides(
            root.clone(),
            root.join("sessions"),
            None,
            false,
            root.join("workspace"),
            TestServerOverrides {
                shutdown: Some(shutdown.clone()),
                ..TestServerOverrides::default()
            },
        )
        .await?
        .context("Runtime socket unavailable for shutdown acceptance")?;
    let thread = runtime_threads
        .create_thread(CreateThreadRequest::default())
        .await?;
    let event = runtime_threads
        .emit_event_for_test(&thread.id, None, "item.completed", json!({"n": 1}))
        .await?;
    let response = crate::tls::reqwest_client()
        .get(format!(
            "http://{addr}/v1/threads/{}/events?since_seq=0&progress=true",
            thread.id
        ))
        .send()
        .await?
        .error_for_status()?;
    let (frame_tx, mut frame_rx) = mpsc::unbounded_channel();
    let collector = tokio::spawn(collect_sse_frames(response, frame_tx));
    // Wait until the stream is live, so shutdown interrupts a healthy stream.
    tokio::time::timeout(ci_scaled(Duration::from_secs(5)), async {
        while let Some((name, payload)) = frame_rx.recv().await {
            if name == "stream.progress" && payload["state"] == "live" {
                return;
            }
        }
    })
    .await
    .context("stream never went live")?;

    assert!(
        shutdown.drain(ci_scaled(Duration::from_secs(10))).await,
        "the server must drain once its streams have ended"
    );
    let frames = tokio::time::timeout(ci_scaled(Duration::from_secs(5)), collector)
        .await
        .context("stream stayed open after shutdown")???;
    let ends = frames
        .iter()
        .filter(|(name, _)| name == "stream.end")
        .count();
    assert_eq!(
        ends, 1,
        "exactly one stream.end, and only at the end: {frames:?}"
    );
    let (name, payload) = frames.last().context("no frames")?;
    assert_eq!(name, "stream.end");
    assert_eq!(
        payload,
        &expected_stream_end(&thread.id, "runtime_shutdown", event.seq)
    );

    handle.abort();
    let _ = fs::remove_dir_all(root);
    Ok(())
}

#[tokio::test]
async fn thread_events_pre_stream_failures_are_http_errors_not_sse() -> Result<()> {
    let (addr, runtime_threads, handle) = spawn_test_server()
        .await?
        .context("Runtime socket unavailable for stream error acceptance")?;
    let client = crate::tls::reqwest_client();
    let missing = client
        .get(format!("http://{addr}/v1/threads/thr_missing/events"))
        .send()
        .await?;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert!(
        missing
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("application/json")),
        "an unknown thread is a JSON error, never an event stream"
    );
    assert!(missing.headers().get("x-codewhale-stream-end").is_none());
    let body: Value = missing.json().await?;
    assert!(body.to_string().contains("thr_missing"), "{body}");

    let thread = runtime_threads
        .create_thread(CreateThreadRequest::default())
        .await?;
    let too_long = client
        .get(format!(
            "http://{addr}/v1/threads/{}/events?replay_limit={}",
            thread.id,
            MAX_RUNTIME_EVENT_REPLAY_TAIL + 1
        ))
        .send()
        .await?;
    assert_eq!(too_long.status(), StatusCode::BAD_REQUEST);
    handle.abort();
    Ok(())
}

#[tokio::test]
async fn steer_and_interrupt_endpoints_work_on_active_turn() -> Result<()> {
    let Some((addr, runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let created: serde_json::Value = client
        .post(format!("http://{addr}/v1/threads"))
        .json(&json!({}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let thread_id = created["id"]
        .as_str()
        .context("missing thread id")?
        .to_string();

    let harness = crate::core::engine::mock_engine_handle();
    runtime_threads
        .install_test_engine(&thread_id, harness.handle.clone())
        .await?;
    let mut rx_op = harness.rx_op;
    let mut rx_steer = harness.rx_steer;
    let tx_event = harness.tx_event;
    let cancel_token = harness.cancel_token;
    tokio::spawn(async move {
        if !matches!(rx_op.recv().await, Some(Op::SendMessage(TurnSpec { .. }))) {
            return;
        }
        let _ = tx_event
            .send(EngineEvent::TurnStarted {
                turn_id: "engine_turn_api".to_string(),
                created_at: chrono::Utc::now(),
                route: None,
                submission_id: None,
            })
            .await;
        let _ = tx_event
            .send(EngineEvent::MessageStarted { index: 0 })
            .await;
        if let Some(steer) = rx_steer.recv().await {
            // An engine that uses the steer has committed it; `commit()` is
            // what reports that acceptance back to `steer_turn`.
            let steer_text = steer.into_pending().commit();
            let _ = tx_event
                .send(EngineEvent::MessageDelta {
                    index: 0,
                    content: format!("steer:{steer_text}"),
                })
                .await;
        }
        cancel_token.cancelled().await;
        sleep(Duration::from_millis(60)).await;
        let _ = tx_event
            .send(EngineEvent::TurnComplete {
                usage: Usage {
                    input_tokens: 2,
                    output_tokens: 1,
                    ..Usage::default()
                },
                parent_route_usage: Usage {
                    input_tokens: 2,
                    output_tokens: 1,
                    ..Usage::default()
                },
                routed_usage_dropped_records: 0,
                status: TurnOutcomeStatus::Completed,
                error: None,
                tool_catalog: None,
                base_url: None,
            })
            .await;
    });

    let turn_start: serde_json::Value = client
        .post(format!("http://{addr}/v1/threads/{thread_id}/turns"))
        .json(&json!({ "prompt": "active controls" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let turn_id = turn_start["turn"]["id"]
        .as_str()
        .context("missing turn id")?
        .to_string();

    let steer_resp: serde_json::Value = client
        .post(format!(
            "http://{addr}/v1/threads/{thread_id}/turns/{turn_id}/steer"
        ))
        .json(&json!({ "prompt": "please steer" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(steer_resp["id"], turn_id);
    assert_eq!(steer_resp["steer_count"], 1);

    let interrupt_resp: serde_json::Value = client
        .post(format!(
            "http://{addr}/v1/threads/{thread_id}/turns/{turn_id}/interrupt"
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(interrupt_resp["id"], turn_id);

    let terminal =
        wait_for_terminal_turn_status(&client, addr, &thread_id, &turn_id, Duration::from_secs(10))
            .await?;
    assert_eq!(terminal, "interrupted");

    let events = runtime_threads.events_since(&thread_id, None)?;
    assert!(events.iter().any(|ev| ev.event == "turn.steered"));
    assert!(
        events
            .iter()
            .any(|ev| ev.event == "turn.interrupt_requested")
    );
    assert!(events.iter().any(|ev| {
        ev.event == "turn.completed"
            && ev
                .payload
                .get("turn")
                .and_then(|turn| turn.get("status"))
                .and_then(Value::as_str)
                == Some("interrupted")
    }));

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn stream_compat_mapping_handles_expected_runtime_events() -> Result<()> {
    let agent_delta = RuntimeEventRecord {
        schema_version: 1,
        seq: 1,
        timestamp: chrono::Utc::now(),
        thread_id: "thr_test".to_string(),
        turn_id: Some("turn_test".to_string()),
        item_id: Some("item_test".to_string()),
        event: "item.delta".to_string(),
        payload: json!({
            "kind": "agent_message",
            "delta": "hello",
        }),
    };
    let mapped = map_compat_stream_event(&agent_delta).context("missing mapped SSE event")?;
    let stream = async_stream::stream! {
        yield Ok::<_, Infallible>(mapped);
    };
    let body =
        axum::body::to_bytes(Sse::new(stream).into_response().into_body(), usize::MAX).await?;
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("event: message.delta"));
    assert!(text.contains("\"content\":\"hello\""));

    let tool_start = RuntimeEventRecord {
        schema_version: 1,
        seq: 2,
        timestamp: chrono::Utc::now(),
        thread_id: "thr_test".to_string(),
        turn_id: Some("turn_test".to_string()),
        item_id: Some("item_tool".to_string()),
        event: "item.started".to_string(),
        payload: json!({
            "tool": { "id": "tool_1", "name": "exec_shell", "input": { "cmd": "pwd" } }
        }),
    };
    let mapped = map_compat_stream_event(&tool_start).context("missing tool.started event")?;
    let stream = async_stream::stream! {
        yield Ok::<_, Infallible>(mapped);
    };
    let body =
        axum::body::to_bytes(Sse::new(stream).into_response().into_body(), usize::MAX).await?;
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("event: tool.started"));

    let tool_done = RuntimeEventRecord {
        schema_version: 1,
        seq: 3,
        timestamp: chrono::Utc::now(),
        thread_id: "thr_test".to_string(),
        turn_id: Some("turn_test".to_string()),
        item_id: Some("item_tool".to_string()),
        event: "item.completed".to_string(),
        payload: json!({
            "item": {
                "id": "item_tool",
                "kind": "tool_call",
                "summary": "ok",
                "detail": "done"
            }
        }),
    };
    let mapped = map_compat_stream_event(&tool_done).context("missing tool.completed event")?;
    let stream = async_stream::stream! {
        yield Ok::<_, Infallible>(mapped);
    };
    let body =
        axum::body::to_bytes(Sse::new(stream).into_response().into_body(), usize::MAX).await?;
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("event: tool.completed"));
    assert!(text.contains("\"success\":true"));

    let user_input_required = RuntimeEventRecord {
        schema_version: 1,
        seq: 4,
        timestamp: chrono::Utc::now(),
        thread_id: "thr_test".to_string(),
        turn_id: Some("turn_test".to_string()),
        item_id: None,
        event: "user_input.required".to_string(),
        payload: json!({
            "id": "input_test",
            "request": {
                "questions": [{
                    "header": "Continue",
                    "id": "choice",
                    "question": "Continue?",
                    "options": [
                        { "label": "Yes", "description": "Continue" },
                        { "label": "No", "description": "Stop" }
                    ]
                }]
            },
            "internal_secret": "required-secret",
        }),
    };
    let mapped = map_compat_stream_event(&user_input_required)
        .context("missing user_input.required event")?;
    let stream = async_stream::stream! {
        yield Ok::<_, Infallible>(mapped);
    };
    let body =
        axum::body::to_bytes(Sse::new(stream).into_response().into_body(), usize::MAX).await?;
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("event: user_input.required"));
    assert!(text.contains("\"input_id\":\"input_test\""));
    assert!(text.contains("\"status\":\"required\""));
    assert!(!text.contains("required-secret"));

    let user_input_answered = RuntimeEventRecord {
        schema_version: 1,
        seq: 5,
        timestamp: chrono::Utc::now(),
        thread_id: "thr_test".to_string(),
        turn_id: Some("turn_test".to_string()),
        item_id: None,
        event: "user_input.answered".to_string(),
        payload: json!({
            "input_id": "input_test",
            "answers": [{ "id": "choice", "value": "answer-secret" }],
        }),
    };
    let mapped = map_compat_stream_event(&user_input_answered)
        .context("missing user_input.answered event")?;
    let stream = async_stream::stream! {
        yield Ok::<_, Infallible>(mapped);
    };
    let body =
        axum::body::to_bytes(Sse::new(stream).into_response().into_body(), usize::MAX).await?;
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("event: user_input.answered"));
    assert!(text.contains("\"status\":\"submitted\""));
    assert!(!text.contains("answer-secret"));
    assert!(!text.contains("\"answers\""));

    let user_input_canceled = RuntimeEventRecord {
        schema_version: 1,
        seq: 6,
        timestamp: chrono::Utc::now(),
        thread_id: "thr_test".to_string(),
        turn_id: Some("turn_test".to_string()),
        item_id: None,
        event: "user_input.canceled".to_string(),
        payload: json!({ "id": "input_test", "terminal": true }),
    };
    let mapped = map_compat_stream_event(&user_input_canceled)
        .context("missing user_input.canceled event")?;
    let stream = async_stream::stream! {
        yield Ok::<_, Infallible>(mapped);
    };
    let body =
        axum::body::to_bytes(Sse::new(stream).into_response().into_body(), usize::MAX).await?;
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("event: user_input.canceled"));
    assert!(text.contains("\"status\":\"canceled\""));
    assert!(text.contains("\"terminal\":true"));

    let approval_required = RuntimeEventRecord {
        schema_version: 1,
        seq: 7,
        timestamp: chrono::Utc::now(),
        thread_id: "thr_test".to_string(),
        turn_id: Some("turn_test".to_string()),
        item_id: None,
        event: "approval.required".to_string(),
        payload: json!({
            "approval_id": "approval_test",
            "tool_name": "exec_command",
            "description": "Run tests",
            "input": { "token": "approval-secret" },
        }),
    };
    let mapped =
        map_compat_stream_event(&approval_required).context("missing approval.required event")?;
    let stream = async_stream::stream! {
        yield Ok::<_, Infallible>(mapped);
    };
    let body =
        axum::body::to_bytes(Sse::new(stream).into_response().into_body(), usize::MAX).await?;
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("event: approval.required"));
    assert!(text.contains("\"approval_id\":\"approval_test\""));
    assert!(!text.contains("approval-secret"));

    let approval_decided = RuntimeEventRecord {
        schema_version: 1,
        seq: 8,
        timestamp: chrono::Utc::now(),
        thread_id: "thr_test".to_string(),
        turn_id: Some("turn_test".to_string()),
        item_id: None,
        event: "approval.decided".to_string(),
        payload: json!({
            "approval_id": "approval_test",
            "decision": "allow",
            "remember": false,
            "internal_secret": "approval-decision-secret",
        }),
    };
    let mapped =
        map_compat_stream_event(&approval_decided).context("missing approval.decided event")?;
    let stream = async_stream::stream! {
        yield Ok::<_, Infallible>(mapped);
    };
    let body =
        axum::body::to_bytes(Sse::new(stream).into_response().into_body(), usize::MAX).await?;
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("event: approval.decided"));
    assert!(text.contains("\"decision\":\"allow\""));
    assert!(!text.contains("approval-decision-secret"));

    // A resolution forced by a turn interrupt or turn teardown must
    // surface the `cancelled` flag so clients clear the pending approval
    // UI instead of reporting a refusal.
    let approval_cancelled = RuntimeEventRecord {
        schema_version: 1,
        seq: 9,
        timestamp: chrono::Utc::now(),
        thread_id: "thr_test".to_string(),
        turn_id: Some("turn_test".to_string()),
        item_id: None,
        event: "approval.decided".to_string(),
        payload: json!({
            "approval_id": "approval_test",
            "decision": "deny",
            "cancelled": true,
        }),
    };
    let mapped = map_compat_stream_event(&approval_cancelled)
        .context("missing cancelled approval.decided event")?;
    let stream = async_stream::stream! {
        yield Ok::<_, Infallible>(mapped);
    };
    let body =
        axum::body::to_bytes(Sse::new(stream).into_response().into_body(), usize::MAX).await?;
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("event: approval.decided"));
    assert!(
        text.contains("\"cancelled\":true"),
        "cancelled resolutions must project the flag through the compat stream: {text}"
    );

    // The compat stream must also keep surfacing `approval.timeout` (the
    // decision-budget resolution, and replays of journals written by older
    // builds that only recorded the legacy event).
    let legacy_timeout = RuntimeEventRecord {
        schema_version: 1,
        seq: 10,
        timestamp: chrono::Utc::now(),
        thread_id: "thr_test".to_string(),
        turn_id: Some("turn_test".to_string()),
        item_id: None,
        event: "approval.timeout".to_string(),
        payload: json!({
            "approval_id": "approval_legacy",
            "timeout_secs": 300,
        }),
    };
    let mapped =
        map_compat_stream_event(&legacy_timeout).context("missing approval.timeout event")?;
    let stream = async_stream::stream! {
        yield Ok::<_, Infallible>(mapped);
    };
    let body =
        axum::body::to_bytes(Sse::new(stream).into_response().into_body(), usize::MAX).await?;
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("event: approval.timeout"));
    assert!(text.contains("\"approval_id\":\"approval_legacy\""));
    assert!(text.contains("\"timeout_secs\":300"));

    let unknown = RuntimeEventRecord {
        schema_version: 1,
        seq: 11,
        timestamp: chrono::Utc::now(),
        thread_id: "thr_test".to_string(),
        turn_id: Some("turn_test".to_string()),
        item_id: None,
        event: "item.delta".to_string(),
        payload: json!({
            "kind": "context_compaction",
            "delta": "ignored",
        }),
    };
    assert!(map_compat_stream_event(&unknown).is_none());
    Ok(())
}

/// Renders one mapped SSE frame and returns its `data:` payload as JSON.
async fn sse_frame_payload(event: SseEvent) -> Result<Value> {
    let stream = async_stream::stream! {
        yield Ok::<_, Infallible>(event);
    };
    let body =
        axum::body::to_bytes(Sse::new(stream).into_response().into_body(), usize::MAX).await?;
    let text = String::from_utf8_lossy(&body).into_owned();
    let data = text
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .context("SSE frame had no data line")?;
    Ok(serde_json::from_str(data)?)
}

fn approval_event_record(seq: u64, event: &str, payload: Value) -> RuntimeEventRecord {
    RuntimeEventRecord {
        schema_version: 1,
        seq,
        timestamp: chrono::Utc::now(),
        thread_id: "thr_test".to_string(),
        turn_id: Some("turn_test".to_string()),
        item_id: None,
        event: event.to_string(),
        payload,
    }
}

/// Every approval frame an SSE-only client sees identifies the approval by the
/// runtime-minted capability. A provider tool-call ID must never surface in
/// `approval_id` or its legacy `id` alias, because those are the fields clients
/// echo back to `POST /v1/approvals/{id}`.
#[tokio::test]
async fn approval_sse_frames_identify_approvals_by_the_opaque_capability() -> Result<()> {
    const OPAQUE: &str = "approval_0123456789abcdef";
    const RAW: &str = "call_1";

    // Each arm reads only the identity fields; the rest are the shape a real
    // payload carries and are ignored by the arms that do not use them.
    for (seq, event) in ["approval.required", "approval.decided", "approval.timeout"]
        .into_iter()
        .enumerate()
    {
        let record = approval_event_record(
            seq as u64 + 1,
            event,
            json!({
                "id": OPAQUE,
                "approval_id": OPAQUE,
                "tool_call_id": RAW,
                "tool_name": "exec_command",
                "description": "Run tests",
                "decision": "allow",
                "timeout_secs": 300,
            }),
        );
        let mapped =
            map_compat_stream_event(&record).with_context(|| format!("missing {event} frame"))?;
        let frame = sse_frame_payload(mapped).await?;
        assert_eq!(frame["approval_id"], OPAQUE, "{event} approval_id");
        assert_eq!(frame["id"], OPAQUE, "{event} legacy id alias");
        assert_ne!(frame["approval_id"], RAW, "{event} must not publish raw id");
        assert_eq!(frame["thread_id"], "thr_test");
    }
    Ok(())
}

/// Companion to the test above: the raw tool-call ID is the only correlator an
/// SSE-only client can use to attach an approval prompt to the tool row it
/// gates, so the projection should forward it alongside the capability.
#[tokio::test]
async fn approval_sse_frames_forward_the_tool_call_correlator() -> Result<()> {
    const OPAQUE: &str = "approval_0123456789abcdef";
    const RAW: &str = "call_1";

    for (seq, event) in ["approval.required", "approval.decided", "approval.timeout"]
        .into_iter()
        .enumerate()
    {
        let record = approval_event_record(
            seq as u64 + 1,
            event,
            json!({
                "id": OPAQUE,
                "approval_id": OPAQUE,
                "tool_call_id": RAW,
                "tool_name": "exec_command",
                "decision": "allow",
            }),
        );
        let mapped =
            map_compat_stream_event(&record).with_context(|| format!("missing {event} frame"))?;
        let frame = sse_frame_payload(mapped).await?;
        assert_eq!(frame["tool_call_id"], RAW, "{event} tool_call_id");
        assert_eq!(frame["approval_id"], OPAQUE, "{event} approval_id");
    }
    Ok(())
}

#[tokio::test]
async fn stream_endpoint_remains_backward_compatible() -> Result<()> {
    let Some((addr, runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    // Create a thread and install a mock engine so /v1/stream doesn't call the real API.
    let created: serde_json::Value = client
        .post(format!("http://{addr}/v1/threads"))
        .json(&json!({}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let thread_id = created["id"]
        .as_str()
        .context("missing thread id")?
        .to_string();

    let harness = crate::core::engine::mock_engine_handle();
    runtime_threads
        .install_test_engine(&thread_id, harness.handle.clone())
        .await?;
    let mut rx_op = harness.rx_op;
    let tx_event = harness.tx_event;
    tokio::spawn(async move {
        if !matches!(rx_op.recv().await, Some(Op::SendMessage(TurnSpec { .. }))) {
            return;
        }
        let _ = tx_event
            .send(EngineEvent::TurnStarted {
                turn_id: "mock_stream".to_string(),
                created_at: chrono::Utc::now(),
                route: None,
                submission_id: None,
            })
            .await;
        let _ = tx_event
            .send(EngineEvent::MessageStarted { index: 0 })
            .await;
        let _ = tx_event
            .send(EngineEvent::MessageDelta {
                index: 0,
                content: "streamed".to_string(),
            })
            .await;
        let _ = tx_event
            .send(EngineEvent::MessageComplete { index: 0 })
            .await;
        let _ = tx_event
            .send(EngineEvent::TurnComplete {
                usage: Usage {
                    input_tokens: 4,
                    output_tokens: 2,
                    ..Usage::default()
                },
                parent_route_usage: Usage {
                    input_tokens: 4,
                    output_tokens: 2,
                    ..Usage::default()
                },
                routed_usage_dropped_records: 0,
                status: TurnOutcomeStatus::Completed,
                error: None,
                tool_catalog: None,
                base_url: None,
            })
            .await;
    });

    // Start the turn and consume events via the SSE endpoint.
    let turn_start: serde_json::Value = client
        .post(format!("http://{addr}/v1/threads/{thread_id}/turns"))
        .json(&json!({ "prompt": "compatibility stream" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let turn_id = turn_start["turn"]["id"]
        .as_str()
        .context("missing turn id")?
        .to_string();

    let _ =
        wait_for_terminal_turn_status(&client, addr, &thread_id, &turn_id, Duration::from_secs(10))
            .await?;

    // Verify that the persisted events include the expected turn lifecycle events.
    let events = runtime_threads.events_since(&thread_id, None)?;
    assert!(
        events.iter().any(|ev| ev.event == "turn.started"),
        "expected turn.started event"
    );
    assert!(
        events.iter().any(|ev| ev.event == "turn.completed"),
        "expected turn.completed event"
    );

    // Verify the SSE endpoint returns event-stream content type.
    let events_resp = client
        .get(format!(
            "http://{addr}/v1/threads/{thread_id}/events?since_seq=0"
        ))
        .send()
        .await?
        .error_for_status()?;
    let content_type = events_resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(content_type.starts_with("text/event-stream"));

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn session_get_returns_404_for_missing_id() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let resp = client
        .get(format!("http://{addr}/v1/sessions/nonexistent_id"))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn session_endpoints_reject_invalid_id() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let get_resp = client
        .get(format!("http://{addr}/v1/sessions/invalid%20id"))
        .send()
        .await?;
    assert_eq!(get_resp.status(), StatusCode::BAD_REQUEST);

    let resume_resp = client
        .post(format!(
            "http://{addr}/v1/sessions/invalid%20id/resume-thread"
        ))
        .json(&json!({}))
        .send()
        .await?;
    assert_eq!(resume_resp.status(), StatusCode::BAD_REQUEST);

    let delete_resp = client
        .delete(format!("http://{addr}/v1/sessions/invalid%20id"))
        .send()
        .await?;
    assert_eq!(delete_resp.status(), StatusCode::BAD_REQUEST);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn session_resume_thread_returns_404_for_missing_session() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let resp = client
        .post(format!(
            "http://{addr}/v1/sessions/nonexistent_session/resume-thread"
        ))
        .json(&json!({}))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn session_resume_thread_returns_400_when_saved_custom_provider_was_removed() -> Result<()> {
    let root = std::env::temp_dir().join(format!(
        "codewhale-session-removed-provider-{}",
        Uuid::new_v4()
    ));
    let sessions_dir = root.join("sessions");
    fs::create_dir_all(&sessions_dir)?;
    let session = json!({
        "schema_version": 1,
        "metadata": {
            "id": "sess_removed_custom_provider",
            "title": "Removed custom provider",
            "created_at": "2025-01-01T00:00:00Z",
            "updated_at": "2025-01-01T00:10:00Z",
            "message_count": 1,
            "total_tokens": 10,
            "model": "local-code-model",
            "model_provider": "lm-studio",
            "workspace": "/tmp/test",
            "mode": "agent"
        },
        "messages": [{
            "role": "user",
            "content": [{ "type": "text", "text": "Resume me" }]
        }],
        "system_prompt": null
    });
    fs::write(
        sessions_dir.join("sess_removed_custom_provider.json"),
        serde_json::to_string_pretty(&session)?,
    )?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root(root, sessions_dir).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let resp = client
        .post(format!(
            "http://{addr}/v1/sessions/sess_removed_custom_provider/resume-thread"
        ))
        .json(&json!({}))
        .send()
        .await?;

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = resp.json().await?;
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("[providers.lm-studio]"), "{message}");
    assert!(message.contains("will not fall back"), "{message}");

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn session_resume_thread_creates_thread_from_saved_session() -> Result<()> {
    let root = std::env::temp_dir().join(format!("deepseek-session-resume-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    fs::create_dir_all(&sessions_dir)?;
    let session = json!({
        "schema_version": 1,
        "metadata": {
            "id": "sess_test_resume",
            "title": "Test resume session",
            "created_at": "2025-01-01T00:00:00Z",
            "updated_at": "2025-01-01T00:10:00Z",
            "message_count": 2,
            "total_tokens": 100,
            "model": "deepseek-v4-pro",
            "workspace": "/tmp/test",
            "mode": "agent"
        },
        "messages": [
            {
                "role": "user",
                "content": [{ "type": "text", "text": "Hello, world!" }]
            },
            {
                "role": "assistant",
                "content": [{ "type": "text", "text": "Hello! How can I help you?" }]
            }
        ],
        "system_prompt": null
    });
    fs::write(
        sessions_dir.join("sess_test_resume.json"),
        serde_json::to_string_pretty(&session)?,
    )?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root(root.clone(), sessions_dir.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let resp = client
        .post(format!(
            "http://{addr}/v1/sessions/sess_test_resume/resume-thread"
        ))
        .json(&json!({ "model": "deepseek-v4-pro" }))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let resumed: serde_json::Value = resp.json().await?;
    assert_eq!(resumed["session_id"], "sess_test_resume");
    assert_eq!(resumed["message_count"], 2);

    let thread_id = resumed["thread_id"]
        .as_str()
        .context("missing resumed thread id")?;
    let detail: serde_json::Value = client
        .get(format!("http://{addr}/v1/threads/{thread_id}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(detail["thread"]["id"], thread_id);
    assert_eq!(detail["thread"]["model_provider"], "deepseek");
    assert_eq!(detail["thread"]["workspace"], "/tmp/test");
    assert_eq!(detail["turns"].as_array().map_or(0, Vec::len), 1);
    assert_eq!(detail["items"].as_array().map_or(0, Vec::len), 2);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn session_resume_thread_reuses_the_thread_that_already_holds_it() -> Result<()> {
    // Resuming a conversation that is already open must hand back the thread
    // holding it: minting a second one per visit is what made "continue this
    // conversation" add a rail row for history the user already had open.
    let _env = lock_test_env();
    let dir = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("CODEWHALE_HOME", dir.path());
    let root = dir.path().join("server");
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir)?;
    let session = json!({
        "schema_version": 1,
        "metadata": {
            "id": "sess_reuse_resume",
            "title": "Reused resume session",
            "created_at": "2025-01-01T00:00:00Z",
            "updated_at": "2025-01-01T00:10:00Z",
            "message_count": 2,
            "total_tokens": 100,
            "model": "deepseek-v4-pro",
            "workspace": "/tmp/test",
            "mode": "agent"
        },
        "messages": [
            {
                "role": "user",
                "content": [{ "type": "text", "text": "Hello, world!" }]
            },
            {
                "role": "assistant",
                "content": [{ "type": "text", "text": "Hello! How can I help you?" }]
            }
        ],
        "system_prompt": null
    });
    fs::write(
        sessions_dir.join("sess_reuse_resume.json"),
        serde_json::to_string_pretty(&session)?,
    )?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root(root.clone(), sessions_dir.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let url = format!("http://{addr}/v1/sessions/sess_reuse_resume/resume-thread");

    let first = client.post(&url).json(&json!({})).send().await?;
    assert_eq!(first.status(), StatusCode::CREATED);
    let first: serde_json::Value = first.json().await?;
    let thread_id = first["thread_id"]
        .as_str()
        .context("missing resumed thread id")?
        .to_string();

    let second = client.post(&url).json(&json!({})).send().await?;
    assert_eq!(second.status(), StatusCode::OK);
    let second: serde_json::Value = second.json().await?;
    assert_eq!(second["thread_id"], thread_id.as_str());
    assert_eq!(second["session_id"], "sess_reuse_resume");
    assert_eq!(second["message_count"], 2);

    let threads: Vec<serde_json::Value> = client
        .get(format!("http://{addr}/v1/threads"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        threads.len(),
        1,
        "resuming the same conversation twice must not add a second thread"
    );

    handle.abort();
    Ok(())
}

/// A saved conversation fixture with a system prompt, written under `id`.
fn write_resumable_session_fixture(sessions_dir: &std::path::Path, id: &str) -> Result<()> {
    let session = json!({
        "schema_version": 1,
        "metadata": {
            "id": id,
            "title": "Resumable fixture",
            "created_at": "2025-01-01T00:00:00Z",
            "updated_at": "2025-01-01T00:10:00Z",
            "message_count": 2,
            "total_tokens": 100,
            "model": "deepseek-v4-pro",
            "workspace": "/tmp/test",
            "mode": "agent"
        },
        "messages": [
            {
                "role": "user",
                "content": [{ "type": "text", "text": "Hello, world!" }]
            },
            {
                "role": "assistant",
                "content": [{ "type": "text", "text": "Hello! How can I help you?" }]
            }
        ],
        "system_prompt": "You are the original conversation's system prompt."
    });
    fs::write(
        sessions_dir.join(format!("{id}.json")),
        serde_json::to_string_pretty(&session)?,
    )?;
    Ok(())
}

#[tokio::test]
async fn session_export_of_a_resumed_thread_leaves_the_source_document_untouched() -> Result<()> {
    // A resumed thread is bound to the original saved session. Exporting it
    // writes the thread's own document; rewriting the original from the
    // thread's projection would drop what the projection does not carry.
    let _env = lock_test_env();
    let dir = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("CODEWHALE_HOME", dir.path());
    let root = dir.path().join("server");
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir)?;
    write_resumable_session_fixture(&sessions_dir, "sess_export_source")?;

    let Some((addr, runtime_threads, handle)) =
        spawn_test_server_with_root(root.clone(), sessions_dir.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let resumed = client
        .post(format!(
            "http://{addr}/v1/sessions/sess_export_source/resume-thread"
        ))
        .json(&json!({}))
        .send()
        .await?;
    assert_eq!(resumed.status(), StatusCode::CREATED);
    let resumed: serde_json::Value = resumed.json().await?;
    let thread_id = resumed["thread_id"]
        .as_str()
        .context("missing resumed thread id")?
        .to_string();
    let source_path = sessions_dir.join("sess_export_source.json");
    let source_before = fs::read(&source_path)?;

    let exported = client
        .post(format!("http://{addr}/v1/sessions"))
        .json(&json!({ "thread_id": thread_id }))
        .send()
        .await?;
    assert_eq!(exported.status(), StatusCode::CREATED);
    let exported: serde_json::Value = exported.json().await?;
    let expected_id = crate::runtime_threads::thread_session_id(&thread_id);
    assert_eq!(exported["session_id"], expected_id.as_str());
    assert_eq!(
        fs::read(&source_path)?,
        source_before,
        "export must not rewrite the document the thread was resumed from"
    );
    assert_eq!(
        runtime_threads.get_thread(&thread_id).await?.session_id,
        Some(expected_id.clone())
    );

    // A second export updates the thread's own document, still not the source.
    let again = client
        .post(format!("http://{addr}/v1/sessions"))
        .json(&json!({ "thread_id": thread_id }))
        .send()
        .await?;
    assert_eq!(again.status(), StatusCode::OK);
    let again: serde_json::Value = again.json().await?;
    assert_eq!(again["session_id"], expected_id.as_str());
    assert_eq!(fs::read(&source_path)?, source_before);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn session_resume_thread_mints_a_fresh_thread_when_the_document_grew() -> Result<()> {
    // The thread holding a conversation shows its checkpointed prefix plus its
    // own turns. Messages another writer appended after the bind are not in
    // that thread, so resuming the grown document must not hand it back.
    let _env = lock_test_env();
    let dir = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("CODEWHALE_HOME", dir.path());
    let root = dir.path().join("server");
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir)?;
    write_resumable_session_fixture(&sessions_dir, "sess_grown_resume")?;

    let Some((addr, runtime_threads, handle)) =
        spawn_test_server_with_root(root.clone(), sessions_dir.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let url = format!("http://{addr}/v1/sessions/sess_grown_resume/resume-thread");
    let first = client.post(&url).json(&json!({})).send().await?;
    assert_eq!(first.status(), StatusCode::CREATED);
    let first: serde_json::Value = first.json().await?;
    let first_thread = first["thread_id"]
        .as_str()
        .context("missing resumed thread id")?
        .to_string();

    // The TUI keeps working on the same conversation and autosaves.
    let manager = crate::session_manager::SessionManager::new(sessions_dir.clone())?;
    let mut grown = manager.load_session("sess_grown_resume")?;
    for (role, text) in [
        (Role::User, "One more question from the terminal"),
        (Role::Assistant, "And its answer"),
    ] {
        grown.messages.push(Message {
            role,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        });
    }
    grown.metadata.message_count = grown.messages.len();
    manager.save_session(&grown)?;

    let second = client.post(&url).json(&json!({})).send().await?;
    assert_eq!(second.status(), StatusCode::CREATED);
    let second: serde_json::Value = second.json().await?;
    let second_thread = second["thread_id"]
        .as_str()
        .context("missing resumed thread id")?
        .to_string();
    assert_ne!(second_thread, first_thread);
    assert_eq!(second["message_count"], 4);
    let detail = runtime_threads.get_thread_detail(&second_thread).await?;
    let texts: Vec<String> = detail
        .items
        .iter()
        .flat_map(|item| [Some(item.summary.clone()), item.detail.clone()])
        .flatten()
        .collect();
    assert!(
        texts
            .iter()
            .any(|text| text.contains("One more question from the terminal")),
        "the fresh thread must hold the appended messages: {texts:?}"
    );

    // Resuming again finds the fresh thread, whose checkpoint covers it all.
    let third = client.post(&url).json(&json!({})).send().await?;
    assert_eq!(third.status(), StatusCode::OK);
    let third: serde_json::Value = third.json().await?;
    assert_eq!(third["thread_id"], second_thread.as_str());

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn session_resume_thread_ignores_an_archived_thread_holding_the_session() -> Result<()> {
    // Archiving is how a conversation is taken off the rail. Handing the
    // archived thread back to a client that asked to continue the conversation
    // would move it somewhere the user cannot see it, so a new thread is the
    // honest answer even though the binding exists.
    let _env = lock_test_env();
    let dir = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("CODEWHALE_HOME", dir.path());
    let root = dir.path().join("server");
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir)?;
    let session = json!({
        "schema_version": 1,
        "metadata": {
            "id": "sess_archived_reuse",
            "title": "Archived holder",
            "created_at": "2025-01-01T00:00:00Z",
            "updated_at": "2025-01-01T00:10:00Z",
            "message_count": 2,
            "total_tokens": 100,
            "model": "deepseek-v4-pro",
            "workspace": "/tmp/test",
            "mode": "agent"
        },
        "messages": [
            {
                "role": "user",
                "content": [{ "type": "text", "text": "Hello, world!" }]
            },
            {
                "role": "assistant",
                "content": [{ "type": "text", "text": "Hello! How can I help you?" }]
            }
        ],
        "system_prompt": null
    });
    fs::write(
        sessions_dir.join("sess_archived_reuse.json"),
        serde_json::to_string_pretty(&session)?,
    )?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root(root.clone(), sessions_dir.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let url = format!("http://{addr}/v1/sessions/sess_archived_reuse/resume-thread");

    let first = client.post(&url).json(&json!({})).send().await?;
    assert_eq!(first.status(), StatusCode::CREATED);
    let first: serde_json::Value = first.json().await?;
    let archived_id = first["thread_id"]
        .as_str()
        .context("missing resumed thread id")?
        .to_string();

    client
        .patch(format!("http://{addr}/v1/threads/{archived_id}"))
        .json(&json!({ "archived": true }))
        .send()
        .await?
        .error_for_status()?;

    let second = client.post(&url).json(&json!({})).send().await?;
    assert_eq!(second.status(), StatusCode::CREATED);
    let second: serde_json::Value = second.json().await?;
    assert_ne!(
        second["thread_id"].as_str(),
        Some(archived_id.as_str()),
        "an archived thread is not somewhere to send the user back to"
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn session_create_from_completed_thread_saves_messages() -> Result<()> {
    let root = std::env::temp_dir().join(format!("deepseek-thread-session-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    let Some((addr, runtime_threads, handle)) =
        spawn_test_server_with_root(root.clone(), sessions_dir).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let created: serde_json::Value = client
        .post(format!("http://{addr}/v1/threads"))
        .json(&json!({
            "model": "deepseek-v4-pro",
            "mode": "plan",
            "workspace": root.join("workspace")
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let thread_id = created["id"]
        .as_str()
        .context("missing thread id")?
        .to_string();

    let patched: serde_json::Value = client
        .patch(format!("http://{addr}/v1/threads/{thread_id}"))
        .json(&json!({ "title": "Thread title fallback" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(patched["title"], "Thread title fallback");

    runtime_threads
        .seed_thread_from_messages(
            &thread_id,
            &[
                Message {
                    role: Role::User,
                    content: vec![ContentBlock::Text {
                        text: "Please save this runtime thread".to_string(),
                        cache_control: None,
                    }],
                },
                Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::Text {
                        text: "Saved replies should round-trip.".to_string(),
                        cache_control: None,
                    }],
                },
            ],
        )
        .await?;

    let resp = client
        .post(format!("http://{addr}/v1/sessions"))
        .json(&json!({ "thread_id": thread_id }))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let saved: serde_json::Value = resp.json().await?;
    assert_eq!(saved["thread_id"], thread_id);
    assert_eq!(saved["message_count"], 2);
    assert_eq!(saved["title"], "Thread title fallback");
    let saved_session_handle = saved["session_id"]
        .as_str()
        .context("missing session id")?
        .to_string();

    let session_manager = crate::session_manager::SessionManager::new(root.join("sessions"))?;
    let created_session = session_manager.load_session_by_prefix(&saved_session_handle)?;
    assert_eq!(created_session.metadata.title, "Thread title fallback");
    assert_eq!(created_session.metadata.model, "deepseek-v4-pro");
    assert_eq!(created_session.metadata.mode.as_deref(), Some("plan"));
    assert_eq!(created_session.metadata.message_count, 2);
    assert_eq!(created_session.messages[0].role, "user");
    assert_eq!(created_session.messages[1].role, "assistant");

    let mut endpoint_session = crate::session_manager::create_saved_session_with_id_and_mode(
        "sess_endpoint_fetch".to_string(),
        &created_session.messages,
        "deepseek-v4-pro",
        &root,
        0,
        None,
        Some("plan"),
    );
    endpoint_session.metadata.title = "Thread title fallback".to_string();
    session_manager.save_session(&endpoint_session)?;

    let detail: serde_json::Value = client
        .get(format!("http://{addr}/v1/sessions/sess_endpoint_fetch"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(detail["metadata"]["title"], "Thread title fallback");
    assert_eq!(detail["metadata"]["model"], "deepseek-v4-pro");
    assert_eq!(detail["metadata"]["mode"], "plan");
    assert_eq!(detail["metadata"]["message_count"], 2);
    assert_eq!(detail["messages"][0]["role"], "user");
    assert_eq!(
        detail["messages"][0]["content"][0]["text"],
        "Please save this runtime thread"
    );
    assert_eq!(detail["messages"][1]["role"], "assistant");

    // Export is idempotent (#6144): a second POST updates the thread's own
    // document in place instead of minting another one and leaving the first
    // unreferenced.
    let documents_before = std::fs::read_dir(root.join("sessions"))?
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .count();
    let again = client
        .post(format!("http://{addr}/v1/sessions"))
        .json(&json!({
            "thread_id": thread_id,
            "title": "Manual saved title"
        }))
        .send()
        .await?;
    assert_eq!(again.status(), StatusCode::OK);
    let manual_title: serde_json::Value = again.json().await?;
    assert_eq!(manual_title["title"], "Manual saved title");
    assert_eq!(manual_title["session_id"], saved_session_handle);
    let documents_after = std::fs::read_dir(root.join("sessions"))?
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .count();
    assert_eq!(documents_after, documents_before);
    assert_eq!(
        session_manager
            .load_session(&saved_session_handle)?
            .metadata
            .title,
        "Manual saved title"
    );
    assert_eq!(
        runtime_threads.get_thread(&thread_id).await?.session_id,
        Some(saved_session_handle.clone())
    );

    // Deleting the document unbinds the thread, which keeps its turns.
    let deleted = client
        .delete(format!("http://{addr}/v1/sessions/{saved_session_handle}"))
        .send()
        .await?;
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        runtime_threads.get_thread(&thread_id).await?.session_id,
        None
    );

    handle.abort();
    Ok(())
}

/// A crash between saving the exported document and binding the thread left
/// that document unreferenced, and the retry minted another. The export id is
/// derived from the thread, so the retry finds and binds the one it wrote.
#[tokio::test]
async fn session_export_retry_after_a_crash_binds_the_document_it_wrote() -> Result<()> {
    let root = std::env::temp_dir().join(format!("codewhale-export-retry-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    let Some((addr, runtime_threads, handle)) =
        spawn_test_server_with_root(root.clone(), sessions_dir.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let thread = runtime_threads
        .create_thread(crate::runtime_threads::CreateThreadRequest {
            workspace: Some(root.join("workspace")),
            ..Default::default()
        })
        .await?;
    let messages = [
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "export me".to_string(),
                cache_control: None,
            }],
        },
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "exported".to_string(),
                cache_control: None,
            }],
        },
    ];
    runtime_threads
        .seed_thread_from_messages(&thread.id, &messages)
        .await?;
    // The first attempt got as far as the document, then died.
    let expected_id = crate::runtime_threads::thread_session_id(&thread.id);
    let manager = crate::session_manager::SessionManager::new(sessions_dir.clone())?;
    manager.save_session(
        &crate::session_manager::create_saved_session_with_id_and_mode(
            expected_id.clone(),
            &messages,
            "deepseek-v4-pro",
            &root,
            0,
            None,
            None,
        ),
    )?;
    assert_eq!(
        runtime_threads.get_thread(&thread.id).await?.session_id,
        None
    );

    let retry = client
        .post(format!("http://{addr}/v1/sessions"))
        .json(&json!({ "thread_id": thread.id }))
        .send()
        .await?;
    assert_eq!(retry.status(), StatusCode::OK);
    let retry: serde_json::Value = retry.json().await?;
    assert_eq!(retry["session_id"], expected_id);
    // Later requests address the session by the id the server returned.
    let session_id = retry["session_id"]
        .as_str()
        .expect("session_id is a string")
        .to_string();
    assert_eq!(
        runtime_threads.get_thread(&thread.id).await?.session_id,
        Some(expected_id.clone())
    );
    let documents = manager.list_sessions()?;
    assert_eq!(documents.len(), 1, "no duplicate document: {documents:?}");

    // Another thread may not rebind itself onto this thread's document.
    let other = runtime_threads
        .create_thread(crate::runtime_threads::CreateThreadRequest {
            workspace: Some(root.join("workspace")),
            ..Default::default()
        })
        .await?;
    let conflict = client
        .put(format!("http://{addr}/v1/sessions"))
        .json(&json!({ "thread_id": other.id, "session_id": expected_id }))
        .send()
        .await?;
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    assert_eq!(
        runtime_threads.get_thread(&other.id).await?.session_id,
        None
    );

    // A session another process holds open cannot be deleted from here. The
    // lease is an OS lock on its own open file description, which is exactly
    // what a second process holding it looks like.
    let lease_path = sessions_dir
        .join(".late-usage")
        .join(format!("{expected_id}.live"));
    std::fs::create_dir_all(lease_path.parent().unwrap())?;
    let lease = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lease_path)?;
    assert!(crate::runtime_threads::try_lock_file_exclusive(&lease)?);
    let refused = client
        .delete(format!("http://{addr}/v1/sessions/{session_id}"))
        .send()
        .await?;
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    assert!(manager.session_document_exists(&expected_id));
    drop(lease);
    let deleted = client
        .delete(format!("http://{addr}/v1/sessions/{session_id}"))
        .send()
        .await?;
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn session_create_from_thread_returns_404_for_missing_thread() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let resp = client
        .post(format!("http://{addr}/v1/sessions"))
        .json(&json!({ "thread_id": "thr_missing" }))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    handle.abort();
    Ok(())
}

/// `PUT /v1/sessions` with no `session_id` persists the conversation's own id,
/// not a fresh uuid, so one conversation keeps one document across saves.
/// (A Runtime thread's engine runs under the thread id; the mock states the
/// id so the test pins only the save contract. Snapshot ownership no longer
/// depends on this binding — see the #6621 tests below.)
#[tokio::test]
async fn session_save_without_an_id_keeps_the_conversation_that_owns_the_snapshots() -> Result<()> {
    let root = std::env::temp_dir().join(format!("deepseek-session-identity-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    let Some((addr, runtime_threads, handle)) =
        spawn_test_server_with_root(root.clone(), sessions_dir.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let created: serde_json::Value = client
        .post(format!("http://{addr}/v1/threads"))
        .json(&json!({
            "model": "deepseek-v4-pro",
            "mode": "agent",
            "workspace": root.join("workspace")
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let thread_id = created["id"]
        .as_str()
        .context("missing thread id")?
        .to_string();

    // A new conversation's id is generated inside the engine, so only a mock
    // can state it. It stands in for the id the real engine stamps on every
    // snapshot the conversation takes.
    const CONVERSATION_ID: &str = "3f9c7b1e-58b4-4d2a-9f5a-8a1e0c6d2b70";
    let harness = crate::core::engine::mock_engine_handle();
    runtime_threads
        .install_test_engine(&thread_id, harness.handle.clone())
        .await?;
    let mut rx_op = harness.rx_op;
    tokio::spawn(async move {
        while let Some(op) = rx_op.recv().await {
            if let Op::GetSessionSnapshot { tx } = op {
                let snapshot = crate::core::ops::SessionSnapshot {
                    session_id: CONVERSATION_ID.to_string(),
                    messages: vec![Message {
                        role: Role::User,
                        content: vec![ContentBlock::Text {
                            text: "One conversation, one id".to_string(),
                            cache_control: None,
                        }],
                    }],
                    total_tokens: 7,
                    model: "deepseek-v4-pro".to_string(),
                    model_provider: "deepseek".to_string(),
                    model_provider_id: None,
                    workspace: root.clone(),
                    system_prompt: None,
                    mode: "agent".to_string(),
                };
                if let Some(tx) = tx.lock().ok().and_then(|mut guard| guard.take()) {
                    let _ = tx.send(snapshot);
                }
            }
        }
    });

    let saved: serde_json::Value = client
        .put(format!("http://{addr}/v1/sessions"))
        .json(&json!({ "thread_id": thread_id }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        saved["session_id"], CONVERSATION_ID,
        "a save that names no session must persist the conversation's own id"
    );

    // The thread is bound to that one document.
    let detail: serde_json::Value = client
        .get(format!("http://{addr}/v1/threads/{thread_id}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(detail["thread"]["session_id"], CONVERSATION_ID);

    // Saving again keeps the one document instead of collecting a new one per
    // visit — the drift above was also how one conversation came to hold
    // several session files.
    let documents = || -> Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in fs::read_dir(&sessions_dir)? {
            names.push(entry?.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        Ok(names)
    };
    let before = documents()?;
    let resaved: serde_json::Value = client
        .put(format!("http://{addr}/v1/sessions"))
        .json(&json!({ "thread_id": thread_id }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(resaved["session_id"], CONVERSATION_ID);
    assert_eq!(
        documents()?,
        before,
        "a second save writes the session it already owns, not another one"
    );
    assert!(
        sessions_dir
            .join(format!("{CONVERSATION_ID}.json"))
            .exists()
    );

    handle.abort();
    Ok(())
}

/// Create a thread over HTTP and seed it with one user/assistant turn.
/// Shared setup for the undo/patch-undo/retry endpoint tests.
async fn create_seeded_thread(
    addr: &SocketAddr,
    runtime_threads: &SharedRuntimeThreadManager,
    root: &FsPath,
    user_text: &str,
) -> Result<String> {
    let client = crate::tls::reqwest_client();
    let created: serde_json::Value = client
        .post(format!("http://{addr}/v1/threads"))
        .json(&json!({
            "model": "deepseek-v4-pro",
            "mode": "agent",
            "workspace": root.join("workspace")
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let thread_id = created["id"]
        .as_str()
        .context("missing thread id")?
        .to_string();

    runtime_threads
        .seed_thread_from_messages(
            &thread_id,
            &[
                Message {
                    role: Role::User,
                    content: vec![ContentBlock::Text {
                        text: user_text.to_string(),
                        cache_control: None,
                    }],
                },
                Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::Text {
                        text: "Done — anything else?".to_string(),
                        cache_control: None,
                    }],
                },
            ],
        )
        .await?;
    Ok(thread_id)
}

#[tokio::test]
async fn undo_endpoint_forks_thread_and_returns_original_user_text() -> Result<()> {
    let root = std::env::temp_dir().join(format!("deepseek-undo-endpoint-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    let Some((addr, runtime_threads, handle)) =
        spawn_test_server_with_root(root.clone(), sessions_dir).await?
    else {
        return Ok(());
    };
    let thread_id =
        create_seeded_thread(&addr, &runtime_threads, &root, "Please undo this turn").await?;
    let client = crate::tls::reqwest_client();

    let resp = client
        .post(format!("http://{addr}/v1/threads/{thread_id}/undo"))
        .json(&json!({}))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let undone: serde_json::Value = resp.json().await?;
    assert_eq!(undone["original_user_text"], "Please undo this turn");
    let forked_id = undone["thread"]["id"]
        .as_str()
        .context("missing forked thread id")?;
    assert_ne!(forked_id, thread_id, "undo must fork, not mutate in place");

    // The forked thread has the undone turn removed.
    let detail: serde_json::Value = client
        .get(format!("http://{addr}/v1/threads/{forked_id}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(detail["turns"].as_array().map_or(usize::MAX, Vec::len), 0);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn undo_endpoint_404s_for_missing_thread() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let resp = client
        .post(format!("http://{addr}/v1/threads/thr_missing/undo"))
        .json(&json!({}))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    handle.abort();
    Ok(())
}

#[tokio::test]
async fn fork_at_turn_endpoint_cuts_at_the_named_turn() -> Result<()> {
    // The anchor is a turn id, not a distance, and the fork keeps the turn it
    // names: the branch point is the answer a person pointed at. Resolved one
    // turn off, a three-turn thread still answers 201 with a plausible thread
    // and the wrong prompt in the composer — which is what this pins.
    let root = std::env::temp_dir().join(format!("deepseek-fork-at-turn-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    let Some((addr, runtime_threads, handle)) =
        spawn_test_server_with_root(root.clone(), sessions_dir).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let created: serde_json::Value = client
        .post(format!("http://{addr}/v1/threads"))
        .json(&json!({
            "model": "deepseek-v4-pro",
            "mode": "agent",
            "workspace": root.join("workspace")
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let thread_id = created["id"]
        .as_str()
        .context("missing thread id")?
        .to_string();

    let user = |text: &str| Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: text.to_string(),
            cache_control: None,
        }],
    };
    let reply = |text: &str| Message {
        role: Role::Assistant,
        content: vec![ContentBlock::Text {
            text: text.to_string(),
            cache_control: None,
        }],
    };
    runtime_threads
        .seed_thread_from_messages(
            &thread_id,
            &[
                user("first"),
                reply("one"),
                user("second"),
                reply("two"),
                user("third"),
                reply("three"),
            ],
        )
        .await?;

    // Name the anchor the way a client would: the turn id carrying "second".
    let detail: serde_json::Value = client
        .get(format!("http://{addr}/v1/threads/{thread_id}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let anchor = detail["items"]
        .as_array()
        .context("missing items")?
        .iter()
        .find(|item| item["kind"] == "user_message" && item["detail"] == "second")
        .and_then(|item| item["turn_id"].as_str())
        .context("no turn carries the second prompt")?
        .to_string();

    let resp = client
        .post(format!("http://{addr}/v1/threads/{thread_id}/fork-at-turn"))
        .json(&json!({ "turn_id": anchor }))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let forked: serde_json::Value = resp.json().await?;
    // The prompt that comes back is what was asked *next*: the branch keeps
    // "second" and hands back "third" as the place to continue from.
    assert_eq!(forked["original_user_text"], "third");
    let forked_id = forked["thread"]["id"]
        .as_str()
        .context("missing forked thread id")?
        .to_string();
    assert_ne!(forked_id, thread_id, "forking must not mutate in place");

    let forked_detail: serde_json::Value = client
        .get(format!("http://{addr}/v1/threads/{forked_id}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let forked_turns = forked_detail["turns"]
        .as_array()
        .context("missing forked turns")?
        .iter()
        .map(|turn| {
            turn["input_summary"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        forked_turns,
        vec!["first".to_string(), "second".to_string()]
    );

    // Naming the last turn keeps the whole conversation: the same rule read at
    // the end of the thread, with nothing left to hand back.
    let last_anchor = detail["items"]
        .as_array()
        .context("missing items")?
        .iter()
        .find(|item| item["kind"] == "user_message" && item["detail"] == "third")
        .and_then(|item| item["turn_id"].as_str())
        .context("no turn carries the third prompt")?
        .to_string();
    let resp = client
        .post(format!("http://{addr}/v1/threads/{thread_id}/fork-at-turn"))
        .json(&json!({ "turn_id": last_anchor }))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let whole: serde_json::Value = resp.json().await?;
    assert!(whole["original_user_text"].is_null());
    let whole_id = whole["thread"]["id"]
        .as_str()
        .context("missing whole-thread fork id")?
        .to_string();
    let whole_detail: serde_json::Value = client
        .get(format!("http://{addr}/v1/threads/{whole_id}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        whole_detail["turns"]
            .as_array()
            .map_or(usize::MAX, Vec::len),
        3
    );

    // The source keeps every turn: a fork is a sibling, not an undo.
    let source_detail: serde_json::Value = client
        .get(format!("http://{addr}/v1/threads/{thread_id}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        source_detail["turns"]
            .as_array()
            .map_or(usize::MAX, Vec::len),
        3
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn fork_at_turn_endpoint_rejects_a_non_user_turn() -> Result<()> {
    // An assistant turn is not a fork point: cutting "before" it would
    // silently keep the user prompt it answers, which is not what the picker
    // said. The anchor must be refused rather than rounded to a neighbour.
    let root = std::env::temp_dir().join(format!("deepseek-fork-bad-turn-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    let Some((addr, runtime_threads, handle)) =
        spawn_test_server_with_root(root.clone(), sessions_dir).await?
    else {
        return Ok(());
    };
    let thread_id =
        create_seeded_thread(&addr, &runtime_threads, &root, "Please fork this turn").await?;
    let client = crate::tls::reqwest_client();

    let resp = client
        .post(format!("http://{addr}/v1/threads/{thread_id}/fork-at-turn"))
        .json(&json!({ "turn_id": "turn_not_a_user_turn" }))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn patch_undo_endpoint_refuses_turns_it_cannot_prove() -> Result<()> {
    let root =
        std::env::temp_dir().join(format!("deepseek-patch-undo-endpoint-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    let Some((addr, runtime_threads, handle)) =
        spawn_test_server_with_root(root.clone(), sessions_dir).await?
    else {
        return Ok(());
    };
    let thread_id =
        create_seeded_thread(&addr, &runtime_threads, &root, "Roll back the patch").await?;
    let client = crate::tls::reqwest_client();

    // A workspace directory that is not available proves nothing about the
    // files a turn changed: the undo is refused instead of forking the
    // conversation away from them.
    let resp = client
        .post(format!("http://{addr}/v1/threads/{thread_id}/patch-undo"))
        .json(&json!({}))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    assert!(resp.text().await?.contains("not available"));

    // A seeded (imported) turn records nothing about what it did to the
    // workspace, so a file undo cannot be proven either way: refused with a
    // code that points the client at the conversation-only undo, and no
    // fork is published.
    fs::create_dir_all(root.join("workspace"))?;
    let resp = client
        .post(format!("http://{addr}/v1/threads/{thread_id}/patch-undo"))
        .json(&json!({}))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let refused: serde_json::Value = resp.json().await?;
    assert_eq!(refused["error"]["code"], "restore_point_unavailable");
    let threads: serde_json::Value = client
        .get(format!("http://{addr}/v1/threads"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(threads.as_array().map(Vec::len), Some(1), "{threads:#}");
    let resp = client
        .post(format!("http://{addr}/v1/threads/{thread_id}/undo"))
        .json(&json!({}))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let undone: serde_json::Value = resp.json().await?;
    assert_eq!(undone["original_user_text"], "Roll back the patch");
    assert_ne!(undone["thread"]["id"].as_str(), Some(thread_id.as_str()));

    handle.abort();
    Ok(())
}

fn snapshot_receipt(
    kind: crate::snapshot::WorkspaceSnapshotKind,
    taken: &crate::snapshot::TakenSnapshot,
    session_id: &str,
) -> crate::snapshot::WorkspaceSnapshotRef {
    crate::snapshot::WorkspaceSnapshotRef::new(kind, taken, session_id, None)
}

/// The receipts of a shell turn: its command runs from the pre-turn snapshot
/// to the post-turn one, so the whole window is the turn's own span.
fn shell_turn_receipts(
    repo: &crate::snapshot::SnapshotRepo,
    pre: &crate::snapshot::TakenSnapshot,
    post: &crate::snapshot::TakenSnapshot,
    session_id: &str,
) -> Vec<crate::snapshot::WorkspaceSnapshotRef> {
    use crate::snapshot::WorkspaceSnapshotKind::{PostTurn, PreTurn};
    let mut pre_receipt = snapshot_receipt(PreTurn, pre, session_id);
    pre_receipt.tool_call_id = Some("user_shell_1".to_string());
    let mut post_receipt = snapshot_receipt(PostTurn, post, session_id);
    post_receipt.changed_paths = Some(changed_between(repo, pre, post));
    vec![pre_receipt, post_receipt]
}

fn changed_between(
    repo: &crate::snapshot::SnapshotRepo,
    from: &crate::snapshot::TakenSnapshot,
    to: &crate::snapshot::TakenSnapshot,
) -> Vec<String> {
    repo.changed_paths_between(&from.tree, &to.tree)
        .expect("diff snapshots")
        .into_iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect()
}

fn dropped_turn(
    turn_id: &str,
    may_change_files: bool,
    snapshots: Vec<crate::snapshot::WorkspaceSnapshotRef>,
) -> crate::runtime_threads::DroppedTurnSnapshots {
    crate::runtime_threads::DroppedTurnSnapshots {
        turn_id: turn_id.to_string(),
        may_change_files,
        snapshots,
        declared_writes: Vec::new(),
        unrun_tool_calls: std::collections::BTreeSet::new(),
    }
}

fn git_missing() -> bool {
    crate::dependencies::Git::command().is_none()
}

/// A patch-undo restores exactly what the dropped turn changed — every write
/// of the turn, not only its last — and nothing else: later work on other
/// files and other sessions' snapshots in the same workspace are untouched.
#[test]
fn patch_undo_helper_restores_only_the_dropped_turns_changes() -> Result<()> {
    if git_missing() {
        return Ok(());
    }
    let _lock = lock_test_env();
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    fs::create_dir_all(&home)?;
    let _home = EnvVarGuard::set("HOME", &home);

    let workspace = root.path().join("workspace");
    fs::create_dir_all(&workspace)?;
    let repo = crate::snapshot::SnapshotRepo::open_or_init(&workspace)?;
    let a = workspace.join("a.txt");
    let other = workspace.join("other.txt");
    fs::write(&a, "before")?;
    fs::write(&other, "other-before")?;
    let pre = repo.take_snapshot("pre-turn:1", Some("thr_a"))?;
    fs::write(&a, "first write")?;
    repo.take_snapshot("tool:call-1", Some("thr_a"))?;
    fs::write(&a, "second write")?;
    fs::create_dir_all(workspace.join("made"))?;
    fs::write(workspace.join("made/new.txt"), "created")?;
    let post = repo.take_snapshot("post-turn:1", Some("thr_a"))?;
    // After the turn: another writer changes an unrelated file, and another
    // session snapshots the workspace.
    fs::write(&other, "later work")?;
    repo.take_snapshot("pre-turn:9", Some("thr_other"))?;

    let turn = dropped_turn(
        "turn_1",
        true,
        shell_turn_receipts(&repo, &pre, &post, "thr_a"),
    );
    let restored = patch_undo_workspace_files(&workspace, std::slice::from_ref(&turn), true)
        .expect("a trusted rollback should succeed");
    assert!(restored.files_restored, "{:?}", restored.summary);
    assert_eq!(fs::read_to_string(&a)?, "before", "both writes are undone");
    assert!(!workspace.join("made/new.txt").exists());
    assert!(
        !workspace.join("made").exists(),
        "the directory the turn created goes with it"
    );
    assert_eq!(fs::read_to_string(&other)?, "later work");
    assert!(
        restored
            .snapshot_label
            .as_deref()
            .is_some_and(|label| label.starts_with("pre-turn:1")),
        "{:?}",
        restored.snapshot_label
    );

    // Undoing again finds the files already at their pre-turn state.
    let again = patch_undo_workspace_files(&workspace, std::slice::from_ref(&turn), true)
        .expect("already restored is provably nothing to do");
    assert!(!again.files_restored);
    Ok(())
}

/// A turn's changed file that someone edited after the turn is never
/// clobbered: the undo is refused with a typed code and nothing changes.
#[test]
fn patch_undo_helper_refuses_to_overwrite_later_edits() -> Result<()> {
    if git_missing() {
        return Ok(());
    }
    let _lock = lock_test_env();
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    fs::create_dir_all(&home)?;
    let _home = EnvVarGuard::set("HOME", &home);

    let workspace = root.path().join("workspace");
    fs::create_dir_all(&workspace)?;
    let repo = crate::snapshot::SnapshotRepo::open_or_init(&workspace)?;
    let a = workspace.join("a.txt");
    fs::write(&a, "before")?;
    let pre = repo.take_snapshot("pre-turn:1", Some("thr_a"))?;
    fs::write(&a, "turn")?;
    let post = repo.take_snapshot("post-turn:1", Some("thr_a"))?;
    fs::write(&a, "user edit after the turn")?;

    let turn = dropped_turn(
        "turn_1",
        true,
        shell_turn_receipts(&repo, &pre, &post, "thr_a"),
    );
    let err = patch_undo_workspace_files(&workspace, &[turn], true)
        .expect_err("a later edit must not be overwritten");
    assert_eq!(err.status, StatusCode::CONFLICT);
    assert_eq!(err.code, Some(PATCH_UNDO_WORKSPACE_CHANGED));
    assert!(err.message.contains("a.txt"), "{}", err.message);
    assert_eq!(fs::read_to_string(&a)?, "user edit after the turn");
    Ok(())
}

/// No recorded restore point, an unclosed window, or a pruned / retagged
/// restore point: the undo fails honestly with a typed 409 instead of
/// forking over changed files. A turn that ran no tools needs none.
#[test]
fn patch_undo_helper_fails_honestly_without_an_owned_restore_point() -> Result<()> {
    use crate::snapshot::WorkspaceSnapshotKind::PreTurn;
    if git_missing() {
        return Ok(());
    }
    let _lock = lock_test_env();
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    fs::create_dir_all(&home)?;
    let _home = EnvVarGuard::set("HOME", &home);

    let workspace = root.path().join("workspace");
    fs::create_dir_all(&workspace)?;
    let repo = crate::snapshot::SnapshotRepo::open_or_init(&workspace)?;
    let a = workspace.join("a.txt");
    fs::write(&a, "before")?;
    let pre = repo.take_snapshot("pre-turn:1", Some("thr_a"))?;
    fs::write(&a, "after")?;
    let post = repo.take_snapshot("post-turn:1", Some("thr_a"))?;

    let cases = [
        (
            dropped_turn("legacy", true, Vec::new()),
            PATCH_UNDO_NO_RESTORE_POINT,
        ),
        (
            dropped_turn(
                "unclosed",
                true,
                vec![snapshot_receipt(PreTurn, &pre, "thr_a")],
            ),
            PATCH_UNDO_NO_RESTORE_POINT,
        ),
        (
            dropped_turn(
                "retagged",
                true,
                shell_turn_receipts(&repo, &pre, &post, "thr_someone_else"),
            ),
            PATCH_UNDO_RESTORE_POINT_PRUNED,
        ),
    ];
    for (turn, code) in cases {
        let err = patch_undo_workspace_files(&workspace, std::slice::from_ref(&turn), true)
            .expect_err(&turn.turn_id);
        assert_eq!(err.status, StatusCode::CONFLICT, "{}", turn.turn_id);
        assert_eq!(err.code, Some(code), "{}: {}", turn.turn_id, err.message);
        assert!(err.message.contains("/undo"), "{}", err.message);
        assert_eq!(fs::read_to_string(&a)?, "after");
    }

    let chat_only = patch_undo_workspace_files(
        &workspace,
        &[dropped_turn("chat", false, Vec::new())],
        false,
    )
    .expect("a turn that ran no tools changed no files");
    assert!(!chat_only.files_restored);

    // A prune that drops the restore point's tree fails closed too.
    let receipts = shell_turn_receipts(&repo, &pre, &post, "thr_a");
    repo.prune_older_than(Duration::ZERO)?;
    let err =
        patch_undo_workspace_files(&workspace, &[dropped_turn("pruned", true, receipts)], true)
            .expect_err("a pruned restore point cannot be restored");
    assert_eq!(err.code, Some(PATCH_UNDO_RESTORE_POINT_PRUNED));
    assert_eq!(fs::read_to_string(&a)?, "after");
    Ok(())
}

/// A prune rebuilds the side repo's commit chain, rewriting every commit id
/// while keeping trees; a recorded restore point still resolves.
#[test]
fn patch_undo_helper_survives_a_prune_that_rewrites_commit_ids() -> Result<()> {
    if git_missing() {
        return Ok(());
    }
    let _lock = lock_test_env();
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    fs::create_dir_all(&home)?;
    let _home = EnvVarGuard::set("HOME", &home);

    let workspace = root.path().join("workspace");
    fs::create_dir_all(&workspace)?;
    let repo = crate::snapshot::SnapshotRepo::open_or_init(&workspace)?;
    let a = workspace.join("a.txt");
    fs::write(&a, "ancient")?;
    repo.take_snapshot("pre-turn:0", Some("thr_old"))?;
    fs::write(&a, "before")?;
    let pre = repo.take_snapshot("pre-turn:1", Some("thr_a"))?;
    fs::write(&a, "after")?;
    let post = repo.take_snapshot("post-turn:1", Some("thr_a"))?;
    assert_eq!(repo.prune_keep_last_n(2)?, 1);
    assert!(
        repo.list(usize::MAX)?
            .iter()
            .all(|snapshot| snapshot.id != pre.id && snapshot.id != post.id),
        "the prune rewrote the survivors' commit ids"
    );

    let restored = patch_undo_workspace_files(
        &workspace,
        &[dropped_turn(
            "turn_1",
            true,
            shell_turn_receipts(&repo, &pre, &post, "thr_a"),
        )],
        true,
    )
    .expect("the tree still resolves the restore point");
    assert!(restored.files_restored);
    assert_eq!(fs::read_to_string(&a)?, "before");
    Ok(())
}

/// One recorded receipt with the span data a recording engine reports.
fn span_receipt(
    kind: crate::snapshot::WorkspaceSnapshotKind,
    taken: &crate::snapshot::TakenSnapshot,
    call: Option<&str>,
    write_paths: Option<&[&str]>,
    changed: Option<Vec<String>>,
) -> crate::snapshot::WorkspaceSnapshotRef {
    let mut receipt = snapshot_receipt(kind, taken, "thr_a");
    receipt.tool_call_id = call.map(str::to_string);
    receipt.write_paths = write_paths.map(|paths| paths.iter().map(|p| p.to_string()).collect());
    receipt.changed_paths = changed;
    receipt
}

/// Attribution from the recorded spans: a path the turn's own declared
/// write changed is restored; a path someone else changed while the turn
/// ran — in a span where no tool of the turn was running, or inside a file
/// tool's span but not a path it declared — refuses the undo instead of
/// reverting their work. An undeclared (shell) span owns what it changed,
/// and a span with no record of its changes fails closed.
#[test]
fn patch_undo_helper_attributes_changes_to_the_turns_own_tool_spans() -> Result<()> {
    use crate::snapshot::WorkspaceSnapshotKind::{PostTool, PostTurn, PreTurn, Tool};
    if git_missing() {
        return Ok(());
    }
    let _lock = lock_test_env();
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    fs::create_dir_all(&home)?;
    let _home = EnvVarGuard::set("HOME", &home);

    let workspace = root.path().join("workspace");
    fs::create_dir_all(&workspace)?;
    let repo = crate::snapshot::SnapshotRepo::open_or_init(&workspace)?;
    let own = workspace.join("own.txt");
    let sibling = workspace.join("sibling.txt");
    fs::write(&own, "before")?;
    fs::write(&sibling, "sibling-before")?;

    // pre → tool(write own.txt) → post_tool → [another thread writes
    // sibling.txt while the model thinks] → post_turn
    let pre = repo.take_snapshot("pre-turn:1", Some("thr_a"))?;
    let tool = repo.take_snapshot("tool:call-1", Some("thr_a"))?;
    fs::write(&own, "turn")?;
    let post_tool = repo.take_snapshot("post-tool:call-1", Some("thr_a"))?;
    fs::write(&sibling, "sibling-during-turn")?;
    let post = repo.take_snapshot("post-turn:1", Some("thr_a"))?;
    let receipts = |gap_writer: bool| {
        let mut receipts = vec![
            span_receipt(PreTurn, &pre, None, None, None),
            span_receipt(
                Tool,
                &tool,
                Some("call-1"),
                Some(&["./own.txt"]),
                Some(changed_between(&repo, &pre, &tool)),
            ),
            span_receipt(
                PostTool,
                &post_tool,
                Some("call-1"),
                None,
                Some(changed_between(&repo, &tool, &post_tool)),
            ),
            span_receipt(
                PostTurn,
                &post,
                None,
                None,
                Some(changed_between(&repo, &post_tool, &post)),
            ),
        ];
        if !gap_writer {
            // The same history with the sibling's write inside the file
            // tool's span instead: still not a path the call declared.
            receipts[2].changed_paths = Some(changed_between(&repo, &tool, &post));
            receipts[3].changed_paths = Some(Vec::new());
        }
        receipts
    };
    for gap_writer in [true, false] {
        let err = patch_undo_workspace_files(
            &workspace,
            &[dropped_turn("turn_1", true, receipts(gap_writer))],
            true,
        )
        .expect_err("another writer's change must not be reverted");
        assert_eq!(err.status, StatusCode::CONFLICT);
        assert_eq!(
            err.code,
            Some(PATCH_UNDO_WORKSPACE_CHANGED),
            "{}",
            err.message
        );
        assert!(err.message.contains("sibling.txt"), "{}", err.message);
        assert!(!err.message.contains("own.txt"), "{}", err.message);
        assert_eq!(fs::read_to_string(&own)?, "turn");
        assert_eq!(fs::read_to_string(&sibling)?, "sibling-during-turn");
    }

    // Without the sibling's write the turn's own change restores.
    fs::write(&sibling, "sibling-before")?;
    let clean_post = repo.take_snapshot("post-turn:2", Some("thr_a"))?;
    let mut clean = receipts(true);
    clean[3] = span_receipt(
        PostTurn,
        &clean_post,
        None,
        None,
        Some(changed_between(&repo, &post_tool, &clean_post)),
    );
    let restored = patch_undo_workspace_files(
        &workspace,
        &[dropped_turn("turn_1", true, clean.clone())],
        true,
    )
    .expect("the turn's own write restores");
    assert!(restored.files_restored, "{:?}", restored.summary);
    assert_eq!(fs::read_to_string(&own)?, "before");

    // An undeclared tool (a shell command) owns whatever changed in its span.
    fs::write(&own, "turn")?;
    let mut shell = clean.clone();
    shell[1].write_paths = None;
    let restored =
        patch_undo_workspace_files(&workspace, &[dropped_turn("turn_1", true, shell)], true)
            .expect("an undeclared span owns its changes");
    assert!(restored.files_restored);
    assert_eq!(fs::read_to_string(&own)?, "before");

    // A span whose changes were never recorded cannot be attributed.
    fs::write(&own, "turn")?;
    let mut unknown = clean;
    unknown[2].changed_paths = None;
    let err =
        patch_undo_workspace_files(&workspace, &[dropped_turn("turn_1", true, unknown)], true)
            .expect_err("an unaccounted span fails closed");
    assert_eq!(
        err.code,
        Some(PATCH_UNDO_NO_RESTORE_POINT),
        "{}",
        err.message
    );
    assert_eq!(fs::read_to_string(&own)?, "turn");
    Ok(())
}

/// A file tool that wrote a path the snapshots never hold (ignored, or
/// outside the workspace) cannot be undone: refused with its own code even
/// when the snapshots show nothing changed.
#[test]
fn patch_undo_helper_refuses_declared_writes_snapshots_cannot_hold() -> Result<()> {
    use crate::snapshot::WorkspaceSnapshotKind::{PostTool, PostTurn, PreTurn, Tool};
    if git_missing() {
        return Ok(());
    }
    let _lock = lock_test_env();
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    fs::create_dir_all(&home)?;
    let _home = EnvVarGuard::set("HOME", &home);

    let workspace = root.path().join("workspace");
    fs::create_dir_all(&workspace)?;
    fs::write(workspace.join(".gitignore"), "*.local\n")?;
    let repo = crate::snapshot::SnapshotRepo::open_or_init(&workspace)?;
    let pre = repo.take_snapshot("pre-turn:1", Some("thr_a"))?;
    let tool = repo.take_snapshot("tool:call-1", Some("thr_a"))?;
    fs::write(workspace.join(".env.local"), "SECRET=changed")?;
    let post_tool = repo.take_snapshot("post-tool:call-1", Some("thr_a"))?;
    let post = repo.take_snapshot("post-turn:1", Some("thr_a"))?;
    let receipts = |declared: &[&str]| {
        vec![
            span_receipt(PreTurn, &pre, None, None, None),
            span_receipt(
                Tool,
                &tool,
                Some("call-1"),
                Some(declared),
                Some(Vec::new()),
            ),
            span_receipt(
                PostTool,
                &post_tool,
                Some("call-1"),
                None,
                Some(changed_between(&repo, &tool, &post_tool)),
            ),
            span_receipt(PostTurn, &post, None, None, Some(Vec::new())),
        ]
    };
    let outside = root.path().join("outside.txt");
    let outside = outside.to_string_lossy().into_owned();
    for declared in [".env.local", "node_modules/pkg/index.js", outside.as_str()] {
        let err = patch_undo_workspace_files(
            &workspace,
            &[dropped_turn("turn_1", true, receipts(&[declared]))],
            true,
        )
        .expect_err(declared);
        assert_eq!(err.status, StatusCode::CONFLICT, "{declared}");
        assert_eq!(
            err.code,
            Some(PATCH_UNDO_PATH_NOT_SNAPSHOTTED),
            "{declared}: {}",
            err.message
        );
        assert!(err.message.contains(declared), "{}", err.message);
    }
    // The same declared write from a call recorded as failed wrote nothing.
    let mut failed = dropped_turn("turn_1", true, receipts(&[".env.local"]));
    failed.unrun_tool_calls.insert("call-1".to_string());
    let result = patch_undo_workspace_files(&workspace, &[failed], true)
        .expect("a failed call wrote nothing");
    assert!(!result.files_restored);
    // Declared through the turn's items rather than a receipt.
    let mut from_items = dropped_turn("turn_1", true, receipts(&[]));
    from_items.declared_writes.push(".env.local".to_string());
    let err = patch_undo_workspace_files(&workspace, &[from_items], true)
        .expect_err("an item-declared ignored write refuses too");
    assert_eq!(err.code, Some(PATCH_UNDO_PATH_NOT_SNAPSHOTTED));
    Ok(())
}

/// The gate the TUI's `/undo` applies, stated as a contract: an untrusted
/// thread may not roll files back. When there *is* something to roll back the
/// whole undo aborts — nothing is changed, and the caller must not fork the
/// conversation either.
#[test]
fn patch_undo_helper_refuses_an_untrusted_rollback_and_touches_nothing() -> Result<()> {
    if git_missing() {
        return Ok(());
    }
    let _lock = lock_test_env();
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    fs::create_dir_all(&home)?;
    let _home = EnvVarGuard::set("HOME", &home);

    let workspace = root.path().join("workspace");
    fs::create_dir_all(&workspace)?;
    let repo = crate::snapshot::SnapshotRepo::open_or_init(&workspace)?;
    let file = workspace.join("a.txt");

    fs::write(&file, "before")?;
    let pre = repo.take_snapshot("pre-turn:1", Some("thr_a"))?;
    fs::write(&file, "after")?;
    let post = repo.take_snapshot("post-turn:1", Some("thr_a"))?;
    let turn = dropped_turn(
        "turn_1",
        true,
        shell_turn_receipts(&repo, &pre, &post, "thr_a"),
    );

    let err = patch_undo_workspace_files(&workspace, std::slice::from_ref(&turn), false)
        .expect_err("an untrusted rollback must be refused");
    assert_eq!(err.status, StatusCode::CONFLICT);
    assert_eq!(err.code, Some(PATCH_UNDO_UNTRUSTED));
    assert!(
        err.message.contains("outside trusted mode"),
        "got: {}",
        err.message
    );
    assert_eq!(
        fs::read_to_string(&file)?,
        "after",
        "a refusal must leave the workspace exactly as it was"
    );

    // The identical call once trusted performs the rollback.
    let restored = patch_undo_workspace_files(&workspace, &[turn], true)
        .expect("a trusted rollback should succeed");
    assert!(restored.files_restored, "{:?}", restored.summary);
    assert_eq!(fs::read_to_string(&file)?, "before");
    Ok(())
}

/// ...but with provably nothing to roll back, an untrusted undo is still
/// allowed: the conversation half needs no trust, and skipping it would make
/// Undo useless in Ask / Auto-Review for no safety gain.
#[test]
fn patch_undo_helper_allows_an_untrusted_undo_with_nothing_to_roll_back() -> Result<()> {
    if git_missing() {
        return Ok(());
    }
    let _lock = lock_test_env();
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    fs::create_dir_all(&home)?;
    let _home = EnvVarGuard::set("HOME", &home);

    let workspace = root.path().join("workspace");
    fs::create_dir_all(&workspace)?;
    let repo = crate::snapshot::SnapshotRepo::open_or_init(&workspace)?;
    let file = workspace.join("a.txt");

    fs::write(&file, "only-state")?;
    let pre = repo.take_snapshot("pre-turn:1", Some("thr_a"))?;
    // The turn ran a tool but changed nothing.
    let post = repo.take_snapshot("post-turn:1", Some("thr_a"))?;

    let result = patch_undo_workspace_files(
        &workspace,
        &[dropped_turn(
            "turn_1",
            true,
            shell_turn_receipts(&repo, &pre, &post, "thr_a"),
        )],
        false,
    )
    .expect("nothing to roll back is not a refusal");
    assert!(!result.files_restored);
    assert!(result.summary.is_some());
    assert_eq!(fs::read_to_string(&file)?, "only-state");
    Ok(())
}

fn sha256_of(path: &FsPath) -> String {
    format!(
        "sha256:{}",
        crate::hashing::sha256_hex(fs::read(path).unwrap())
    )
}

fn revert_request(path: &str, snapshot_id: &str, expected_hash: &str) -> RevertThreadFileRequest {
    RevertThreadFileRequest {
        path: path.to_string(),
        snapshot_id: snapshot_id.to_string(),
        expected_hash: expected_hash.to_string(),
    }
}

/// The acceptance criterion for per-file revert: restoring one file must not
/// roll back any other file's turn changes.
#[test]
fn revert_file_helper_restores_only_the_named_file() -> Result<()> {
    let _lock = lock_test_env();
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    fs::create_dir_all(&home)?;
    let _home = EnvVarGuard::set("HOME", &home);

    let workspace = root.path().join("workspace");
    fs::create_dir_all(&workspace)?;
    let repo = crate::snapshot::SnapshotRepo::open_or_init(&workspace)?;
    let wanted = workspace.join("wanted.txt");
    let other = workspace.join("other.txt");

    fs::write(&wanted, "wanted-before")?;
    fs::write(&other, "other-before")?;
    let taken = repo.take_snapshot("pre-turn:1", Some("session-a"))?;
    let owned = [snapshot_receipt(
        crate::snapshot::WorkspaceSnapshotKind::PreTurn,
        &taken,
        "session-a",
    )];

    fs::write(&wanted, "wanted-after")?;
    fs::write(&other, "other-after")?;

    let snapshot = repo.list(10)?.remove(0);
    let reverted = revert_file_from_snapshot(
        &workspace,
        &owned,
        &revert_request("wanted.txt", snapshot.id.as_str(), &sha256_of(&wanted)),
    )
    .expect("scoped revert should succeed");
    assert_eq!(reverted.path, "wanted.txt");
    assert_eq!(reverted.action, "modified");
    assert_eq!(reverted.snapshot_label, "pre-turn:1");
    assert_eq!(reverted.snapshot_id, snapshot.id.as_str());
    assert_eq!(fs::read_to_string(&wanted)?, "wanted-before");
    assert_eq!(
        fs::read_to_string(&other)?,
        "other-after",
        "reverting one file must leave every other file's changes in place"
    );
    Ok(())
}

#[test]
fn revert_file_helper_accepts_an_absolute_path_inside_the_workspace() -> Result<()> {
    let _lock = lock_test_env();
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    fs::create_dir_all(&home)?;
    let _home = EnvVarGuard::set("HOME", &home);

    let workspace = root.path().join("workspace");
    fs::create_dir_all(&workspace)?;
    let repo = crate::snapshot::SnapshotRepo::open_or_init(&workspace)?;
    let file = workspace.join("a.txt");

    fs::write(&file, "v1")?;
    let taken = repo.take_snapshot("pre-turn:1", Some("session-a"))?;
    let owned = [snapshot_receipt(
        crate::snapshot::WorkspaceSnapshotKind::PreTurn,
        &taken,
        "session-a",
    )];
    fs::write(&file, "v2")?;

    // A recorded path may arrive absolute; normalization reports it back
    // workspace-relative so the GUI's change record stays consistent. The
    // restore point is named by the tree id its receipt recorded.
    let reverted = revert_file_from_snapshot(
        &workspace,
        &owned,
        &revert_request(
            &file.to_string_lossy(),
            taken.tree.as_str(),
            &sha256_of(&file),
        ),
    )
    .expect("scoped revert should accept an in-workspace absolute path");
    assert_eq!(reverted.path, "a.txt");
    assert_eq!(fs::read_to_string(&file)?, "v1");
    Ok(())
}

/// The client names the exact snapshot from its change record. A newer
/// unrelated snapshot must not be selected for it, and a stale hash means
/// the user edited the file after the record was captured: refuse and leave
/// the workspace untouched.
#[test]
fn revert_file_helper_requires_exact_snapshot_identity_and_reviewed_hash() -> Result<()> {
    use crate::snapshot::WorkspaceSnapshotKind::{PostTurn, Tool};
    let _lock = lock_test_env();
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    fs::create_dir_all(&home)?;
    let _home = EnvVarGuard::set("HOME", &home);

    let workspace = root.path().join("workspace");
    fs::create_dir_all(&workspace)?;
    let repo = crate::snapshot::SnapshotRepo::open_or_init(&workspace)?;
    let a = workspace.join("a.txt");
    let b = workspace.join("b.txt");

    // Tool A edits a.txt; tool B snapshots before editing only b.txt; the user
    // then edits a.txt by hand.
    fs::write(&a, "a-before")?;
    fs::write(&b, "b-before")?;
    let tool_a = repo.take_snapshot("tool:call-a", Some("session-a"))?;
    fs::write(&a, "a-after-tool")?;
    let tool_b = repo.take_snapshot("tool:call-b", Some("session-a"))?;
    fs::write(&b, "b-after-tool")?;
    fs::write(&a, "a-user-edit")?;
    let post = repo.take_snapshot("post-turn:1", Some("session-a"))?;
    let mut owned = vec![
        snapshot_receipt(Tool, &tool_a, "session-a"),
        snapshot_receipt(Tool, &tool_b, "session-a"),
        snapshot_receipt(PostTurn, &post, "session-a"),
    ];

    // Restoring a.txt from tool B's snapshot would erase only the user edit;
    // the client must ask for tool A's snapshot and gets back a-before.
    let err = revert_file_from_snapshot(
        &workspace,
        &owned,
        &revert_request("a.txt", tool_a.id.as_str(), &sha256_of(&b)),
    )
    .expect_err("a hash from different bytes must refuse");
    assert_eq!(err.status, StatusCode::CONFLICT);
    assert!(
        err.message.contains("changed after"),
        "got: {}",
        err.message
    );
    assert_eq!(fs::read_to_string(&a)?, "a-user-edit");

    let reverted = revert_file_from_snapshot(
        &workspace,
        &owned,
        &revert_request("a.txt", tool_a.id.as_str(), &sha256_of(&a)),
    )
    .expect("exact identity restores");
    assert_eq!(reverted.snapshot_label, "tool:call-a");
    assert_eq!(fs::read_to_string(&a)?, "a-before");
    assert_eq!(
        fs::read_to_string(&b)?,
        "b-after-tool",
        "b.txt is untouched"
    );

    // Already matching: 409, nothing changed.
    let err = revert_file_from_snapshot(
        &workspace,
        &owned,
        &revert_request("a.txt", tool_a.id.as_str(), &sha256_of(&a)),
    )
    .expect_err("nothing to revert");
    assert_eq!(err.status, StatusCode::CONFLICT);
    assert!(
        err.message.contains("nothing to revert"),
        "got: {}",
        err.message
    );

    // A snapshot owned by another thread (even when the receipt list is
    // tampered to name it under this thread's tag), a post-turn state, or an
    // unknown id are all refused with a refresh hint.
    let foreign = repo.take_snapshot("tool:foreign", Some("session-b"))?;
    owned.push(snapshot_receipt(Tool, &foreign, "session-a"));
    for id in [
        foreign.id.as_str(),
        post.id.as_str(),
        "0123456789abcdef0123456789abcdef01234567",
    ] {
        let err = revert_file_from_snapshot(
            &workspace,
            &owned,
            &revert_request("b.txt", id, &sha256_of(&b)),
        )
        .expect_err(id);
        assert_eq!(err.status, StatusCode::CONFLICT, "{id}");
        assert!(
            err.message.contains("refresh the change record"),
            "{id}: {}",
            err.message
        );
    }
    assert_eq!(fs::read_to_string(&b)?, "b-after-tool");

    // Directories and Git metadata are 400s before anything is compared.
    for path in ["", "src", ".git/config", "../escape.txt"] {
        fs::create_dir_all(workspace.join("src"))?;
        let err = revert_file_from_snapshot(
            &workspace,
            &owned,
            &revert_request(path, tool_a.id.as_str(), "absent"),
        )
        .expect_err(path);
        assert_eq!(err.status, StatusCode::BAD_REQUEST, "{path}");
    }
    Ok(())
}

#[test]
fn revert_file_helper_refuses_foreign_snapshots_and_paths_outside_the_workspace() -> Result<()> {
    let _lock = lock_test_env();
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    fs::create_dir_all(&home)?;
    let _home = EnvVarGuard::set("HOME", &home);

    let workspace = root.path().join("workspace");
    fs::create_dir_all(&workspace)?;
    let repo = crate::snapshot::SnapshotRepo::open_or_init(&workspace)?;
    let file = workspace.join("a.txt");

    // Only another thread recorded a restore point, so this thread owns
    // nothing it is allowed to revert.
    fs::write(&file, "foreign-before")?;
    repo.take_snapshot("pre-turn:1", Some("session-b"))?;
    fs::write(&file, "foreign-after")?;

    let foreign = repo.list(10)?.remove(0);
    let err = revert_file_from_snapshot(
        &workspace,
        &[],
        &revert_request("a.txt", foreign.id.as_str(), &sha256_of(&file)),
    )
    .expect_err("a foreign thread's snapshot must not be restorable");
    assert_eq!(err.status, StatusCode::CONFLICT);
    assert!(
        err.message.contains("another thread"),
        "got: {}",
        err.message
    );
    assert_eq!(fs::read_to_string(&file)?, "foreign-after");

    let traversal = revert_file_from_snapshot(
        &workspace,
        &[],
        &revert_request("../escape.txt", foreign.id.as_str(), "absent"),
    )
    .expect_err("traversal must be refused");
    assert_eq!(traversal.status, StatusCode::BAD_REQUEST);
    Ok(())
}

/// The GUI decides whether to show the per-file Revert button from a `GET`
/// probe of this route: 405 ("route exists, wrong method") means available,
/// 404 means an older engine that must degrade with an explanation. Assert the
/// wire contract, not just the Rust helper.
#[tokio::test]
async fn file_revert_route_probes_as_available_and_404s_unknown_threads() -> Result<()> {
    let root = std::env::temp_dir().join(format!("deepseek-file-revert-route-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root(root.clone(), sessions_dir).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let probe = client
        .get(format!("http://{addr}/v1/threads/__probe__/file-revert"))
        .send()
        .await?;
    assert_eq!(
        probe.status(),
        StatusCode::METHOD_NOT_ALLOWED,
        "the capability probe reads any non-404 as 'endpoint available'"
    );

    let body = json!({
        "path": "a.txt",
        "snapshot_id": "0123456789abcdef0123456789abcdef01234567",
        "expected_hash": "absent"
    });
    let missing = client
        .post(format!("http://{addr}/v1/threads/thr_missing/file-revert"))
        .json(&body)
        .send()
        .await?;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    handle.abort();
    Ok(())
}

/// Route-level contract for the new destructive endpoint: malformed bodies
/// are 422/400 before any thread state is read, an untrusted thread is a 409
/// even with a well-formed request, and a trusted thread without a bound
/// restore point it owns is a 409 that names the reason. No file is touched in
/// any case.
#[tokio::test]
async fn file_revert_route_validates_body_then_trust_then_ownership() -> Result<()> {
    // The route opens the snapshot store under the resolved home. Seal the
    // process environment and own that home, so a concurrent test's temporary
    // HOME (deleted when it ends) can never host or remove this store mid-request.
    let _env = lock_test_env();
    let root = std::env::temp_dir().join(format!("deepseek-file-revert-gates-{}", Uuid::new_v4()));
    let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
    let sessions_dir = root.join("sessions");
    let Some((addr, runtime_threads, handle)) =
        spawn_test_server_with_root(root.clone(), sessions_dir).await?
    else {
        return Ok(());
    };
    let thread_id = create_seeded_thread(&addr, &runtime_threads, &root, "Edit a.txt").await?;
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace)?;
    let file = workspace.join("a.txt");
    fs::write(&file, "keep")?;
    let client = crate::tls::reqwest_client();
    let url = format!("http://{addr}/v1/threads/{thread_id}/file-revert");
    let well_formed = json!({
        "path": "a.txt",
        "snapshot_id": "0123456789abcdef0123456789abcdef01234567",
        "expected_hash": format!("sha256:{}", "a".repeat(64))
    });

    // Missing required fields: rejected by the JSON extractor.
    let resp = client
        .post(&url)
        .json(&json!({ "path": "a.txt" }))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    // Malformed identity/hash: 400 with the expected format.
    for bad in [
        json!({ "path": "a.txt", "snapshot_id": "not-hex", "expected_hash": "absent" }),
        json!({ "path": "a.txt", "snapshot_id": "0123456789abcdef0123456789abcdef01234567", "expected_hash": "md5:abc" }),
        json!({ "path": "a.txt", "snapshot_id": "0123456789abcdef0123456789abcdef01234567", "expected_hash": format!("sha256:{}", "A".repeat(64)) }),
    ] {
        let resp = client.post(&url).json(&bad).send().await?;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{bad}");
    }
    // Untrusted thread: 409 that says how to proceed.
    let resp = client.post(&url).json(&well_formed).send().await?;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let text = resp.text().await?;
    assert!(text.contains("trusted mode"), "got: {text}");

    // Trusted, but the thread recorded no such restore point: still a 409
    // with the reason.
    client
        .patch(format!("http://{addr}/v1/threads/{thread_id}"))
        .json(&json!({ "trust_mode": true }))
        .send()
        .await?
        .error_for_status()?;
    let resp = client.post(&url).json(&well_formed).send().await?;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let text = resp.text().await?;
    assert!(text.contains("refresh the change record"), "got: {text}");
    assert_eq!(fs::read_to_string(&file)?, "keep");

    handle.abort();
    Ok(())
}

/// All three restore routes refuse while a turn is active in an overlapping
/// workspace, and admit again once it settles. Nothing is changed on refusal.
#[tokio::test]
async fn restore_routes_refuse_an_active_turn_in_the_workspace() -> Result<()> {
    // The route opens the snapshot store under the resolved home. Seal the
    // process environment and own that home, so a concurrent test's temporary
    // HOME (deleted when it ends) can never host or remove this store mid-request.
    let _env = lock_test_env();
    let root = std::env::temp_dir().join(format!("deepseek-restore-active-{}", Uuid::new_v4()));
    let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
    let sessions_dir = root.join("sessions");
    let Some((addr, runtime_threads, handle)) =
        spawn_test_server_with_root(root.clone(), sessions_dir).await?
    else {
        return Ok(());
    };
    let thread_id = create_seeded_thread(&addr, &runtime_threads, &root, "Keep working").await?;
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace)?;
    let file = workspace.join("a.txt");
    fs::write(&file, "live")?;
    let client = crate::tls::reqwest_client();
    client
        .patch(format!("http://{addr}/v1/threads/{thread_id}"))
        .json(&json!({ "trust_mode": true }))
        .send()
        .await?
        .error_for_status()?;
    let harness = crate::core::engine::mock_engine_handle();
    runtime_threads
        .install_test_engine(&thread_id, harness.handle.clone())
        .await?;
    runtime_threads
        .set_active_turn_for_test(&thread_id, Some("turn_live"))
        .await?;

    let revert_body = json!({
        "path": "a.txt",
        "snapshot_id": "0123456789abcdef0123456789abcdef01234567",
        "expected_hash": "absent"
    });
    let refusals = [
        client
            .post(format!("http://{addr}/v1/threads/{thread_id}/file-revert"))
            .json(&revert_body)
            .send()
            .await?,
        client
            .post(format!("http://{addr}/v1/threads/{thread_id}/patch-undo"))
            .json(&json!({}))
            .send()
            .await?,
    ];
    for resp in refusals {
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let text = resp.text().await?;
        assert!(text.contains("already has an active turn"), "got: {text}");
    }
    assert_eq!(fs::read_to_string(&file)?, "live");
    // The server-wide snapshot route validates its id first, then admission.
    let resp = client
        .post(format!("http://{addr}/v1/snapshots/not-hex/restore"))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    runtime_threads
        .set_active_turn_for_test(&thread_id, None)
        .await?;
    // Admitted again: the revert now fails on the unknown snapshot id, past
    // the admission and trust gates, without touching the file.
    let resp = client
        .post(format!("http://{addr}/v1/threads/{thread_id}/file-revert"))
        .json(&revert_body)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    assert_eq!(status, StatusCode::CONFLICT, "got: {text}");
    assert!(text.contains("refresh the change record"), "got: {text}");
    assert_eq!(fs::read_to_string(&file)?, "live");

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn retry_endpoint_reuses_dropped_user_text_to_start_a_turn() -> Result<()> {
    let root = std::env::temp_dir().join(format!("deepseek-retry-endpoint-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    let Some((addr, runtime_threads, handle)) =
        spawn_test_server_with_root(root.clone(), sessions_dir).await?
    else {
        return Ok(());
    };
    let thread_id =
        create_seeded_thread(&addr, &runtime_threads, &root, "Retry this request").await?;
    let client = crate::tls::reqwest_client();

    let resp = client
        .post(format!("http://{addr}/v1/threads/{thread_id}/retry"))
        .json(&json!({}))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let retried: serde_json::Value = resp.json().await?;
    let forked_id = retried["thread"]["id"]
        .as_str()
        .context("missing forked thread id")?;
    assert_ne!(forked_id, thread_id);
    assert_eq!(retried["turn"]["thread_id"], forked_id);

    handle.abort();
    Ok(())
}

#[test]
fn restore_snapshot_endpoint_helper_restores_workspace_files() -> Result<()> {
    let _lock = lock_test_env();
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    fs::create_dir_all(&home)?;
    let _home = EnvVarGuard::set("HOME", &home);

    let workspace = root.path().join("workspace");
    fs::create_dir_all(&workspace)?;
    let repo = crate::snapshot::SnapshotRepo::open_or_init(&workspace)?;
    fs::write(workspace.join("a.txt"), "v1")?;
    let snapshot_id = repo.snapshot("pre-turn:1")?;
    fs::write(workspace.join("a.txt"), "v2")?;

    restore_snapshot_for_workspace(&workspace, snapshot_id.as_str())
        .expect("snapshot restore should succeed");
    assert_eq!(fs::read_to_string(workspace.join("a.txt"))?, "v1");
    Ok(())
}

#[tokio::test]
async fn session_create_from_thread_rejects_active_turn() -> Result<()> {
    let Some((addr, runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let created: serde_json::Value = client
        .post(format!("http://{addr}/v1/threads"))
        .json(&json!({}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let thread_id = created["id"]
        .as_str()
        .context("missing thread id")?
        .to_string();

    let harness = crate::core::engine::mock_engine_handle();
    runtime_threads
        .install_test_engine(&thread_id, harness.handle.clone())
        .await?;
    let mut rx_op = harness.rx_op;
    let tx_event = harness.tx_event;
    let (active_tx, active_rx) = oneshot::channel();
    let (finish_tx, finish_rx) = oneshot::channel();
    tokio::spawn(async move {
        if !matches!(rx_op.recv().await, Some(Op::SendMessage(TurnSpec { .. }))) {
            return;
        }
        let _ = tx_event
            .send(EngineEvent::TurnStarted {
                turn_id: "mock_active_session_save".to_string(),
                created_at: chrono::Utc::now(),
                route: None,
                submission_id: None,
            })
            .await;
        let _ = tx_event
            .send(EngineEvent::MessageStarted { index: 0 })
            .await;
        let _ = active_tx.send(());
        let _ = finish_rx.await;
        let _ = tx_event
            .send(EngineEvent::MessageDelta {
                index: 0,
                content: "now complete".to_string(),
            })
            .await;
        let _ = tx_event
            .send(EngineEvent::MessageComplete { index: 0 })
            .await;
        let _ = tx_event
            .send(EngineEvent::TurnComplete {
                usage: Usage {
                    input_tokens: 2,
                    output_tokens: 1,
                    ..Usage::default()
                },
                parent_route_usage: Usage {
                    input_tokens: 2,
                    output_tokens: 1,
                    ..Usage::default()
                },
                routed_usage_dropped_records: 0,
                status: TurnOutcomeStatus::Completed,
                error: None,
                tool_catalog: None,
                base_url: None,
            })
            .await;
    });

    let started: serde_json::Value = client
        .post(format!("http://{addr}/v1/threads/{thread_id}/turns"))
        .json(&json!({ "prompt": "save me while active" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let turn_id = started["turn"]["id"]
        .as_str()
        .context("missing turn id")?
        .to_string();
    tokio::time::timeout(ci_scaled(Duration::from_secs(2)), active_rx)
        .await
        .context("timed out waiting for mock active turn")?
        .context("mock active turn sender dropped")?;
    wait_for_in_progress_item(&client, addr, &thread_id, Duration::from_secs(2)).await?;

    let resp = client
        .post(format!("http://{addr}/v1/sessions"))
        .json(&json!({ "thread_id": thread_id }))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body: serde_json::Value = resp.json().await?;
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("queued or active turn"))
    );

    let _ = finish_tx.send(());
    let terminal =
        wait_for_terminal_turn_status(&client, addr, &thread_id, &turn_id, Duration::from_secs(10))
            .await?;
    assert_eq!(terminal, "completed");

    handle.abort();
    Ok(())
}

#[test]
fn snapshots_endpoint_lists_workspace_snapshots() -> Result<()> {
    let _lock = lock_test_env();
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    fs::create_dir_all(&home)?;
    let _home = EnvVarGuard::set("HOME", &home);

    let workspace = root.path().join("workspace");
    fs::create_dir_all(&workspace)?;
    let repo = crate::snapshot::SnapshotRepo::open_or_init(&workspace)?;
    fs::write(workspace.join("a.txt"), "v1")?;
    repo.snapshot("pre-turn:1")?;
    fs::write(workspace.join("a.txt"), "v2")?;
    repo.snapshot("post-turn:1")?;

    let snapshots = snapshot_entries_for_workspace(&workspace, SnapshotsQuery { limit: Some(1) })
        .expect("snapshot listing should succeed");
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].label, "post-turn:1");
    assert!(snapshots[0].id.len() >= 8);
    assert!(snapshots[0].timestamp > 0);

    let bad_limit = snapshot_entries_for_workspace(&workspace, SnapshotsQuery { limit: Some(101) })
        .expect_err("limit above cap should fail");
    assert_eq!(bad_limit.status, StatusCode::BAD_REQUEST);
    Ok(())
}

/// Seed a sessions directory and start a server against it.
///
/// The route tests below need to control what is on disk, so they cannot use
/// the `spawn_test_server()` convenience that hides its own temp paths.
async fn spawn_server_with_saved_sessions(
    sessions: &[(&str, &str, bool)],
) -> Result<Option<(SocketAddr, PathBuf, tokio::task::JoinHandle<()>)>> {
    let root = std::env::temp_dir().join(format!("codewhale-session-routes-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace)?;
    let manager = crate::session_manager::SessionManager::new(sessions_dir.clone())?;
    for (id, title, archived) in sessions {
        let mut saved = crate::session_manager::create_saved_session_with_id_and_mode(
            (*id).to_string(),
            &[
                codewhale_models::Message {
                    role: Role::User,
                    content: vec![codewhale_models::ContentBlock::Text {
                        text: format!("prompt for {title} with token=hunter2"),
                        cache_control: None,
                    }],
                },
                codewhale_models::Message {
                    role: Role::Assistant,
                    content: vec![codewhale_models::ContentBlock::Text {
                        text: "acknowledged".to_string(),
                        cache_control: None,
                    }],
                },
            ],
            "deepseek-chat",
            &workspace,
            12,
            None,
            Some("agent"),
        );
        saved.metadata.title = (*title).to_string();
        saved.metadata.archived = *archived;
        manager.save_session(&saved)?;
    }
    let Some((addr, _threads, handle)) =
        spawn_test_server_with_root(root, sessions_dir.clone()).await?
    else {
        return Ok(None);
    };
    Ok(Some((addr, sessions_dir, handle)))
}

/// `/v1/sessions/summary` is routed, projects the shared row shape, and hides
/// archived rows until asked — the dashboard's whole session list depends on
/// all three being true at once.
#[tokio::test]
async fn session_summary_route_projects_rows_and_honours_archive_filters() -> Result<()> {
    let Some((addr, _dir, handle)) = spawn_server_with_saved_sessions(&[
        ("sess-active", "Active work", false),
        ("sess-putaway", "Put away", true),
    ])
    .await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let active: Vec<serde_json::Value> = client
        .get(format!(
            "http://127.0.0.1:{}/v1/sessions/summary",
            addr.port()
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        active
            .iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["sess-active"],
        "archived sessions must not appear in the default listing"
    );
    // Field-compatible with /v1/threads/summary — the dashboard renders both
    // with one row renderer.
    for field in [
        "title",
        "preview",
        "model",
        "mode",
        "workspace",
        "updated_at",
    ] {
        assert!(!active[0][field].is_null(), "summary row missing {field}");
    }
    assert_eq!(active[0]["preview"], active[0]["title"]);

    let archived: Vec<serde_json::Value> = client
        .get(format!(
            "http://127.0.0.1:{}/v1/sessions/summary?archived_only=true",
            addr.port()
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        archived
            .iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["sess-putaway"]
    );

    handle.abort();
    Ok(())
}

/// `PATCH /v1/sessions/{id}` renames and archives through the one writer, and
/// reports only what actually moved.
#[tokio::test]
async fn session_patch_route_renames_archives_and_reports_real_changes() -> Result<()> {
    let Some((addr, sessions_dir, handle)) =
        spawn_server_with_saved_sessions(&[("sess-patch", "Before", false)]).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let patched: serde_json::Value = client
        .patch(format!(
            "http://127.0.0.1:{}/v1/sessions/sess-patch",
            addr.port()
        ))
        .json(&json!({ "title": "After", "archived": true }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(patched["session"]["title"], "After");
    assert_eq!(patched["session"]["archived"], true);
    assert_eq!(patched["changes"]["title"], "After");
    assert_eq!(patched["changes"]["archived"], true);

    // Durable, not just echoed back.
    let manager = crate::session_manager::SessionManager::new(sessions_dir)?;
    let reloaded = manager.load_session("sess-patch")?;
    assert_eq!(reloaded.metadata.title, "After");
    assert!(reloaded.metadata.archived);

    // A re-patch to the same state changes nothing, and says so.
    let repeat: serde_json::Value = client
        .patch(format!(
            "http://127.0.0.1:{}/v1/sessions/sess-patch",
            addr.port()
        ))
        .json(&json!({ "archived": true }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert!(
        repeat["changes"]
            .as_object()
            .expect("changes object")
            .is_empty(),
        "a no-op patch must report no changes"
    );

    // An empty body is a client error, not a silent no-op.
    let empty = client
        .patch(format!(
            "http://127.0.0.1:{}/v1/sessions/sess-patch",
            addr.port()
        ))
        .json(&json!({}))
        .send()
        .await?;
    assert_eq!(empty.status(), StatusCode::BAD_REQUEST);

    // A blank title is rejected with the reason, not accepted.
    let blank = client
        .patch(format!(
            "http://127.0.0.1:{}/v1/sessions/sess-patch",
            addr.port()
        ))
        .json(&json!({ "title": "   " }))
        .send()
        .await?;
    assert_eq!(blank.status(), StatusCode::BAD_REQUEST);

    handle.abort();
    Ok(())
}

/// A session the TUI holds open is refused with a typed 409 rather than
/// written behind its back.
#[tokio::test]
async fn session_patch_route_refuses_a_live_session_with_a_conflict() -> Result<()> {
    // The live-session claim is process-global by construction (the embedded
    // API runs inside the TUI process), so this test must not run alongside
    // anything else that claims or clears it.
    let _lock = lock_test_env();
    let Some((addr, _dir, handle)) =
        spawn_server_with_saved_sessions(&[("sess-live", "Held open", false)]).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    crate::session_manager::set_live_session(Some("sess-live"));
    let conflict = client
        .patch(format!(
            "http://127.0.0.1:{}/v1/sessions/sess-live",
            addr.port()
        ))
        .json(&json!({ "title": "Renamed from the dashboard" }))
        .send()
        .await?;
    assert_eq!(conflict.status(), StatusCode::CONFLICT);

    crate::session_manager::set_live_session(None);
    let allowed = client
        .patch(format!(
            "http://127.0.0.1:{}/v1/sessions/sess-live",
            addr.port()
        ))
        .json(&json!({ "title": "Renamed from the dashboard" }))
        .send()
        .await?;
    assert_eq!(allowed.status(), StatusCode::OK);

    handle.abort();
    Ok(())
}

/// `?peek=true` returns the bounded redacted projection, and the plain route
/// still returns the full detail shape.
#[tokio::test]
async fn session_detail_route_serves_a_bounded_redacted_peek_on_request() -> Result<()> {
    let Some((addr, _dir, handle)) =
        spawn_server_with_saved_sessions(&[("sess-peek", "Peekable", false)]).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let peek: serde_json::Value = client
        .get(format!(
            "http://127.0.0.1:{}/v1/sessions/sess-peek?peek=true&entries=12",
            addr.port()
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(peek["session_id"], "sess-peek");
    assert_eq!(peek["live"], false);
    assert!(peek["entries"].as_array().expect("entries").len() <= 12);
    // The seeded prompt carries `token=hunter2`; a peek must not re-emit it.
    let body = peek.to_string();
    assert!(
        !body.contains("hunter2"),
        "peek leaked a credential: {body}"
    );
    // No field a client could read as live turn state.
    for forbidden in ["status", "running", "active", "turn"] {
        assert!(peek.get(forbidden).is_none(), "peek exposed `{forbidden}`");
    }

    let detail: serde_json::Value = client
        .get(format!(
            "http://127.0.0.1:{}/v1/sessions/sess-peek",
            addr.port()
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(detail["metadata"]["id"], "sess-peek");
    assert!(
        detail["messages"].is_array(),
        "the plain route keeps returning full detail"
    );

    handle.abort();
    Ok(())
}

/// An unknown id is a 404 that leaves nothing behind: no live lease or lock
/// file is created for an id with nothing to delete. A malformed id is a
/// 400, not a liveness conflict.
#[tokio::test]
async fn session_delete_returns_404_for_missing_id() -> Result<()> {
    let Some((addr, sessions_dir, handle)) = spawn_server_with_saved_sessions(&[]).await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let resp = client
        .delete(format!("http://{addr}/v1/sessions/nonexistent-id"))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    for sidecar in ["nonexistent-id.live", "nonexistent-id.lock"] {
        assert!(
            !sessions_dir.join(".late-usage").join(sidecar).exists(),
            "an unknown id must not litter {sidecar}"
        );
    }
    let malformed = client
        .delete(format!("http://{addr}/v1/sessions/not.a.session"))
        .send()
        .await?;
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
    handle.abort();
    Ok(())
}

/// #561 / whalescale#255 — extra CORS origins from `RuntimeApiOptions`
/// are added on top of the built-in defaults and propagate through to the
/// `Access-Control-Allow-Origin` response header for preflight requests.
/// Built-in defaults must keep working unchanged.
#[tokio::test]
async fn cors_layer_appends_extra_origins_and_keeps_defaults() -> Result<()> {
    // The cors_layer fn is the layer factory — exercise it through a
    // Router with a single trivial route so we can issue OPTIONS preflights
    // and observe the response headers.
    let extra = vec!["http://localhost:5173".to_string()];
    let layer = cors_layer(&extra);
    let router: Router = Router::new()
        .route("/probe", get(|| async { "ok" }))
        .layer(layer);

    let listener = match TcpListener::bind("127.0.0.1:0").await {
        Ok(listener) => listener,
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => return Ok(()),
        Err(err) => return Err(err.into()),
    };
    let addr = listener.local_addr()?;
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    let client = crate::tls::reqwest_client();

    // The user-supplied origin is allowed.
    let resp = client
        .request(reqwest::Method::OPTIONS, format!("http://{addr}/probe"))
        .header("Origin", "http://localhost:5173")
        .header("Access-Control-Request-Method", "GET")
        .send()
        .await?;
    assert_eq!(
        resp.headers()
            .get("access-control-allow-origin")
            .and_then(|v| v.to_str().ok()),
        Some("http://localhost:5173")
    );

    // A built-in default origin still works.
    let resp = client
        .request(reqwest::Method::OPTIONS, format!("http://{addr}/probe"))
        .header("Origin", "http://localhost:1420")
        .header("Access-Control-Request-Method", "GET")
        .send()
        .await?;
    assert_eq!(
        resp.headers()
            .get("access-control-allow-origin")
            .and_then(|v| v.to_str().ok()),
        Some("http://localhost:1420")
    );

    // An origin that's neither configured nor a default is rejected
    // (CorsLayer omits the Allow-Origin header on mismatch).
    let resp = client
        .request(reqwest::Method::OPTIONS, format!("http://{addr}/probe"))
        .header("Origin", "http://malicious.example")
        .header("Access-Control-Request-Method", "GET")
        .send()
        .await?;
    assert!(
        resp.headers().get("access-control-allow-origin").is_none(),
        "non-allowed origin must not be echoed back"
    );

    handle.abort();
    Ok(())
}

/// #561 — invalid origins (non-ASCII, etc.) are skipped without aborting
/// the layer build.
#[test]
fn cors_layer_skips_invalid_origins() {
    let extras = vec![
        "http://valid.example".to_string(),
        // Embedded NUL char makes `HeaderValue::from_str` fail.
        "http://invalid.example\0".to_string(),
        "  ".to_string(), // whitespace-only is dropped
    ];
    // Should not panic.
    let _ = cors_layer(&extras);
}

#[tokio::test]
async fn cors_layer_exposes_stream_capabilities_for_allowed_origins() -> Result<()> {
    use axum::handler::Handler;

    let handler = (|| async {
        (
            [
                ("x-codewhale-stream-end", "1"),
                ("x-codewhale-event-progress", "1"),
            ],
            "event: stream.end\n\n",
        )
    })
    .layer(cors_layer(&["http://localhost:5173".to_string()]));
    for origin in ["http://localhost:1420", "http://localhost:5173"] {
        let request = Request::builder()
            .uri("/events")
            .header("Origin", origin)
            .body(axum::body::Body::empty())?;
        let response = handler.clone().call(request, ()).await;
        assert_eq!(response.headers()["access-control-allow-origin"], origin);
        for capability in ["x-codewhale-stream-end", "x-codewhale-event-progress"] {
            assert_eq!(response.headers()[capability], "1");
            assert!(
                response.headers()["access-control-expose-headers"]
                    .to_str()?
                    .split(',')
                    .any(|name| name.trim().eq_ignore_ascii_case(capability)),
                "{capability} must be readable cross-origin"
            );
        }
    }
    let request = Request::builder()
        .uri("/events")
        .header("Origin", "http://malicious.example")
        .body(axum::body::Body::empty())?;
    let rejected = handler.call(request, ()).await;
    assert!(
        rejected
            .headers()
            .get("access-control-allow-origin")
            .is_none()
    );
    Ok(())
}

/// #562 / whalescale#256 — `PATCH /v1/threads/{id}` accepts the new
/// fields (allow_shell, trust_mode, auto_approve, model, mode, title,
/// system_prompt). Legacy mode aliases remain accepted as one-way inputs and
/// the response returns the canonical product mode. Each field is
/// independently optional; an empty string clears `title` / `system_prompt`
/// back to None.
#[tokio::test]
async fn patch_thread_accepts_extended_field_set() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let created: serde_json::Value = client
        .post(format!("http://{addr}/v1/threads"))
        .json(&json!({
            "model": "deepseek-v4-flash",
            "mode": "agent"
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let thread_id = created["id"]
        .as_str()
        .context("missing thread id")?
        .to_string();

    // Patch every new field at once.
    let patched: serde_json::Value = client
        .patch(format!("http://{addr}/v1/threads/{thread_id}"))
        .json(&json!({
            "allow_shell": true,
            "trust_mode": true,
            "auto_approve": true,
            "model": "deepseek-v4-pro",
            "mode": "yolo",
            "title": "Whalescale UI test thread",
            "system_prompt": "You are a useful assistant."
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    assert_eq!(patched["allow_shell"], true);
    assert_eq!(patched["trust_mode"], true);
    assert_eq!(patched["auto_approve"], true);
    assert_eq!(patched["model"], "deepseek-v4-pro");
    assert_eq!(patched["mode"], "agent");
    assert_eq!(patched["permission_posture"], "full_access");
    assert_eq!(patched["title"], "Whalescale UI test thread");
    assert_eq!(patched["system_prompt"], "You are a useful assistant.");

    // Empty string clears title back to None.
    let cleared: serde_json::Value = client
        .patch(format!("http://{addr}/v1/threads/{thread_id}"))
        .json(&json!({ "title": "" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert!(
        cleared["title"].is_null() || !cleared.as_object().unwrap().contains_key("title"),
        "empty title must serialize as None: {cleared:?}"
    );

    // Empty patch (no fields) is still rejected.
    let empty = client
        .patch(format!("http://{addr}/v1/threads/{thread_id}"))
        .json(&json!({}))
        .send()
        .await?;
    assert_eq!(empty.status(), StatusCode::BAD_REQUEST);

    // Empty model is rejected (validation).
    let bad_model = client
        .patch(format!("http://{addr}/v1/threads/{thread_id}"))
        .json(&json!({ "model": "  " }))
        .send()
        .await?;
    assert_eq!(bad_model.status(), StatusCode::BAD_REQUEST);

    handle.abort();
    Ok(())
}

/// #563 / whalescale#260 — `archived_only=true` returns archived-only
/// (no active threads), distinct from `include_archived=true` which
/// returns both.
#[tokio::test]
async fn list_threads_archived_only_filter_matches_only_archived() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    // Two threads — keep one active, archive the other.
    let active: serde_json::Value = client
        .post(format!("http://{addr}/v1/threads"))
        .json(&json!({}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let active_id = active["id"].as_str().unwrap().to_string();

    let archived: serde_json::Value = client
        .post(format!("http://{addr}/v1/threads"))
        .json(&json!({}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let archived_id = archived["id"].as_str().unwrap().to_string();

    client
        .patch(format!("http://{addr}/v1/threads/{archived_id}"))
        .json(&json!({ "archived": true }))
        .send()
        .await?
        .error_for_status()?;

    // Default (active only) → only the unarchived one.
    let active_list: serde_json::Value = client
        .get(format!("http://{addr}/v1/threads"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let ids: Vec<&str> = active_list
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["id"].as_str())
        .collect();
    assert!(ids.contains(&active_id.as_str()));
    assert!(!ids.contains(&archived_id.as_str()));

    // archived_only=true → only the archived one.
    let archived_list: serde_json::Value = client
        .get(format!("http://{addr}/v1/threads?archived_only=true"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let ids: Vec<&str> = archived_list
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["id"].as_str())
        .collect();
    assert_eq!(ids, vec![archived_id.as_str()]);

    // archived_only=true takes precedence over include_archived=true.
    let archived_list: serde_json::Value = client
        .get(format!(
            "http://{addr}/v1/threads?include_archived=true&archived_only=true"
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let ids: Vec<&str> = archived_list
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["id"].as_str())
        .collect();
    assert_eq!(ids, vec![archived_id.as_str()]);

    // Same filter works on the summary endpoint.
    let summary: serde_json::Value = client
        .get(format!(
            "http://{addr}/v1/threads/summary?archived_only=true&limit=10"
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let summary_ids: Vec<&str> = summary
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["id"].as_str())
        .collect();
    assert_eq!(summary_ids, vec![archived_id.as_str()]);

    handle.abort();
    Ok(())
}

/// The exact-ID filter must run before the result limit so an owned older
/// thread remains visible even when newer threads belong to other callers.
#[tokio::test]
async fn list_thread_summary_filters_ids_before_limit() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let mut owned_ids = Vec::new();

    for _ in 0..2 {
        let created: serde_json::Value = client
            .post(format!("http://{addr}/v1/threads"))
            .json(&json!({}))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        owned_ids.push(
            created["id"]
                .as_str()
                .context("missing thread id")?
                .to_string(),
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    for _ in 0..8 {
        client
            .post(format!("http://{addr}/v1/threads"))
            .json(&json!({}))
            .send()
            .await?
            .error_for_status()?;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let recent: serde_json::Value = client
        .get(format!(
            "http://{addr}/v1/threads/summary?limit=8&include_archived=true"
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let recent_ids: Vec<&str> = recent
        .as_array()
        .context("summary must be an array")?
        .iter()
        .filter_map(|thread| thread["id"].as_str())
        .collect();
    assert!(
        owned_ids
            .iter()
            .all(|id| !recent_ids.contains(&id.as_str()))
    );

    let mut filtered_url = reqwest::Url::parse(&format!(
        "http://{addr}/v1/threads/summary?limit=8&include_archived=true"
    ))?;
    filtered_url
        .query_pairs_mut()
        .append_pair("thread_ids", &owned_ids.join(","));
    let filtered: serde_json::Value = client
        .get(filtered_url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let filtered_ids: Vec<&str> = filtered
        .as_array()
        .context("filtered summary must be an array")?
        .iter()
        .filter_map(|thread| thread["id"].as_str())
        .collect();
    assert_eq!(filtered_ids.len(), owned_ids.len());
    assert!(
        owned_ids
            .iter()
            .all(|id| filtered_ids.contains(&id.as_str()))
    );

    let mut oversized_url =
        reqwest::Url::parse(&format!("http://{addr}/v1/threads/summary?limit=8"))?;
    oversized_url
        .query_pairs_mut()
        .append_pair("thread_ids", &vec!["fixture-id"; 201].join(","));
    let rejected = client.get(oversized_url).send().await?;
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
    let at_limit = format!(
        "http://{addr}/v1/threads/summary?limit=8&thread_ids={}",
        vec!["fixture-id"; 200].join(",")
    );
    assert_eq!(client.get(at_limit).send().await?.status(), StatusCode::OK);

    handle.abort();
    Ok(())
}

/// #564 / whalescale#261 — `GET /v1/usage` aggregates per-turn token +
/// cost data. With no threads the response is well-formed and totals are
/// zero with empty buckets (never a 404).
#[tokio::test]
async fn usage_endpoint_returns_empty_aggregation_for_fresh_store() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let body: serde_json::Value = client
        .get(format!("http://{addr}/v1/usage"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(body["group_by"], "day");
    assert_eq!(body["totals"]["input_tokens"], 0);
    assert_eq!(body["totals"]["output_tokens"], 0);
    assert_eq!(body["totals"]["turns"], 0);
    assert!(
        body["buckets"].as_array().unwrap().is_empty(),
        "buckets must be empty when no turns exist: {body}"
    );

    // group_by query options are validated.
    let bad_group = client
        .get(format!("http://{addr}/v1/usage?group_by=galaxy"))
        .send()
        .await?;
    assert_eq!(bad_group.status(), StatusCode::BAD_REQUEST);

    // Each accepted group_by value succeeds.
    for gb in ["day", "model", "provider", "thread"] {
        let resp = client
            .get(format!("http://{addr}/v1/usage?group_by={gb}"))
            .send()
            .await?;
        assert!(resp.status().is_success(), "group_by={gb} failed: {resp:?}");
    }

    // Bad ISO-8601 timestamp rejected.
    let bad_since = client
        .get(format!("http://{addr}/v1/usage?since=not-a-date"))
        .send()
        .await?;
    assert_eq!(bad_since.status(), StatusCode::BAD_REQUEST);

    // since > until rejected.
    let inverted = client
        .get(format!(
            "http://{addr}/v1/usage?since=2030-01-02T00:00:00Z&until=2030-01-01T00:00:00Z"
        ))
        .send()
        .await?;
    assert_eq!(inverted.status(), StatusCode::BAD_REQUEST);

    handle.abort();
    Ok(())
}

/// `GET /v1/threads/{id}/usage` reports the thread-scoped token + cost
/// totals the GUI's session-cost surface consumes. A thread with no turns
/// yields zeroed totals (both published currencies present in the payload);
/// a thread that does not exist is a 404, never zeros, so the GUI cannot
/// mistake a typo'd id for a free conversation.
#[tokio::test]
async fn thread_usage_endpoint_scopes_totals_to_one_thread() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let created: serde_json::Value = client
        .post(format!("http://{addr}/v1/threads"))
        .json(&json!({}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let id = created["id"].as_str().expect("thread id").to_string();

    let usage: serde_json::Value = client
        .get(format!("http://{addr}/v1/threads/{id}/usage"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(usage["thread_id"], id);
    assert_eq!(usage["totals"]["input_tokens"], 0);
    assert_eq!(usage["totals"]["output_tokens"], 0);
    assert_eq!(usage["totals"]["cost_usd"], 0.0);
    assert_eq!(usage["totals"]["cost_cny"], 0.0);
    assert_eq!(usage["totals"]["turns"], 0);
    assert_eq!(usage["totals"]["priced_turns"], 0);
    assert_eq!(usage["totals"]["cny_priced_turns"], 0);
    assert_eq!(usage["totals"]["cny_unpriced_turns"], 0);
    assert_eq!(usage["totals"]["cny_unpriced_reasons"], json!([]));
    assert_eq!(usage["totals"]["cost_complete"], true);

    let missing = client
        .get(format!("http://{addr}/v1/threads/does-not-exist/usage"))
        .send()
        .await?;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    handle.abort();
    Ok(())
}

/// `GET /v1/threads/{id}/receipt` and its per-turn form sit behind the same
/// bearer boundary as every `/v1` route, answer with the shared receipt
/// shape, and 404 an unknown thread or a turn from elsewhere.
#[tokio::test]
async fn thread_receipt_routes_require_auth_and_return_the_receipt_shape() -> Result<()> {
    let root = std::env::temp_dir().join(format!("codewhale-receipt-api-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    let token = "receipt-test-token".to_string();
    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root_and_token(root, sessions_dir, Some(token.clone())).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let created: serde_json::Value = client
        .post(format!("http://{addr}/v1/threads"))
        .bearer_auth(&token)
        .json(&json!({}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let id = created["id"].as_str().expect("thread id").to_string();

    for path in [
        format!("/v1/threads/{id}/receipt"),
        format!("/v1/threads/{id}/turns/turn_x/receipt"),
    ] {
        let anonymous = client.get(format!("http://{addr}{path}")).send().await?;
        assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED, "{path}");
    }

    let receipt: serde_json::Value = client
        .get(format!("http://{addr}/v1/threads/{id}/receipt"))
        .bearer_auth(&token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(receipt["schema_id"], "codewhale.receipt/v1");
    assert_eq!(receipt["source"]["kind"], "thread");
    assert_eq!(receipt["source"]["id"], id);
    assert_eq!(receipt["actions"], json!([]));
    assert_eq!(receipt["totals"]["commands"], 0);
    assert!(receipt["not_recorded"].is_array());

    let foreign_turn = client
        .get(format!(
            "http://{addr}/v1/threads/{id}/turns/turn_x/receipt"
        ))
        .bearer_auth(&token)
        .send()
        .await?;
    assert_eq!(foreign_turn.status(), StatusCode::NOT_FOUND);
    let missing = client
        .get(format!("http://{addr}/v1/threads/thr_missing/receipt"))
        .bearer_auth(&token)
        .send()
        .await?;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    handle.abort();
    Ok(())
}

/// The four canonical thread-history controls mutate or inspect shared store
/// state, so they sit behind the same bearer + workspace-scope middleware as
/// every other `/v1` route: anonymous and wrong-token posts are refused before
/// the handler runs. Authorized coverage lives in
/// `runtime_store_convergence.rs`.
#[tokio::test]
async fn thread_history_operation_routes_require_auth() -> Result<()> {
    let root = std::env::temp_dir().join(format!("codewhale-history-auth-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    let token = "history-auth-test-token".to_string();
    let Some((addr, _threads, handle)) =
        spawn_test_server_with_root_and_token(root, sessions_dir, Some(token.clone())).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    for path in [
        "/v1/thread-history/operations/lookup",
        "/v1/thread-history/operations/recover",
        "/v1/thread-history/mutate",
        "/v1/thread-history/import",
    ] {
        let anonymous = client
            .post(format!("http://{addr}{path}"))
            .json(&json!({}))
            .send()
            .await?;
        assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED, "{path}");
        let wrong = client
            .post(format!("http://{addr}{path}"))
            .bearer_auth("not-the-runtime-token")
            .json(&json!({}))
            .send()
            .await?;
        assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED, "{path}");
        // The token gate admits the request; the handler then rejects the
        // empty body itself, which is the same 4xx the route gave when these
        // were reachable without auth — anything but 401 proves we reached it.
        let authorized = client
            .post(format!("http://{addr}{path}"))
            .bearer_auth(&token)
            .json(&json!({}))
            .send()
            .await?;
        assert_ne!(authorized.status(), StatusCode::UNAUTHORIZED, "{path}");
    }

    handle.abort();
    Ok(())
}

/// `GET /v1/approvals` serves the account-wide approval history behind the
/// approvals log: decided rows carry their outcome + decision time, pending
/// asks read "pending" with no decision time, newest ask first. A corrupt
/// session log is skipped (warned server-side), never a 500.
#[tokio::test]
async fn approvals_endpoint_lists_decided_and_pending_newest_first() -> Result<()> {
    use crate::approval_log::{ApprovalOutcome, ApprovalReceipt, ApprovalReceiptStore};
    use chrono::{DateTime, Utc};

    let root = std::env::temp_dir().join(format!("codewhale-approvals-api-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    let Some((addr, _threads, handle)) =
        spawn_test_server_with_root(root, sessions_dir.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let store = ApprovalReceiptStore::new(sessions_dir);

    fn at(hour: u32) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(&format!("2026-09-10T{hour:02}:00:00Z"))
            .expect("fixture time")
            .to_utc()
    }
    // Decided pair in one session, a still-pending ask in another, a corrupt
    // log in a third. Fixed stamps keep the newest-first order exact.
    store.append(
        "sess-decided",
        &ApprovalReceipt::Asked {
            approval_id: "tool-1".into(),
            tool_call_id: "tool-1".into(),
            tool_name: "exec_shell".into(),
            created_at: at(10),
            target: Some("api.example.com".into()),
            summary: Some("curl api.example.com".into()),
        },
    )?;
    store.append(
        "sess-decided",
        &ApprovalReceipt::Decided {
            approval_id: "tool-1".into(),
            tool_call_id: "tool-1".into(),
            outcome: ApprovalOutcome::Denied,
            created_at: at(11),
            decided_by: Some(crate::approval_log::ApprovalDecider::User),
            grant_id: None,
            matched_label: None,
        },
    )?;
    store.append(
        "sess-pending",
        &ApprovalReceipt::Asked {
            approval_id: "tool-2".into(),
            tool_call_id: "tool-2".into(),
            tool_name: "write_file".into(),
            created_at: at(12),
            target: None,
            summary: None,
        },
    )?;
    let corrupt_dir = store.log_path("sess-corrupt")?;
    fs::create_dir_all(corrupt_dir.parent().expect("log parent"))?;
    fs::write(&corrupt_dir, "not-json\n")?;

    let rows: serde_json::Value = client
        .get(format!("http://{addr}/v1/approvals"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(rows.as_array().expect("rows").len(), 2);
    assert_eq!(rows[0]["approval_id"], "tool-2");
    assert_eq!(rows[0]["tool_name"], "write_file");
    assert_eq!(rows[0]["outcome"], "pending");
    assert!(rows[0]["decided_at"].is_null());
    assert_eq!(rows[1]["approval_id"], "tool-1");
    assert_eq!(rows[1]["tool_name"], "exec_shell");
    assert_eq!(rows[1]["outcome"], "denied");
    assert_eq!(rows[1]["decided_by"], "user");
    assert_eq!(rows[1]["target"], "api.example.com");
    assert_eq!(rows[1]["summary"], "curl api.example.com");
    assert!(rows[0].get("target").is_none());
    assert!(rows[0].get("summary").is_none());
    assert!(rows[0].get("decided_by").is_none());
    assert_eq!(rows[1]["asked_at"], "2026-09-10T10:00:00Z");
    assert_eq!(rows[1]["decided_at"], "2026-09-10T11:00:00Z");

    let one: serde_json::Value = client
        .get(format!("http://{addr}/v1/approvals?limit=1"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(one.as_array().expect("rows").len(), 1);
    assert_eq!(one[0]["approval_id"], "tool-2");

    handle.abort();
    Ok(())
}

/// `PUT /v1/sessions` persists the thread's audited cost with the same
/// field semantics the TUI writer uses: parent-turn spend in
/// `session_cost_*`, routed-child spend in `subagent_cost_*`, so a session
/// that already carries TUI-written sub-agent spend keeps one display total
/// instead of counting child spend twice. Coverage rides along (#4318):
/// `coverage_recorded` is set and the CNY counters describe the provider's
/// own CNY rows, so a reload reads the totals as known, not legacy-unknown.
#[tokio::test]
async fn session_save_merges_thread_cost_split_and_records_coverage() -> Result<()> {
    let root = std::env::temp_dir().join(format!("deepseek-session-cost-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    let Some((addr, runtime_threads, handle)) =
        spawn_test_server_with_root(root.clone(), sessions_dir.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let created: serde_json::Value = client
        .post(format!("http://{addr}/v1/threads"))
        .json(&json!({
            "model": "deepseek-v4-flash",
            "workspace": root.join("workspace")
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let thread_id = created["id"]
        .as_str()
        .context("missing thread id")?
        .to_string();

    // Seed one completed turn: a DeepSeek first-party parent call plus a
    // large routed-child (sub-agent) call, both priced.
    let store = runtime_threads.test_store();
    let now = Utc::now();
    let mut turn = TurnRecord {
        decision_receipts: Vec::new(),
        max_output_tokens: None,
        schema_version: 2,
        id: "turn_cost_merge".to_string(),
        thread_id: thread_id.clone(),
        status: RuntimeTurnStatus::Completed,
        input_summary: "cost merge".to_string(),
        created_at: now,
        started_at: Some(now),
        ended_at: Some(now),
        duration_ms: Some(0),
        usage: Some(Usage {
            input_tokens: 10_000,
            output_tokens: 1_000,
            ..Usage::default()
        }),
        model_request_diagnostics: None,
        operation_activity: None,
        routing_settlement: false,
        effective_route_usage: None,
        permission_posture: None,
        mode: None,
        effective_provider: None,
        effective_provider_id: None,
        effective_openrouter_vendor: None,
        effective_billing_surface: None,
        effective_endpoint_fingerprint: None,
        effective_provider_live_pricing: None,
        effective_billing_mode: None,
        effective_dispatched_at: None,
        effective_model: None,
        routed_usage: vec![crate::cost_status::EffectiveRouteUsage {
            route: crate::cost_status::EffectiveRouteEnvelope {
                openrouter_vendor: None,
                provider: ProviderKind::Deepseek,
                provider_identity: ProviderKind::Deepseek.as_str().to_string(),
                model: "deepseek-v4-flash".to_string(),
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE.to_string()),
                endpoint_fingerprint: None,
                provider_live_pricing: None,
                billing_mode: crate::cost_status::RouteBillingMode::Metered,
                dispatched_at: now,
            },
            usage: Usage {
                input_tokens: 20_000_000,
                output_tokens: 2_000_000,
                ..Usage::default()
            },
        }],
        routed_usage_drop_records: Vec::new(),
        routed_usage_source_ids: Vec::new(),
        routed_usage_dropped_records: 0,
        error: None,
        item_ids: Vec::new(),
        steer_count: 0,
        agent_mail_message_id: None,
        artifacts: Vec::new(),
        workspace: None,
        workspace_snapshots: Vec::new(),
    };
    // Mirror `TurnRecord::persist_effective_route` (private to the runtime
    // threads module): persist the route envelope onto the turn so the
    // recorded-time pricer sees a complete dispatch record.
    turn.effective_provider = Some(ProviderKind::Deepseek.as_str().to_string());
    turn.effective_provider_id = Some(ProviderKind::Deepseek.as_str().to_string());
    turn.effective_billing_surface =
        Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE.to_string());
    turn.effective_endpoint_fingerprint = None;
    turn.effective_billing_mode = Some(crate::cost_status::RouteBillingMode::Metered);
    turn.effective_dispatched_at = Some(now);
    turn.effective_model = Some("deepseek-v4-flash".to_string());
    store.save_turn(&turn)?;
    let mut thread = store.load_thread(&thread_id)?;
    thread.latest_turn_id = Some(turn.id.clone());
    store.save_thread(&thread)?;

    let endpoint: serde_json::Value = client
        .get(format!("http://{addr}/v1/threads/{thread_id}/usage"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let combined_usd = endpoint["totals"]["cost_usd"]
        .as_f64()
        .context("combined cost_usd")?;
    assert!(combined_usd > 0.0);

    // Pre-existing session file carrying TUI-written sub-agent spend — the
    // shape a GUI save of a resumed TUI session merges into.
    let session_manager = crate::session_manager::SessionManager::new(sessions_dir.clone())?;
    let mut prior = crate::session_manager::create_saved_session_with_id_and_mode(
        "sess_cost_merge".to_string(),
        &[],
        "deepseek-v4-flash",
        &root,
        0,
        None,
        None,
    );
    prior.metadata.cost.subagent_cost_usd = 0.5;
    prior.metadata.cost.subagent_cost_cny = 3.0;
    session_manager.save_session(&prior)?;

    client
        .put(format!("http://{addr}/v1/sessions"))
        .json(&json!({
            "thread_id": thread_id,
            "session_id": "sess_cost_merge"
        }))
        .send()
        .await?
        .error_for_status()?;

    let saved = session_manager.load_session_by_prefix("sess_cost_merge")?;
    let cost = &saved.metadata.cost;
    // Parent-turn spend lands in the parent field only, so the display
    // total keeps exactly one copy of the child spend.
    assert!(cost.session_cost_usd > 0.0);
    assert!(cost.session_cost_usd < combined_usd);
    let child_usd = combined_usd - cost.session_cost_usd;
    assert!(child_usd > 0.5);
    assert_eq!(cost.subagent_cost_usd, child_usd);
    assert_eq!(
        (cost.session_cost_usd + cost.subagent_cost_usd - combined_usd).abs(),
        0.0
    );
    assert!(cost.subagent_cost_cny > 3.0);
    assert!(cost.session_cost_cny > 0.0);
    // Coverage the runtime computed reads back as known, with CNY counted
    // under the provider's own CNY row.
    assert!(cost.coverage_recorded);
    assert!(!cost.coverage_is_legacy_unknown());
    assert_eq!(cost.priced_turns, 1);
    assert_eq!(cost.unpriced_turns, 0);
    assert_eq!(cost.cny_priced_turns, 1);
    assert_eq!(cost.cny_unpriced_turns, 0);
    assert!(cost.unpriced_reasons.is_empty());
    assert!(
        cost.cny_unpriced_reasons.is_empty(),
        "DeepSeek first-party parent is CNY-priced; reasons must not invent a gap"
    );

    // Re-saving is idempotent: max-merge never re-adds the same spend.
    client
        .put(format!("http://{addr}/v1/sessions"))
        .json(&json!({
            "thread_id": thread_id,
            "session_id": "sess_cost_merge"
        }))
        .send()
        .await?
        .error_for_status()?;
    let resaved = session_manager.load_session_by_prefix("sess_cost_merge")?;
    assert_eq!(
        resaved.metadata.cost.session_cost_usd,
        cost.session_cost_usd
    );
    assert_eq!(
        resaved.metadata.cost.subagent_cost_usd,
        cost.subagent_cost_usd
    );

    handle.abort();
    Ok(())
}

/// USD-unpriced parent + DeepSeek-priced child: coverage reasons persist
/// with the parent columns (#4318) while routed spend still lands only in
/// `subagent_cost_*` (no double-count on reload).
#[tokio::test]
async fn session_save_persists_parent_cny_unpriced_reasons_without_double_count() -> Result<()> {
    let root = std::env::temp_dir().join(format!("deepseek-session-cny-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    let Some((addr, runtime_threads, handle)) =
        spawn_test_server_with_root(root.clone(), sessions_dir.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let created: serde_json::Value = client
        .post(format!("http://{addr}/v1/threads"))
        .json(&json!({
            "model": "gpt-5.5",
            "workspace": root.join("workspace")
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let thread_id = created["id"]
        .as_str()
        .context("missing thread id")?
        .to_string();

    let store = runtime_threads.test_store();
    let now = Utc::now();
    let mut turn = TurnRecord {
        decision_receipts: Vec::new(),
        max_output_tokens: None,
        schema_version: 2,
        id: "turn_cny_reasons".to_string(),
        thread_id: thread_id.clone(),
        status: RuntimeTurnStatus::Completed,
        input_summary: "cny reasons".to_string(),
        created_at: now,
        started_at: Some(now),
        ended_at: Some(now),
        duration_ms: Some(0),
        usage: Some(Usage {
            input_tokens: 10_000,
            output_tokens: 1_000,
            ..Usage::default()
        }),
        model_request_diagnostics: None,
        operation_activity: None,
        routing_settlement: false,
        effective_route_usage: None,
        permission_posture: None,
        mode: None,
        effective_provider: None,
        effective_provider_id: None,
        effective_openrouter_vendor: None,
        effective_billing_surface: None,
        effective_endpoint_fingerprint: None,
        effective_provider_live_pricing: None,
        effective_billing_mode: None,
        effective_dispatched_at: None,
        effective_model: None,
        routed_usage: vec![crate::cost_status::EffectiveRouteUsage {
            route: crate::cost_status::EffectiveRouteEnvelope {
                openrouter_vendor: None,
                provider: ProviderKind::Deepseek,
                provider_identity: ProviderKind::Deepseek.as_str().to_string(),
                model: "deepseek-v4-flash".to_string(),
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE.to_string()),
                endpoint_fingerprint: None,
                provider_live_pricing: None,
                billing_mode: crate::cost_status::RouteBillingMode::Metered,
                dispatched_at: now,
            },
            usage: Usage {
                input_tokens: 20_000,
                output_tokens: 2_000,
                ..Usage::default()
            },
        }],
        routed_usage_drop_records: Vec::new(),
        routed_usage_source_ids: Vec::new(),
        routed_usage_dropped_records: 0,
        error: None,
        item_ids: Vec::new(),
        steer_count: 0,
        agent_mail_message_id: None,
        artifacts: Vec::new(),
        workspace: None,
        workspace_snapshots: Vec::new(),
    };
    turn.effective_provider = Some(ProviderKind::Openai.as_str().to_string());
    turn.effective_provider_id = Some(ProviderKind::Openai.as_str().to_string());
    turn.effective_billing_surface = Some(crate::pricing::UNCLASSIFIED_BILLING_SURFACE.to_string());
    turn.effective_endpoint_fingerprint = None;
    turn.effective_billing_mode = Some(crate::cost_status::RouteBillingMode::Metered);
    turn.effective_dispatched_at = Some(now);
    turn.effective_model = Some("gpt-5.5".to_string());
    store.save_turn(&turn)?;
    let mut thread = store.load_thread(&thread_id)?;
    thread.latest_turn_id = Some(turn.id.clone());
    store.save_thread(&thread)?;

    client
        .put(format!("http://{addr}/v1/sessions"))
        .json(&json!({
            "thread_id": thread_id,
            "session_id": "sess_cny_reasons"
        }))
        .send()
        .await?
        .error_for_status()?;

    let session_manager = crate::session_manager::SessionManager::new(sessions_dir)?;
    let saved = session_manager.load_session_by_prefix("sess_cny_reasons")?;
    let cost = &saved.metadata.cost;
    assert_eq!(
        cost.session_cost_usd, 0.0,
        "unpriced parent must not invent USD"
    );
    assert!(
        cost.subagent_cost_usd > 0.0,
        "routed DeepSeek child is priced"
    );
    assert_eq!(
        cost.priced_turns, 0,
        "parent coverage columns stay parent-only"
    );
    assert_eq!(cost.unpriced_turns, 1);
    assert_eq!(cost.cny_unpriced_turns, 1);
    assert!(!cost.unpriced_reasons.is_empty());
    assert!(
        !cost.cny_unpriced_reasons.is_empty(),
        "CNY reasons must persist with the parent coverage counts (#4318)"
    );
    assert!(cost.coverage_recorded);
    assert!(!cost.coverage_is_legacy_unknown());

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn watch_client_token_refuses_runtime_mutations_and_upgrades() -> Result<()> {
    let _lock = lock_test_env();
    let root = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("CODEWHALE_HOME", root.path());
    let Some((addr, _, handle)) = spawn_test_server_with_root_and_token(
        root.path().to_path_buf(),
        root.path().join("sessions"),
        Some("watch-scope-fixture-master".to_string()),
    )
    .await?
    else {
        bail!("the owned client-token HTTP fixture could not bind");
    };
    let client = crate::tls::reqwest_client();
    let base = format!("http://{addr}");
    let info: Value = client
        .get(format!("{base}/v1/runtime/info"))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(info["capabilities"]["client_token_intents"], true);
    let mint = |body: Value| {
        client
            .post(format!("{base}/v1/auth/client-tokens"))
            .bearer_auth("watch-scope-fixture-master")
            .json(&body)
    };
    let watch: Value = mint(json!({"device_id": "read-device", "label": "cwc-seat:drive"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(watch["intent"], "watch");
    let watch_token = watch["token"].as_str().context("watch token")?;
    let reads = client
        .get(format!("{base}/v1/threads/summary"))
        .bearer_auth(watch_token)
        .send()
        .await?;
    assert_eq!(reads.status(), StatusCode::OK);
    for (method, path) in [
        (Method::POST, "/v1/threads"),
        (Method::PUT, "/v1/workspace/files"),
        (Method::DELETE, "/v1/memory"),
        (Method::POST, "/v1/approvals/not-pending"),
    ] {
        let refused = client
            .request(method, format!("{base}{path}"))
            .bearer_auth(watch_token)
            .json(&json!({}))
            .send()
            .await?;
        assert_eq!(refused.status(), StatusCode::FORBIDDEN, "{path}");
    }
    let upgrade = client
        .get(format!("{base}/v1/threads/summary"))
        .bearer_auth(watch_token)
        .header(header::UPGRADE, "websocket")
        .send()
        .await?;
    assert_eq!(upgrade.status(), StatusCode::FORBIDDEN);
    let drive: Value = mint(json!({"device_id": "write-device", "intent": "drive"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(drive["intent"], "drive");
    let created = client
        .post(format!("{base}/v1/threads"))
        .bearer_auth(drive["token"].as_str().context("drive token")?)
        .json(&json!({}))
        .send()
        .await?;
    assert_eq!(created.status(), StatusCode::CREATED);
    let invalid = mint(json!({"device_id": "bad-device", "intent": "admin"}))
        .send()
        .await?;
    assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let revoked = client
        .delete(format!(
            "{base}/v1/auth/client-tokens/{}",
            watch["id"].as_str().context("watch id")?
        ))
        .bearer_auth("watch-scope-fixture-master")
        .send()
        .await?;
    assert_eq!(revoked.status(), StatusCode::NO_CONTENT);
    let expired_read = client
        .get(format!("{base}/v1/threads/summary"))
        .bearer_auth(watch_token)
        .send()
        .await?;
    assert_eq!(expired_read.status(), StatusCode::UNAUTHORIZED);
    handle.abort();
    Ok(())
}

#[tokio::test]
async fn runtime_info_reports_bind_state() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let info: serde_json::Value = client
        .get(format!("http://{addr}/v1/runtime/info"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(info["service"], "codewhale-runtime-api");
    assert_eq!(info["runtime_api_version"], "1.0");
    assert_eq!(info["codewhale_version"], info["version"]);
    let commit = info["codewhale_commit"]
        .as_str()
        .expect("runtime build commit must be a string");
    // Since #5245 the commit is env-stamped only: a stamped build (CI /
    // release / a `CODEWHALE_BUILD_SHA=…` dogfood build) reports a full 40-hex
    // sha; an unstamped local build honestly reports "unknown" rather than
    // reading the checkout. Both are valid provenance — a fabricated sha
    // would be the bug.
    if commit == "unknown" {
        // Unstamped local build — the honest absence.
    } else {
        assert_eq!(commit.len(), 40, "a stamped build commit is a full sha");
        assert!(
            commit.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "runtime build commit must be hexadecimal"
        );
    }
    assert_eq!(info["bind_host"], "127.0.0.1");
    assert_eq!(info["auth_required"], false);
    assert!(info["version"].is_string());
    assert_eq!(info["transports"], json!(["http", "sse"]));
    assert_eq!(info["capabilities"]["threads"], true);
    assert_eq!(
        info["capabilities"]["turn_operation_idempotency"], true,
        "clients must have an explicit fail-closed gate before sending operation_key"
    );
    assert_eq!(info["capabilities"]["turn_operation_lookup"], true);
    assert_eq!(info["capabilities"]["account_session"], true);
    assert_eq!(info["capabilities"]["external_tools"], true);
    assert_eq!(info["capabilities"]["worker_runtime"], true);
    assert_eq!(info["account"]["schema_version"], 1);
    assert_eq!(info["account"]["state"], "signed_out");
    assert_eq!(info["account"]["api_base"], "https://api.codewhale.net");
    assert_eq!(info["account"]["scopes"], json!([]));
    assert!(info["account"].get("access_token").is_none());
    assert!(info["account"].get("refresh_token").is_none());
    assert!(info["account"].get("email").is_none());
    assert!(info["experimental"].is_object());

    handle.abort();
    Ok(())
}

#[test]
fn unauthenticated_runtime_info_redacts_secure_account_identity_without_loading_it() {
    use codewhale_secrets::account::{AccountSessionState, RuntimeAccountInfo};

    let loaded = std::cell::Cell::new(false);
    let info = runtime_account_info_for_request(false, "https://api.codewhale.net", || {
        loaded.set(true);
        RuntimeAccountInfo {
            schema_version: 1,
            state: AccountSessionState::Authenticated,
            api_base: "https://api.codewhale.net".to_string(),
            account_id: Some("acct-private".to_string()),
            session_id: Some("session-private".to_string()),
            scopes: vec!["identity:read".to_string()],
            expires_at: Some("2030-01-01T00:00:00Z".to_string()),
        }
    });
    assert!(
        !loaded.get(),
        "unauthenticated probes must not read secure storage"
    );
    assert_eq!(info.state, AccountSessionState::SignedOut);
    let json = serde_json::to_string(&info).unwrap();
    for private in ["acct-private", "session-private", "identity:read"] {
        assert!(!json.contains(private));
    }
}

#[test]
fn runtime_account_api_origin_rejects_credentials_paths_and_non_loopback_http() {
    assert_eq!(
        normalize_runtime_account_api_base("https://api.codewhale.net/"),
        Some("https://api.codewhale.net".to_string())
    );
    assert_eq!(
        normalize_runtime_account_api_base("http://127.0.0.1:8787"),
        Some("http://127.0.0.1:8787".to_string())
    );
    for invalid in [
        "https://user:secret@example.test",
        "https://example.test/account",
        "https://example.test?token=secret",
        "http://api.codewhale.net",
    ] {
        assert_eq!(
            normalize_runtime_account_api_base(invalid),
            None,
            "{invalid}"
        );
    }
}

#[tokio::test]
async fn create_thread_accepts_dynamic_tools_and_environments() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let created: serde_json::Value = client
        .post(format!("http://{addr}/v1/threads"))
        .json(&json!({
            "model": "test-model",
            "dynamic_tools": [
                {
                    "namespace": "tau_bench",
                    "name": "get_reservation",
                    "description": "Look up a reservation.",
                    "input_schema": { "type": "object" }
                }
            ],
            "environments": [
                { "environment_id": "local", "cwd": "/workspace" }
            ]
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert!(created["id"].is_string());

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn create_thread_normalizes_and_persists_named_permission_posture() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let created: serde_json::Value = client
        .post(format!("http://{addr}/v1/threads"))
        .json(&json!({
            "model": "test-model",
            "mode": "operate",
            "permission_posture": "auto-review"
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(created["mode"], "operate");
    assert_eq!(created["permission_posture"], "auto_review");
    assert_eq!(created["auto_approve"], false);
    assert_eq!(created["trust_mode"], false);

    let authoritative_ask: serde_json::Value = client
        .post(format!("http://{addr}/v1/threads"))
        .json(&json!({
            "model": "test-model",
            "mode": "yolo",
            "permission_posture": "ask",
            "auto_approve": true
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(authoritative_ask["mode"], "agent");
    assert_eq!(authoritative_ask["permission_posture"], "ask");
    assert_eq!(authoritative_ask["auto_approve"], false);

    let invalid = client
        .post(format!("http://{addr}/v1/threads"))
        .json(&json!({
            "model": "test-model",
            "permission_posture": "owner"
        }))
        .send()
        .await?;
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn start_turn_accepts_dynamic_tools_and_environment_id() -> Result<()> {
    Box::pin(async {
        let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
            return Ok(());
        };
        let client = crate::tls::reqwest_client();

        let created: serde_json::Value = client
            .post(format!("http://{addr}/v1/threads"))
            .json(&json!({ "model": "test-model" }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let thread_id = created["id"].as_str().context("missing thread id")?;

        let started: serde_json::Value = client
            .post(format!("http://{addr}/v1/threads/{thread_id}/turns"))
            .json(&json!({
                "prompt": "hello",
                "dynamic_tools": [
                    {
                        "name": "simple_tool",
                        "description": "A simple tool.",
                        "input_schema": { "type": "object" }
                    }
                ],
                "environment_id": "local",
                "permission_posture": "auto-review"
            }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_eq!(started["turn"]["thread_id"], thread_id);
        assert_eq!(started["thread"]["permission_posture"], "ask");
        assert_eq!(started["turn"]["permission_posture"], "auto_review");

        let stored: serde_json::Value = client
            .get(format!("http://{addr}/v1/threads/{thread_id}"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_eq!(stored["turns"][0]["permission_posture"], "auto_review");

        handle.abort();
        Ok(())
    })
    .await
}

/// A turn receipts the mode it ran in, not the thread's mode: a client reading
/// the thread when the turn finishes would otherwise learn how the thread is
/// set up *now*, which is a different question as soon as the mode is switched
/// mid-run.
#[tokio::test]
async fn turn_record_reports_the_mode_it_ran_in() -> Result<()> {
    Box::pin(async {
        let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
            return Ok(());
        };
        let client = crate::tls::reqwest_client();

        let created: serde_json::Value = client
            .post(format!("http://{addr}/v1/threads"))
            .json(&json!({ "model": "test-model" }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let thread_id = created["id"].as_str().context("missing thread id")?;

        let started: serde_json::Value = client
            .post(format!("http://{addr}/v1/threads/{thread_id}/turns"))
            .json(&json!({ "prompt": "plan it", "mode": "plan" }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_eq!(started["turn"]["mode"], "plan");

        let stored: serde_json::Value = client
            .get(format!("http://{addr}/v1/threads/{thread_id}"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_eq!(stored["turns"][0]["mode"], "plan");

        handle.abort();
        Ok(())
    })
    .await
}

#[tokio::test]
async fn mobile_runtime_router_starts_without_duplicate_method_routes() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().to_path_buf();
    let sessions_dir = root.join("sessions");
    let Some((_addr, _runtime_threads, handle)) =
        spawn_test_server_with_root_token_and_mobile(root, sessions_dir, None, true).await?
    else {
        return Ok(());
    };

    // Axum panics during `build_router` when a method/path pair is registered
    // twice, so reaching the running server is the regression assertion.
    handle.abort();
    Ok(())
}

#[test]
fn mobile_stream_statuses_use_selected_locale() -> Result<()> {
    use codewhale_localization::{Locale, MessageId, tr};

    for &locale in Locale::shipped_complete() {
        let html = mobile_html(locale);
        let script = html
            .split_once("<script>")
            .context("mobile script")?
            .1
            .split_once("boot().catch")
            .context("mobile startup")?
            .0;
        let runtime = rquickjs::Runtime::new()?;
        let context = rquickjs::Context::full(&runtime)?;
        context.with(|ctx| -> Result<()> {
            ctx.eval::<(), _>(script)?;
            for (expression, message) in [
                (
                    "streamEndStatus('replay_failed')",
                    MessageId::MobileStreamReplayFailed,
                ),
                (
                    "streamEndStatus('catch_up_failed')",
                    MessageId::MobileStreamCatchUpFailed,
                ),
                (
                    "streamEndStatus('runtime_shutdown')",
                    MessageId::MobileStreamRuntimeShutdown,
                ),
                (
                    "streamEndStatus('future_reason')",
                    MessageId::MobileStreamEnded,
                ),
                ("streamMessages.closed", MessageId::MobileStreamClosed),
                (
                    "streamMessages.reconnecting",
                    MessageId::MobileStreamReconnecting,
                ),
                ("streamMessages.connected", MessageId::MobileStreamConnected),
            ] {
                let actual: String = ctx.eval(expression)?;
                assert_eq!(actual, tr(locale, message), "{}: {message:?}", locale.tag());
                if locale != Locale::En {
                    assert_ne!(
                        actual,
                        tr(Locale::En, message),
                        "{}: {message:?}",
                        locale.tag()
                    );
                }
            }
            Ok(())
        })?;
    }
    Ok(())
}

#[tokio::test]
async fn mobile_page_is_available_only_when_enabled() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().to_path_buf();
    let sessions_dir = root.join("sessions");
    let Some((addr, runtime_threads, handle)) = spawn_test_server_with_root_token_and_mobile(
        root.join("disabled"),
        sessions_dir.clone(),
        None,
        false,
    )
    .await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let disabled = client.get(format!("http://{addr}/mobile")).send().await?;
    assert_eq!(disabled.status(), StatusCode::NOT_FOUND);
    handle.abort();
    let _ = handle.await;
    drop(runtime_threads);

    let Some((addr, _runtime_threads, handle)) = spawn_test_server_with_root_token_and_mobile(
        root.join("enabled"),
        sessions_dir,
        None,
        true,
    )
    .await?
    else {
        return Ok(());
    };
    let enabled = client
        .get(format!("http://{addr}/mobile"))
        .send()
        .await?
        .error_for_status()?;
    let html = enabled.text().await?;
    assert!(html.contains("Codewhale Mobile"));
    assert!(html.contains("/v1/approvals/"));
    assert!(html.contains("MAX_VISIBLE_EVENTS = 100"));
    assert!(html.contains("replay_limit:"));
    assert!(html.contains("X-Codewhale-Mobile-Request"));
    assert!(!html.contains("localStorage.getItem"));
    assert!(!html.contains("localStorage.setItem"));
    assert!(!html.contains("setRuntimeTokenCookie"));

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn mobile_page_serves_shell_when_auth_enabled() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().to_path_buf();
    let sessions_dir = root.join("sessions");
    let token = "abc ABC+/?:=&%".to_string();
    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root_token_and_mobile(root, sessions_dir, Some(token.clone()), true)
            .await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let shell = client
        .get(format!("http://{addr}/mobile"))
        .send()
        .await?
        .error_for_status()?;
    let html = shell.text().await?;
    assert!(html.contains("Codewhale Mobile"));
    assert!(html.contains("codewhale_mobile_session_proof"));
    assert!(html.contains("/__codewhale/mobile/session"));
    assert!(!html.contains("codewhale_runtime_token="));
    assert!(!html.contains("localStorage.setItem"));

    let bearer = client
        .get(format!("http://{addr}/mobile"))
        .bearer_auth(&token)
        .send()
        .await?
        .error_for_status()?;
    assert!(bearer.text().await?.contains("Codewhale Mobile"));

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn mobile_bootstrap_uses_opaque_origin_bound_session_and_single_use_stream_ticket()
-> Result<()> {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().to_path_buf();
    let sessions_dir = root.join("sessions");
    let workspace = root.join("workspace");
    let token = "mobile-runtime-test-token".to_string();
    let (mobile_state, nonce) = mobile::RuntimeMobileState::new();
    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root_token_mobile_workspace_and_overrides(
            root,
            sessions_dir,
            Some(token.clone()),
            true,
            workspace,
            TestServerOverrides {
                mobile: Some(mobile_state),
                ..TestServerOverrides::default()
            },
        )
        .await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client_builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let origin = format!("http://{addr}");

    let shell = client.get(format!("{origin}/mobile")).send().await?;
    assert_eq!(shell.status(), StatusCode::OK);
    assert_eq!(
        shell
            .headers()
            .get(header::REFERRER_POLICY)
            .and_then(|value| value.to_str().ok()),
        Some("no-referrer")
    );
    let shell = shell.text().await?;
    assert!(!shell.contains(&token));
    assert!(!shell.contains("localStorage.setItem"));
    assert!(!shell.contains("localStorage.getItem"));
    assert!(!shell.contains("codewhale_runtime_token="));

    let unauthenticated_session = client
        .post(format!("{origin}/__codewhale/mobile/session"))
        .send()
        .await?;
    assert_eq!(unauthenticated_session.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        unauthenticated_session
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("no-store")
    );

    let manual_session = client
        .post(format!("{origin}/__codewhale/mobile/session"))
        .bearer_auth(&token)
        .send()
        .await?;
    assert_eq!(manual_session.status(), StatusCode::OK);
    let manual_cookie = manual_session
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .context("manual mobile session Set-Cookie")?
        .to_string();
    assert!(manual_cookie.starts_with("codewhale_mobile_session=cwms_"));
    assert!(manual_cookie.ends_with("; Max-Age=1800; HttpOnly; SameSite=Strict; Path=/"));
    assert!(!manual_cookie.contains(&token));
    let manual_body = manual_session.text().await?;
    assert!(!manual_body.contains(&token));

    let bootstrap = client
        .get(format!("{origin}/__codewhale/mobile/bootstrap/{nonce}"))
        .send()
        .await?;
    assert_eq!(bootstrap.status(), StatusCode::SEE_OTHER);
    let bootstrap_cookie = bootstrap
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .context("bootstrap mobile session Set-Cookie")?
        .to_string();
    assert!(bootstrap_cookie.starts_with("codewhale_mobile_session=cwms_"));
    assert!(!bootstrap_cookie.contains(&token));
    let location = bootstrap
        .headers()
        .get(header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .context("bootstrap mobile redirect")?;
    assert!(location.starts_with("/mobile#request_proof=cwmr_"));
    assert!(location.contains("&stream_ticket=cwmt_"));
    assert!(!location.contains(&token));

    let bootstrap_cookie_pair = bootstrap_cookie
        .split(';')
        .next()
        .context("bootstrap mobile cookie pair")?
        .to_string();
    let fragment = location
        .strip_prefix("/mobile#")
        .context("bootstrap redirect must use a fragment")?;
    let request_proof = fragment
        .split('&')
        .find_map(|part| part.strip_prefix("request_proof="))
        .context("bootstrap request proof")?
        .to_string();
    let stream_ticket = fragment
        .split('&')
        .find_map(|part| part.strip_prefix("stream_ticket="))
        .context("bootstrap stream ticket")?
        .to_string();

    let cookie_only = client
        .get(format!("{origin}/v1/threads/summary"))
        .header(header::COOKIE, &bootstrap_cookie_pair)
        .send()
        .await?;
    assert_eq!(cookie_only.status(), StatusCode::UNAUTHORIZED);

    let sibling_port_replay = client
        .get(format!("{origin}/v1/threads/summary"))
        .header(header::COOKIE, &bootstrap_cookie_pair)
        .header(mobile::MOBILE_REQUEST_HEADER, &request_proof)
        .header(header::ORIGIN, "http://127.0.0.1:3000")
        .header("sec-fetch-site", "same-site")
        .send()
        .await?;
    assert_eq!(sibling_port_replay.status(), StatusCode::UNAUTHORIZED);

    let same_origin = client
        .get(format!("{origin}/v1/threads/summary"))
        .header(header::COOKIE, &bootstrap_cookie_pair)
        .header(mobile::MOBILE_REQUEST_HEADER, &request_proof)
        .header(header::ORIGIN, &origin)
        .header("sec-fetch-site", "same-origin")
        .send()
        .await?;
    assert_eq!(same_origin.status(), StatusCode::OK);

    let created: serde_json::Value = client
        .post(format!("{origin}/v1/threads"))
        .header(header::COOKIE, &bootstrap_cookie_pair)
        .header(mobile::MOBILE_REQUEST_HEADER, &request_proof)
        .header(header::ORIGIN, &origin)
        .header("sec-fetch-site", "same-origin")
        .json(&json!({}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let thread_id = created["id"].as_str().context("mobile-created thread id")?;

    let stream_url =
        format!("{origin}/v1/threads/{thread_id}/events?mobile_stream_ticket={stream_ticket}");
    let stream = client
        .get(&stream_url)
        .header(header::COOKIE, &bootstrap_cookie_pair)
        .header(header::ORIGIN, &origin)
        .header("sec-fetch-site", "same-origin")
        .send()
        .await?;
    assert_eq!(stream.status(), StatusCode::OK);
    drop(stream);

    let replayed_stream = client
        .get(&stream_url)
        .header(header::COOKIE, &bootstrap_cookie_pair)
        .header(header::ORIGIN, &origin)
        .header("sec-fetch-site", "same-origin")
        .send()
        .await?;
    assert_eq!(replayed_stream.status(), StatusCode::UNAUTHORIZED);

    let refreshed: serde_json::Value = client
        .post(format!("{origin}/__codewhale/mobile/stream-ticket"))
        .header(header::COOKIE, &bootstrap_cookie_pair)
        .header(mobile::MOBILE_REQUEST_HEADER, &request_proof)
        .header(header::ORIGIN, &origin)
        .header("sec-fetch-site", "same-origin")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let refreshed_ticket = refreshed["stream_ticket"]
        .as_str()
        .context("refreshed stream ticket")?;
    let refreshed_stream_url =
        format!("{origin}/v1/threads/{thread_id}/events?mobile_stream_ticket={refreshed_ticket}");
    let sibling_stream_replay = client
        .get(&refreshed_stream_url)
        .header(header::COOKIE, &bootstrap_cookie_pair)
        .header(header::ORIGIN, "http://127.0.0.1:3000")
        .header("sec-fetch-site", "same-site")
        .send()
        .await?;
    assert_eq!(sibling_stream_replay.status(), StatusCode::UNAUTHORIZED);

    let same_origin_stream = client
        .get(&refreshed_stream_url)
        .header(header::COOKIE, &bootstrap_cookie_pair)
        .header(header::ORIGIN, &origin)
        .header("sec-fetch-site", "same-origin")
        .send()
        .await?;
    assert_eq!(same_origin_stream.status(), StatusCode::OK);
    drop(same_origin_stream);

    let reused_bootstrap = client
        .get(format!("{origin}/__codewhale/mobile/bootstrap/{nonce}"))
        .send()
        .await?;
    assert_eq!(reused_bootstrap.status(), StatusCode::UNAUTHORIZED);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn mobile_insecure_mode_allows_page_and_v1_routes_without_token() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().to_path_buf();
    let sessions_dir = root.join("sessions");
    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root_token_and_mobile(root, sessions_dir, None, true).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let page = client
        .get(format!("http://{addr}/mobile"))
        .send()
        .await?
        .error_for_status()?;
    assert!(page.text().await?.contains("Codewhale Mobile"));

    let summary = client
        .get(format!("http://{addr}/v1/threads/summary"))
        .send()
        .await?
        .error_for_status()?;
    assert_eq!(summary.status(), StatusCode::OK);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn thread_summary_projects_typed_pending_attention_count() -> Result<()> {
    let Some((addr, runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let attention_thread: serde_json::Value = client
        .post(format!("http://{addr}/v1/threads"))
        .json(&json!({}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let attention_id = attention_thread["id"]
        .as_str()
        .context("missing attention thread id")?;
    let _approval_rx = runtime_threads
        .register_pending_approval_for_thread_for_test(attention_id, "approval-summary-attention");
    runtime_threads
        .register_pending_user_input_for_thread_for_test(attention_id, "input-summary-attention");

    let recent_thread: serde_json::Value = client
        .post(format!("http://{addr}/v1/threads"))
        .json(&json!({}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let recent_id = recent_thread["id"]
        .as_str()
        .context("missing recent thread id")?;

    let summaries: serde_json::Value = client
        .get(format!("http://{addr}/v1/threads/summary?limit=100"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let rows = summaries.as_array().context("summary should be an array")?;
    let attention = rows
        .iter()
        .find(|row| row["id"] == attention_id)
        .context("attention thread summary")?;
    let recent = rows
        .iter()
        .find(|row| row["id"] == recent_id)
        .context("recent thread summary")?;
    assert_eq!(attention["pending_attention_count"], 2);
    assert_eq!(recent["pending_attention_count"], 0);

    handle.abort();
    Ok(())
}

/// `GET /v1/threads/summary?search=` bounded the *store read* by `limit`
/// before matching, so a thread older than the newest `limit` rows could not
/// be found by searching for it — the embedded dashboard's search box asks
/// for `limit=100`, which silently made every older thread unsearchable.
/// `limit` bounds the returned rows, not how far the search looks.
#[tokio::test]
async fn thread_summary_search_finds_a_match_older_than_the_row_limit() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    // `POST /v1/threads` has no title field; the title is a PATCH. Patching
    // also re-stamps `updated_at`, so ordering is not left to two creates
    // landing inside the same clock tick.
    let create = |title: &str| {
        let client = client.clone();
        let title = title.to_string();
        async move {
            let thread: serde_json::Value = client
                .post(format!("http://{addr}/v1/threads"))
                .json(&json!({}))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            let id = thread["id"]
                .as_str()
                .context("missing thread id")?
                .to_string();
            client
                .patch(format!("http://{addr}/v1/threads/{id}"))
                .json(&json!({ "title": title }))
                .send()
                .await?
                .error_for_status()?;
            anyhow::Ok(id)
        }
    };

    // Oldest thread carries the needle; three newer threads crowd it out of
    // any `limit`-sized window.
    let needle_id = create("zebracrossing handoff").await?;
    for title in ["decoy one", "decoy two", "decoy three"] {
        create(title).await?;
    }

    let summaries: serde_json::Value = client
        .get(format!(
            "http://{addr}/v1/threads/summary?limit=2&search=zebracrossing"
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let rows = summaries.as_array().context("summary should be an array")?;
    assert_eq!(
        rows.len(),
        1,
        "search must reach past the row limit; got {summaries}"
    );
    assert_eq!(rows[0]["id"], needle_id);

    // `limit` still bounds the returned rows.
    let bounded: serde_json::Value = client
        .get(format!("http://{addr}/v1/threads/summary?limit=2"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        bounded
            .as_array()
            .context("summary should be an array")?
            .len(),
        2
    );

    handle.abort();
    Ok(())
}

/// `GET /v1/threads/summary?search=` used to call `get_thread_detail` for every
/// thread *before* matching. Detail walks the entire turns directory and the
/// entire items directory, so a non-matching dashboard keystroke was
/// O(threads × (all_turns + all_items)) JSON reads. Matching on the thread
/// record first (and peeking one latest-turn file only when the title is
/// unset) must not scale that way: adding items must not multiply reads by
/// thread count.
#[tokio::test]
async fn thread_summary_search_does_not_scan_the_whole_store_per_thread() -> Result<()> {
    let Some((addr, runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    // Settle the server's startup item-index warm-up, so the read counts below
    // measure this route and not a background pass over the directory.
    runtime_threads.warm_item_index()?;
    let client = crate::tls::reqwest_client();

    const THREADS: usize = 8;
    const TURNS_PER_THREAD: usize = 4;
    const ITEMS_PER_TURN: usize = 4;
    const PREVIEW_TOKEN: &str = "previewonlyneedlenowhereontherecord";
    const TITLE_NEEDLE: &str = "zebracrossing-summary-search";

    let mut thread_ids = Vec::with_capacity(THREADS);
    for index in 0..THREADS {
        let thread: serde_json::Value = client
            .post(format!("http://{addr}/v1/threads"))
            .json(&json!({}))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let id = thread["id"]
            .as_str()
            .context("missing thread id")?
            .to_string();
        let title = if index == 0 {
            TITLE_NEEDLE
        } else {
            "decoy summary search title"
        };
        client
            .patch(format!("http://{addr}/v1/threads/{id}"))
            .json(&json!({ "title": title }))
            .send()
            .await?
            .error_for_status()?;
        seed_summary_search_transcript(
            runtime_threads.test_store(),
            &id,
            index,
            TURNS_PER_THREAD,
            ITEMS_PER_TURN,
            PREVIEW_TOKEN,
        )?;
        thread_ids.push(id);
    }

    let total_turns = (THREADS * TURNS_PER_THREAD) as u64;
    let total_items = (THREADS * TURNS_PER_THREAD * ITEMS_PER_TURN) as u64;
    let quadratic_file_reads = (THREADS as u64) * (total_turns + total_items);

    runtime_threads.reset_whole_store_scan_file_reads();
    let missed: serde_json::Value = client
        .get(format!(
            "http://{addr}/v1/threads/summary?limit=2&search={PREVIEW_TOKEN}"
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let (turn_files, item_files) = runtime_threads.whole_store_scan_file_reads();
    let scan_reads = turn_files + item_files;
    assert!(
        scan_reads.saturating_mul(2) < quadratic_file_reads,
        "non-matching search read {turn_files} turn files and {item_files} item files \
         across {THREADS} threads ({total_turns} turns, {total_items} items); \
         a per-thread get_thread_detail would have been ~{quadratic_file_reads} \
         whole-store file reads"
    );
    assert!(
        missed
            .as_array()
            .context("summary should be an array")?
            .is_empty(),
        "preview text is display-only and must not be a search key; got {missed}"
    );

    runtime_threads.reset_whole_store_scan_file_reads();
    let found: serde_json::Value = client
        .get(format!(
            "http://{addr}/v1/threads/summary?limit=2&search={TITLE_NEEDLE}"
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let rows = found.as_array().context("summary should be an array")?;
    assert_eq!(
        rows.len(),
        1,
        "title search must still find the needle; got {found}"
    );
    assert_eq!(rows[0]["id"], thread_ids[0]);
    assert!(
        rows[0]["preview"]
            .as_str()
            .is_some_and(|preview| preview.contains(PREVIEW_TOKEN)),
        "matching rows still load detail so the preview is filled; got {found}"
    );

    handle.abort();
    Ok(())
}

/// The default listing carries no `search`, and it must not walk the items
/// directory at all.
///
/// This route used to call `get_thread_detail` for every row, and detail is a
/// whole-store walk: `list_turns_for_thread` scans every turn record and
/// `list_items_for_turns_map` scans every item record, because an item's
/// filename carries only the item id. Listing `T` threads therefore cost
/// `T x (all_turns + all_items)` reads — seconds-per-thread, and the reason a
/// 72-thread rail went blank. The next fix made that one pass per directory,
/// and the one after it read each row's preview out of the row's own newest
/// turn, so no item file is read by directory walk at all. The bound asserted
/// here is that zero, so any return to a store-wide item read fails loudly.
#[tokio::test]
async fn thread_summary_listing_never_walks_the_items_directory() -> Result<()> {
    let Some((addr, runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    // Settle the server's startup item-index warm-up, so the read counts below
    // measure this route and not a background pass over the directory.
    runtime_threads.warm_item_index()?;
    let client = crate::tls::reqwest_client();

    const THREADS: usize = 8;
    const TURNS_PER_THREAD: usize = 4;
    const ITEMS_PER_TURN: usize = 4;
    const PREVIEW_TOKEN: &str = "listingreadsstoreonce";

    for index in 0..THREADS {
        let thread: serde_json::Value = client
            .post(format!("http://{addr}/v1/threads"))
            .json(&json!({}))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let id = thread["id"]
            .as_str()
            .context("missing thread id")?
            .to_string();
        seed_summary_search_transcript(
            runtime_threads.test_store(),
            &id,
            index,
            TURNS_PER_THREAD,
            ITEMS_PER_TURN,
            PREVIEW_TOKEN,
        )?;
    }

    let total_turns = (THREADS * TURNS_PER_THREAD) as u64;
    let total_items = (THREADS * TURNS_PER_THREAD * ITEMS_PER_TURN) as u64;
    let per_thread_file_reads = THREADS as u64 * (total_turns + total_items);

    runtime_threads.reset_whole_store_scan_file_reads();
    let listed: serde_json::Value = client
        .get(format!("http://{addr}/v1/threads/summary?limit={THREADS}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let (turn_files, item_files) = runtime_threads.whole_store_scan_file_reads();
    let rows = listed.as_array().context("summary should be an array")?;

    assert_eq!(
        rows.len(),
        THREADS,
        "every thread must still be listed; got {listed}"
    );
    assert_eq!(
        turn_files, total_turns,
        "the page still reads every turn record exactly once: read {turn_files} of \
         {total_turns} turn files, while reading one thread's detail per row would have \
         been {per_thread_file_reads} reads in total"
    );
    assert_eq!(
        item_files, 0,
        "a page must not read the items directory at all: read {item_files} item files, \
         while one pass over the directory would have been {total_items} and one detail \
         read per row {per_thread_file_reads}"
    );
    assert!(
        rows.iter().all(|row| row["preview"]
            .as_str()
            .is_some_and(|preview| preview.contains(PREVIEW_TOKEN))),
        "reading each row's own turn must still fill every preview; got {listed}"
    );
    assert!(
        rows.iter()
            .all(|row| row["latest_turn_status"] == "completed"),
        "one pass must still report each thread's newest turn status; got {listed}"
    );
    assert!(
        rows.iter().all(|row| row["pending_attention_count"] == 0),
        "one pass must still read attention from live state; got {listed}"
    );

    handle.abort();
    Ok(())
}

/// The summary resolves git metadata once per distinct workspace, so the
/// resolve list must collapse repeated workspaces instead of keeping one entry
/// per row: every entry costs up to five blocking `git` processes.
#[test]
fn thread_summary_resolves_git_metadata_once_per_distinct_workspace() {
    let repo = PathBuf::from("/srv/repo");
    let other = PathBuf::from("/srv/other");
    let rows = vec![
        repo.clone(),
        repo.clone(),
        other.clone(),
        repo.clone(),
        other.clone(),
    ];
    assert_eq!(distinct_paths(rows), vec![repo, other]);
}

fn seed_summary_search_transcript(
    store: &crate::runtime_threads::RuntimeThreadStore,
    thread_id: &str,
    thread_index: usize,
    turns: usize,
    items_per_turn: usize,
    preview_token: &str,
) -> Result<()> {
    let mut thread = store.load_thread(thread_id)?;
    let base = Utc::now();
    let mut latest_turn_id = None;
    for turn_offset in 0..turns {
        let created_at = base + chrono::Duration::milliseconds(turn_offset as i64);
        let turn_id = format!("turn_sum_{thread_index}_{turn_offset}");
        let mut item_ids = Vec::with_capacity(items_per_turn);
        for item_offset in 0..items_per_turn {
            let item_id = format!("item_sum_{thread_index}_{turn_offset}_{item_offset}");
            let kind = if item_offset == 0 {
                TurnItemKind::UserMessage
            } else {
                TurnItemKind::AgentMessage
            };
            let text = format!("{preview_token} {thread_index} {turn_offset} {item_offset}");
            store.save_item(&crate::runtime_threads::TurnItemRecord {
                schema_version: 2,
                id: item_id.clone(),
                turn_id: turn_id.clone(),
                kind,
                status: TurnItemLifecycleStatus::Completed,
                summary: text.clone(),
                detail: Some(text),
                metadata: None,
                artifact_refs: Vec::new(),
                artifacts: Vec::new(),
                started_at: Some(created_at),
                ended_at: Some(created_at),
            })?;
            item_ids.push(item_id);
        }
        store.save_turn(&TurnRecord {
            decision_receipts: Vec::new(),
            max_output_tokens: None,
            schema_version: 2,
            id: turn_id.clone(),
            thread_id: thread_id.to_string(),
            status: RuntimeTurnStatus::Completed,
            input_summary: format!("decoy prompt {thread_index} {turn_offset}"),
            created_at,
            started_at: Some(created_at),
            ended_at: Some(created_at),
            duration_ms: Some(0),
            usage: None,
            model_request_diagnostics: None,
            operation_activity: None,
            routing_settlement: false,
            effective_route_usage: None,
            permission_posture: None,
            mode: None,
            effective_provider: None,
            effective_provider_id: None,
            effective_openrouter_vendor: None,
            effective_billing_surface: None,
            effective_endpoint_fingerprint: None,
            effective_provider_live_pricing: None,
            effective_billing_mode: None,
            effective_dispatched_at: None,
            effective_model: None,
            routed_usage: Vec::new(),
            routed_usage_drop_records: Vec::new(),
            routed_usage_source_ids: Vec::new(),
            routed_usage_dropped_records: 0,
            error: None,
            item_ids,
            steer_count: 0,
            agent_mail_message_id: None,
            artifacts: Vec::new(),
            workspace: None,
            workspace_snapshots: Vec::new(),
        })?;
        latest_turn_id = Some(turn_id);
    }
    thread.latest_turn_id = latest_turn_id;
    store.save_thread(&thread)?;
    Ok(())
}

#[tokio::test]
async fn decide_approval_404s_when_nothing_pending() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let resp = client
        .post(format!("http://{addr}/v1/approvals/no_such_id"))
        .json(&json!({ "decision": "allow" }))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn submit_user_input_404s_without_entering_engine_mailbox_for_unknown_id() -> Result<()> {
    let Some((addr, runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let thread = runtime_threads
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut harness = crate::core::engine::mock_engine_handle();
    runtime_threads
        .install_test_engine(&thread.id, harness.handle.clone())
        .await?;

    let response = crate::tls::reqwest_client()
        .post(format!(
            "http://{addr}/v1/user-input/{}/input-missing",
            thread.id
        ))
        .json(&json!({
            "answers": [{
                "id": "choice",
                "label": "Missing",
                "value": "must-not-enter-engine-mailbox",
            }],
        }))
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(
        tokio::time::timeout(
            Duration::from_millis(25),
            harness.recv_user_input_submission()
        )
        .await
        .is_err(),
        "unknown user input reached the engine mailbox"
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn events_endpoint_rejects_unbounded_tail_requests() -> Result<()> {
    let Some((addr, runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let thread = runtime_threads
        .create_thread(CreateThreadRequest::default())
        .await?;
    let response = crate::tls::reqwest_client()
        .get(format!(
            "http://{addr}/v1/threads/{}/events?replay_limit={}",
            thread.id,
            MAX_RUNTIME_EVENT_REPLAY_TAIL.saturating_add(1),
        ))
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn decide_approval_400s_on_bad_decision() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let resp = client
        .post(format!("http://{addr}/v1/approvals/whatever"))
        .json(&json!({ "decision": "yolo" }))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn decide_approval_delivers_to_runtime() -> Result<()> {
    let Some((addr, runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    // The caller never chooses the approval ID; the runtime mints it and the
    // client echoes it back verbatim.
    let (approval_id, rx) = runtime_threads.register_pending_approval_for_test("ext_id");
    assert_ne!(approval_id, "ext_id");

    // A provider-shaped raw ID is not accepted as a fallback for the real one.
    let raw = client
        .post(format!("http://{addr}/v1/approvals/ext_id"))
        .json(&json!({ "decision": "allow", "remember": false }))
        .send()
        .await?;
    assert_eq!(raw.status(), StatusCode::NOT_FOUND);

    let resp = client
        .post(format!("http://{addr}/v1/approvals/{approval_id}"))
        .json(&json!({ "decision": "allow", "remember": false }))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = resp.json().await?;
    assert_eq!(body["ok"], true);
    assert_eq!(body["approval_id"], approval_id);
    assert_eq!(body["decision"], "allow");
    assert_eq!(body["delivered"], true);

    let received = tokio::time::timeout(ci_scaled(Duration::from_secs(1)), rx).await??;
    assert_eq!(
        received,
        ExternalApprovalDecision::Allow { remember: false }
    );

    // The capability is single-use: a duplicate response (retry, double-click,
    // replayed request) cannot deliver a second decision.
    let replay = client
        .post(format!("http://{addr}/v1/approvals/{approval_id}"))
        .json(&json!({ "decision": "deny", "remember": true }))
        .send()
        .await?;
    assert_eq!(replay.status(), StatusCode::NOT_FOUND);

    handle.abort();
    Ok(())
}

/// The snapshot a reconnecting client resumes from must carry both halves of
/// the approval identity over the wire: the opaque capability it posts back,
/// and the raw tool-call correlator it matches against the tool row. Only the
/// first is accepted by `POST /v1/approvals/{id}`.
#[tokio::test]
async fn thread_snapshot_separates_approval_capability_from_tool_correlator() -> Result<()> {
    let Some((addr, runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let thread: serde_json::Value = client
        .post(format!("http://{addr}/v1/threads"))
        .json(&json!({}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let thread_id = thread["id"].as_str().context("missing thread id")?;

    let (approval_id, rx) =
        runtime_threads.register_pending_approval_for_thread_for_test(thread_id, "call_1");

    let detail: serde_json::Value = client
        .get(format!("http://{addr}/v1/threads/{thread_id}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let pending = detail["pending_approvals"]
        .as_array()
        .context("missing pending_approvals")?;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["id"], approval_id);
    assert_eq!(pending[0]["tool_call_id"], "call_1");

    // A provider call ID is a correlator, not a credential — even now that a
    // client can read it straight out of the snapshot.
    let by_correlator = client
        .post(format!("http://{addr}/v1/approvals/call_1"))
        .json(&json!({ "decision": "allow", "remember": false }))
        .send()
        .await?;
    assert_eq!(by_correlator.status(), StatusCode::NOT_FOUND);

    let by_capability = client
        .post(format!("http://{addr}/v1/approvals/{approval_id}"))
        .json(&json!({ "decision": "deny", "remember": false }))
        .send()
        .await?;
    assert_eq!(by_capability.status(), StatusCode::OK);
    let received = tokio::time::timeout(ci_scaled(Duration::from_secs(1)), rx).await??;
    assert_eq!(received, ExternalApprovalDecision::Deny { remember: false });

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn dynamic_tool_result_endpoint_delivers_to_runtime() -> Result<()> {
    let Some((addr, runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let thread: serde_json::Value = client
        .post(format!("http://{addr}/v1/threads"))
        .json(&json!({}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let thread_id = thread["id"].as_str().context("thread id")?;
    let rx =
        runtime_threads.register_pending_dynamic_tool_for_test(thread_id, "turn_1", "call_1")?;

    let wrong_turn = client
        .post(format!(
            "http://{addr}/v1/threads/{thread_id}/turns/turn_wrong/tool-calls/call_1/result"
        ))
        .json(&json!({ "success": false }))
        .send()
        .await?;
    assert_eq!(wrong_turn.status(), StatusCode::NOT_FOUND);

    let resp = client
        .post(format!(
            "http://{addr}/v1/threads/{thread_id}/turns/turn_1/tool-calls/call_1/result"
        ))
        .json(&json!({
            "success": true,
            "content": [{ "type": "input_text", "text": "ok" }]
        }))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::ACCEPTED);

    let received = tokio::time::timeout(ci_scaled(Duration::from_secs(1)), rx).await??;
    assert!(received.success);
    assert_eq!(received.content.len(), 1);
    let resolved = runtime_threads
        .events_since(thread_id, None)?
        .into_iter()
        .filter(|event| event.event == "tool_call.resolved")
        .collect::<Vec<_>>();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].payload["call_id"], "call_1");
    assert!(resolved[0].payload.get("content").is_none());

    let duplicate = client
        .post(format!(
            "http://{addr}/v1/threads/{thread_id}/turns/turn_1/tool-calls/call_1/result"
        ))
        .json(&json!({ "success": true }))
        .send()
        .await?;
    assert_eq!(duplicate.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        runtime_threads
            .events_since(thread_id, None)?
            .iter()
            .filter(|event| event.event == "tool_call.resolved")
            .count(),
        1
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn skills_endpoint_includes_enabled_field() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let body: serde_json::Value = client
        .get(format!("http://{addr}/v1/skills"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if let Some(skills) = body["skills"].as_array() {
        for skill in skills {
            assert!(skill.get("enabled").is_some());
        }
    }

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn unicode_skill_activation_matches_api_load_and_owned_lifecycle() -> Result<()> {
    use crate::tools::spec::{ToolContext, ToolSpec as _};

    let _env = lock_test_env();
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("runtime");
    let workspace = tmp.path().join("workspace");
    let home = tmp.path().join("home");
    fs::create_dir_all(&root)?;
    fs::create_dir_all(&home)?;
    let _home = EnvVarGuard::set("HOME", &home);
    let _userprofile = EnvVarGuard::set("USERPROFILE", &home);
    let _state_home = EnvVarGuard::set("CODEWHALE_HOME", &root);
    let _config = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", tmp.path().join("config.toml"));
    crate::test_support::trust_workspace(&workspace);
    let (original_dir, _) = create_managed_skill(&workspace, "技能")?;
    create_managed_skill(&workspace, "分析")?;
    create_managed_skill(&workspace, "skill")?;
    let a = crate::skills::normalize_skill_name_for_lookup("技能");
    let b = crate::skills::normalize_skill_name_for_lookup("分析");
    assert_ne!(a, b);
    let state_path = root.join("skills_state.toml");
    let initial = b"disabled = [\"skill\"]\n";
    fs::write(&state_path, initial)?;
    let (addr, _threads, handle) = spawn_test_server_with_root_token_mobile_workspace(
        root.clone(),
        root.join("sessions"),
        None,
        false,
        workspace.clone(),
    )
    .await?
    .context("isolated skill API fixture requires loopback")?;
    let client = crate::tls::reqwest_client();
    let context = ToolContext::new(&workspace).with_skills_config(
        workspace.join(".codewhale/skills"),
        crate::skills::SkillDiscoveryMode::Compatible,
    );
    let tool = crate::tools::skill::LoadSkillTool;

    let before: serde_json::Value = client
        .get(format!("http://{addr}/v1/skills"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    for name in [a.as_str(), b.as_str(), "skill"] {
        let entry = before["skills"]
            .as_array()
            .context("skills")?
            .iter()
            .find(|skill| skill["name"] == name)
            .context("discovered identity")?;
        assert_eq!(entry["enabled"], false);
    }
    assert!(
        tool.execute(json!({"name":"技能"}), &context)
            .await
            .is_err()
    );
    assert_eq!(
        fs::read(&state_path)?,
        initial,
        "listing/loading must not migrate state"
    );

    let raw_toggle = client
        .post(format!("http://{addr}/v1/skills/技能"))
        .json(&json!({"enabled":true}))
        .send()
        .await?;
    assert_eq!(
        raw_toggle.status(),
        StatusCode::NOT_FOUND,
        "toggle accepts exact catalog IDs only"
    );
    for name in [a.as_str(), "skill"] {
        client
            .post(format!("http://{addr}/v1/skills/{name}"))
            .json(&json!({"enabled":true}))
            .send()
            .await?
            .error_for_status()?;
    }
    let after: serde_json::Value = client
        .get(format!("http://{addr}/v1/skills"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    for (name, enabled) in [(a.as_str(), true), (b.as_str(), false), ("skill", true)] {
        let entry = after["skills"]
            .as_array()
            .context("skills")?
            .iter()
            .find(|skill| skill["name"] == name)
            .context("discovered identity")?;
        assert_eq!(entry["enabled"], enabled);
    }
    for name in [a.as_str(), "技能", "skill"] {
        let loaded = tool.execute(json!({"name":name}), &context).await?;
        assert!(loaded.success);
    }
    assert!(
        tool.execute(json!({"name":"分析"}), &context)
            .await
            .is_err()
    );
    let audit: serde_json::Value = client
        .get(format!("http://{addr}/v1/skills/{a}/audit"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(audit["skills"][0]["name"], a);
    assert!(original_dir.join("SKILL.md").is_file());
    assert!(
        !workspace.join(".codewhale/skills").join(&a).exists(),
        "no directory rename"
    );
    client
        .delete(format!("http://{addr}/v1/skills/{a}?scope=project"))
        .send()
        .await?
        .error_for_status()?;
    assert!(
        !original_dir.exists(),
        "owned resolver must use the shared canonical identity"
    );
    assert!(workspace.join(".codewhale/skills/分析/SKILL.md").is_file());
    handle.abort();
    Ok(())
}

#[tokio::test]
async fn skills_endpoint_exposes_safe_plugin_provenance_and_shared_toggle() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("runtime");
    let workspace = tmp.path().join("workspace");
    let plugin_root = tmp.path().join("plugins/demo");
    fs::create_dir_all(plugin_root.join("skills/review"))?;
    fs::write(
        plugin_root.join("plugin.toml"),
        "schema_version = 1\n[plugin]\nname = \"demo\"\nversion = \"1.0.0\"\n[skills]\npath = \"skills\"\n",
    )?;
    fs::write(
        plugin_root.join("skills/review/SKILL.md"),
        "---\nname: review\ndescription: reviewed plugin Skill\n---\nbody\n",
    )?;
    let plugin_config = crate::plugins::discovery::DiscoveryConfig {
        workspace: workspace.clone(),
        user_plugins_dir: tmp.path().join("plugins"),
        workspace_plugins_dir: workspace.join(".codewhale/plugins"),
        builtin_plugin_dirs: Vec::new(),
        state_path: tmp.path().join("plugin-state/state.json"),
    };
    let discovery = crate::plugins::PluginDiscoveryContext::from_config_and_environment(
        &plugin_config,
        crate::plugins::HostEnvironment::default(),
    );
    let mut plugins = discovery.registry_for_workspace(&workspace);
    Arc::make_mut(&mut plugins)
        .trust("demo")
        .map_err(anyhow::Error::msg)?;
    Arc::make_mut(&mut plugins)
        .enable("demo")
        .map_err(anyhow::Error::msg)?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root_token_mobile_workspace_and_overrides(
            root.clone(),
            root.join("sessions"),
            None,
            false,
            workspace,
            TestServerOverrides {
                plugin_discovery: Some(discovery),
                ..TestServerOverrides::default()
            },
        )
        .await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let list = client
        .get(format!("http://{addr}/v1/skills"))
        .send()
        .await?
        .error_for_status()?
        .json::<serde_json::Value>()
        .await?;
    let plugin_skill = list["skills"]
        .as_array()
        .and_then(|skills| skills.iter().find(|skill| skill["name"] == "demo:review"))
        .context("plugin Skill in runtime API catalog")?;
    assert_eq!(plugin_skill["enabled"], true);
    assert_eq!(plugin_skill["path"], serde_json::Value::Null);
    assert_eq!(plugin_skill["source"], "reviewed-plugin-snapshot:demo");
    assert!(plugin_skill["plugin_id"].as_str().is_some());
    assert!(plugin_skill["plugin_generation"].as_u64().is_some());
    assert!(plugin_skill["plugin_content_hash"].as_str().is_some());
    assert!(
        !plugin_skill
            .to_string()
            .contains(&plugin_root.display().to_string()),
        "runtime API must not expose mutable or staged plugin paths"
    );

    client
        .post(format!("http://{addr}/v1/skills/demo:review"))
        .json(&json!({ "enabled": false }))
        .send()
        .await?
        .error_for_status()?;
    let after = client
        .get(format!("http://{addr}/v1/skills"))
        .send()
        .await?
        .error_for_status()?
        .json::<serde_json::Value>()
        .await?;
    let plugin_skill = after["skills"]
        .as_array()
        .and_then(|skills| skills.iter().find(|skill| skill["name"] == "demo:review"))
        .context("plugin Skill after toggle")?;
    assert_eq!(plugin_skill["enabled"], false);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn skill_toggle_endpoint_404s_for_unknown_skill() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let resp = client
        .post(format!("http://{addr}/v1/skills/no-such-skill"))
        .json(&json!({ "enabled": false }))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    handle.abort();
    Ok(())
}

#[test]
fn resolve_skills_scenario() {
    // Scenario consolidation of: resolve_skills_dir_finds_workspace_local_agents_skills, resolve_skills_dir_finds_workspace_local_skills_fallback, resolve_skills_dir_respects_codewhale_only_scan, resolve_skills_dir_preserves_explicit_dir_in_codewhale_only_scan
    // from resolve_skills_dir_finds_workspace_local_agents_skills
    {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = tmp.path();
        let local_skills = workspace.join(".agents").join("skills");
        crate::test_support::trust_workspace(workspace);
        fs::create_dir_all(&local_skills).expect("create skills dir");

        let config = Config::default();
        let resolved = resolve_skills_dir(&config, workspace);

        let expected = fs::canonicalize(&local_skills).expect("canonical local skills");
        assert_eq!(resolved, expected);
    }
    // from resolve_skills_dir_finds_workspace_local_skills_fallback
    {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = tmp.path();
        let local_skills = workspace.join("skills");
        crate::test_support::trust_workspace(workspace);
        fs::create_dir_all(&local_skills).expect("create skills dir");

        let config = Config {
            skills: Some(crate::config::SkillsConfig {
                flat_workspace_root: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolved = resolve_skills_dir(&config, workspace);

        let expected = fs::canonicalize(&local_skills).expect("canonical local skills");
        assert_eq!(resolved, expected);
    }
    // from resolve_skills_dir_respects_codewhale_only_scan
    {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = tmp.path();
        let agents_skills = workspace.join(".agents").join("skills");
        let codewhale_skills = workspace.join(".codewhale").join("skills");
        fs::create_dir_all(&agents_skills).expect("create agents skills dir");
        fs::create_dir_all(&codewhale_skills).expect("create codewhale skills dir");
        crate::test_support::trust_workspace(workspace);

        let config = Config {
            skills: Some(crate::config::SkillsConfig {
                scan_codewhale_only: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolved = resolve_skills_dir(&config, workspace);

        let expected = fs::canonicalize(&codewhale_skills).expect("canonical codewhale skills");
        assert_eq!(resolved, expected);
    }
    // from resolve_skills_dir_preserves_explicit_dir_in_codewhale_only_scan
    {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = tmp.path().join("workspace");
        let codewhale_skills = workspace.join(".codewhale").join("skills");
        let configured_skills = tmp.path().join("configured-skills");
        fs::create_dir_all(&codewhale_skills).expect("create codewhale skills dir");
        fs::create_dir_all(&configured_skills).expect("create configured skills dir");

        let config = Config {
            skills_dir: Some(configured_skills.to_string_lossy().into_owned()),
            skills: Some(crate::config::SkillsConfig {
                scan_codewhale_only: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolved = resolve_skills_dir(&config, &workspace);

        assert_eq!(resolved, configured_skills);
    }
}

#[test]
fn skills_search_directories_includes_custom_skills_dir() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let workspace = tmp.path().join("workspace");
    let custom_skills = tmp.path().join("custom-skills");
    fs::create_dir_all(&workspace).expect("create workspace");
    fs::create_dir_all(&custom_skills).expect("create custom skills");

    let directories = skills_search_directories(
        &workspace,
        &custom_skills,
        crate::skills::SkillDiscoveryMode::Compatible,
    );

    assert!(
        directories.iter().any(|dir| dir == &custom_skills),
        "custom skills_dir must be reported when discovery searches it"
    );
    let message = format_skill_search_paths(&directories);
    assert!(message.contains("custom-skills"));
}

#[test]
fn skill_entry_is_bundled_requires_configured_bundle_path() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bundled_skills_dir = tmp.path().join("bundled-skills");
    let bundled_skill_path = bundled_skills_dir.join("delegate").join("SKILL.md");
    let override_skill_path = tmp
        .path()
        .join("workspace")
        .join(".agents")
        .join("skills")
        .join("delegate")
        .join("SKILL.md");
    fs::create_dir_all(bundled_skill_path.parent().expect("bundled parent"))
        .expect("create bundled skill dir");
    fs::create_dir_all(override_skill_path.parent().expect("override parent"))
        .expect("create override skill dir");
    fs::write(
        &bundled_skill_path,
        "---\nname: delegate\ndescription: bundled\n---\n",
    )
    .expect("write bundled skill");
    fs::write(
        &override_skill_path,
        "---\nname: delegate\ndescription: override\n---\n",
    )
    .expect("write override skill");

    let bundled_skill = crate::skills::Skill {
        legacy_activation_name: None,
        name: "delegate".to_string(),
        description: String::new(),
        localized_descriptions: std::collections::HashMap::new(),
        invocation: crate::skills::SkillInvocation::ModelAndUser,
        aliases: Vec::new(),
        argument_hint: None,
        body: String::new(),
        path: bundled_skill_path,
        source: crate::skills::SkillSource::Native,
    };
    let override_skill = crate::skills::Skill {
        legacy_activation_name: None,
        name: "delegate".to_string(),
        description: String::new(),
        localized_descriptions: std::collections::HashMap::new(),
        invocation: crate::skills::SkillInvocation::ModelAndUser,
        aliases: Vec::new(),
        argument_hint: None,
        body: String::new(),
        path: override_skill_path,
        source: crate::skills::SkillSource::Native,
    };

    assert!(skill_entry_is_bundled(&bundled_skill, &bundled_skills_dir));
    assert!(!skill_entry_is_bundled(
        &override_skill,
        &bundled_skills_dir
    ));
}

/// A `skills` symlink that points outside the workspace must NOT be
/// returned as the resolved skills directory. Containment check ensures
/// the canonicalized candidate stays under the canonicalized workspace
/// root, so a malicious or misconfigured symlink can't promote
/// `/etc` (or any other path) into the skills loader.
#[cfg(unix)]
#[test]
fn resolve_skills_dir_rejects_symlink_escaping_workspace() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let _home = crate::test_support::SealedHome::at(tmp.path());
    let workspace_root = tmp.path().join("workspace");
    let escape_target = tmp.path().join("escape_target");
    fs::create_dir_all(&workspace_root).expect("create workspace");
    fs::create_dir_all(&escape_target).expect("create escape target");

    let dotagents = workspace_root.join(".agents");
    fs::create_dir_all(&dotagents).expect("create .agents");
    let bad_link = dotagents.join("skills");
    std::os::unix::fs::symlink(&escape_target, &bad_link).expect("symlink");

    let config = Config::default();
    let resolved = resolve_skills_dir(&config, &workspace_root);

    let canon_escape = fs::canonicalize(&escape_target).expect("canon escape");
    assert_ne!(
        resolved, canon_escape,
        "symlink escaping workspace must not be resolved as skills dir"
    );
    assert_eq!(
        resolved,
        config.skills_dir(),
        "with no valid in-workspace skills dir, resolution should fall back to config"
    );
}

/// An untrusted workspace's skill dirs must not become the resolved skills
/// dir: discovery would search it and bypass the workspace-trust gate.
#[test]
fn resolve_skills_dir_ignores_untrusted_workspace_skills() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let _home = crate::test_support::SealedHome::at(tmp.path());
    let codewhale_only = Config {
        skills: Some(crate::config::SkillsConfig {
            scan_codewhale_only: Some(true),
            ..Default::default()
        }),
        ..Default::default()
    };
    for (relative, config) in [
        (".agents/skills", Config::default()),
        (
            "skills",
            Config {
                skills: Some(crate::config::SkillsConfig {
                    flat_workspace_root: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ),
        (".codewhale/skills", codewhale_only),
    ] {
        let workspace = tmp
            .path()
            .join(format!("ws-{}", relative.replace('/', "-")));
        let local_skills = workspace.join(relative);
        fs::create_dir_all(&local_skills).expect("create skills dir");

        assert_eq!(
            resolve_skills_dir(&config, &workspace),
            config.skills_dir(),
            "untrusted {relative} must not be resolved as the skills dir"
        );

        crate::test_support::trust_workspace(&workspace);
        assert_eq!(
            resolve_skills_dir(&config, &workspace),
            fs::canonicalize(&local_skills).expect("canonical skills"),
            "trusted {relative} resolves as before"
        );
    }
}

#[cfg(unix)]
#[test]
fn resolve_skills_dir_rejects_codewhale_only_symlink_escaping_workspace() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let _home = crate::test_support::SealedHome::at(tmp.path());
    let workspace_root = tmp.path().join("workspace");
    let escape_target = tmp.path().join("escape_target");
    fs::create_dir_all(&workspace_root).expect("create workspace");
    fs::create_dir_all(&escape_target).expect("create escape target");

    let dotcodewhale = workspace_root.join(".codewhale");
    fs::create_dir_all(&dotcodewhale).expect("create .codewhale");
    let bad_link = dotcodewhale.join("skills");
    std::os::unix::fs::symlink(&escape_target, &bad_link).expect("symlink");

    let config = Config {
        skills: Some(crate::config::SkillsConfig {
            scan_codewhale_only: Some(true),
            ..Default::default()
        }),
        ..Default::default()
    };
    let resolved = resolve_skills_dir(&config, &workspace_root);

    let canon_escape = fs::canonicalize(&escape_target).expect("canon escape");
    assert_ne!(
        resolved, canon_escape,
        "CodeWhale-only symlink escaping workspace must not be resolved as skills dir"
    );
    assert_eq!(
        resolved,
        config.skills_dir(),
        "with no valid in-workspace CodeWhale skills dir, resolution should fall back to config"
    );
}

// ---------------------------------------------------------------------------
// /v1/config + /v1/config/reload endpoint tests
// ---------------------------------------------------------------------------

/// Helper: POST to `/v1/config` with the given key/value and return the
/// response status + body JSON.
async fn post_set_config(
    client: &reqwest::Client,
    addr: &SocketAddr,
    key: &str,
    value: &str,
    persist: bool,
) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = client
        .post(format!("http://{addr}/v1/config"))
        .json(&serde_json::json!({
            "key": key,
            "value": value,
            "persist": persist,
        }))
        .send()
        .await
        .expect("POST /v1/config should not fail at transport level");
    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .await
        .unwrap_or_else(|_| serde_json::json!({"_error": "non-json response body"}));
    (status, body)
}

#[tokio::test]
async fn set_config_rejects_unknown_key_with_bad_request() -> Result<()> {
    let root = std::env::temp_dir().join(format!("codewhale-config-unknown-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root(root, sessions_dir).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let (status, body) = post_set_config(&client, &addr, "nonexistent_key", "x", true).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "unknown key should return 400, body: {body}"
    );
    let message = body["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_lowercase();
    assert!(
        message.contains("unknown setting"),
        "error message should mention 'unknown setting', got: {message}"
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn set_config_validates_max_history_input() -> Result<()> {
    // Fix #4: invalid max_history input must return 400 instead of silently
    // falling back to a default value.
    let root = std::env::temp_dir().join(format!("codewhale-config-maxhist-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root(root, sessions_dir).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    // Non-integer input must be rejected.
    let (status, body) = post_set_config(&client, &addr, "max_history", "not-a-number", true).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "invalid max_history should return 400, body: {body}"
    );

    // Negative input must also be rejected (parse::<usize> rejects negatives).
    let (status, body) = post_set_config(&client, &addr, "max_history", "-5", true).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "negative max_history should return 400, body: {body}"
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn set_config_validates_subagents_enabled_input() -> Result<()> {
    // Fix #1: subagents_enabled must validate input and reject non-boolean
    // values with a descriptive 400 error.
    let root = std::env::temp_dir().join(format!("codewhale-config-subenabled-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root(root, sessions_dir).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let (status, body) = post_set_config(&client, &addr, "subagents_enabled", "maybe", true).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "non-boolean subagents_enabled should return 400, body: {body}"
    );
    let message = body["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_lowercase();
    assert!(
        message.contains("subagents_enabled"),
        "error message should name the key, got: {message}"
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn set_config_validates_subagents_max_depth_input() -> Result<()> {
    // Fix #1: subagents_max_depth must validate input and reject non-integer
    // values with a descriptive 400 error.
    let root = std::env::temp_dir().join(format!("codewhale-config-subdepth-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root(root, sessions_dir).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let (status, body) = post_set_config(&client, &addr, "subagents_max_depth", "deep", true).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "non-integer subagents_max_depth should return 400, body: {body}"
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn set_config_with_config_path_writes_to_specified_file() -> Result<()> {
    // Fix #2: when the server is started with --config, set_config must
    // persist to that specific file rather than the default discovery path.
    let root =
        std::env::temp_dir().join(format!("codewhale-config-path-persist-{}", Uuid::new_v4()));
    fs::create_dir_all(&root)?;
    let config_file = root.join("custom-config.toml");
    fs::write(&config_file, "# initial\n")?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    // Persist a subagents_max_depth value above the ceiling to also verify
    // clamping (Fix #1).
    let over_ceiling = u64::from(codewhale_config::MAX_SPAWN_DEPTH_CEILING) + 10;
    let (status, body) = post_set_config(
        &client,
        &addr,
        "subagents_max_depth",
        &over_ceiling.to_string(),
        true,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "persisting subagents_max_depth should succeed, body: {body}"
    );
    assert!(
        body["persisted"].as_bool().unwrap_or(false),
        "response should report persisted=true, body: {body}"
    );

    // Read the config file and verify the value was clamped and written.
    let contents = fs::read_to_string(&config_file)
        .with_context(|| format!("config file should exist at {}", config_file.display()))?;
    assert!(
        contents.contains("max_depth"),
        "config file should contain max_depth key, got: {contents}"
    );
    // The value should be clamped to MAX_SPAWN_DEPTH_CEILING.
    let expected = format!(
        "max_depth = {}",
        u64::from(codewhale_config::MAX_SPAWN_DEPTH_CEILING)
    );
    assert!(
        contents.contains(&expected),
        "config file should contain clamped value '{expected}', got: {contents}"
    );

    // Also verify a subagents_enabled persistence writes to the same file.
    let (status, body) = post_set_config(&client, &addr, "subagents_enabled", "true", true).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let contents = fs::read_to_string(&config_file)?;
    assert!(
        contents.contains("enabled = true"),
        "config file should contain enabled = true, got: {contents}"
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn get_config_search_provider_reports_runtime_source() -> Result<()> {
    let _lock = lock_test_env();
    let _code = EnvVarGuard::remove("CODEWHALE_SEARCH_PROVIDER");
    let _legacy = EnvVarGuard::remove("DEEPSEEK_SEARCH_PROVIDER");
    let _tavily = EnvVarGuard::remove("TAVILY_API_KEY");

    let root = std::env::temp_dir().join(format!("codewhale-search-source-{}", Uuid::new_v4()));
    fs::create_dir_all(&root)?;
    let config_file = root.join("config.toml");
    fs::write(&config_file, "[search]\napi_key = \"tvly-autodetected\"\n")?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let body: serde_json::Value = client
        .get(format!("http://{addr}/v1/config"))
        .send()
        .await
        .expect("GET /v1/config should not fail at transport level")
        .json()
        .await
        .expect("GET /v1/config should be JSON");
    handle.abort();

    assert_eq!(body["search_provider"], "tavily", "body: {body}");
    assert_eq!(
        body["search_provider_source"], "tavily key",
        "GET must publish the same source token doctor prints: {body}"
    );
    assert!(
        body.get("missing_key").is_none(),
        "missing-key is stdout-only: {body}"
    );
    Ok(())
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn set_config_search_provider_persist_skips_firecrawl_default_round_trip() -> Result<()> {
    let _lock = lock_test_env();
    let _code = EnvVarGuard::remove("CODEWHALE_SEARCH_PROVIDER");
    let _legacy = EnvVarGuard::remove("DEEPSEEK_SEARCH_PROVIDER");
    let _tavily = EnvVarGuard::remove("TAVILY_API_KEY");

    let root = std::env::temp_dir().join(format!("codewhale-search-persist-{}", Uuid::new_v4()));
    fs::create_dir_all(&root)?;
    let config_file = root.join("config.toml");
    fs::write(&config_file, "# initial\n")?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    // GET returns `firecrawl`; saving it back must NOT write a pin, because
    // `provider = "firecrawl"` would flip source Default -> Config and then
    // block a later Tavily key.
    let (status, body) =
        post_set_config(&client, &addr, "search_provider", "firecrawl", true).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["persisted"], false, "body: {body}");
    let contents = fs::read_to_string(&config_file)?;
    assert!(
        !contents.contains("provider"),
        "round-tripping the resolved default must not pin it: {contents}"
    );

    // An explicit *change* still persists.
    let (status, body) = post_set_config(&client, &addr, "search_provider", "bocha", true).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["persisted"], true, "body: {body}");
    let contents = fs::read_to_string(&config_file)?;
    assert!(
        contents.contains("provider = \"bocha\""),
        "an explicit provider change must persist: {contents}"
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn set_config_search_provider_persist_skips_tavily_autodetect() -> Result<()> {
    let _lock = lock_test_env();
    let _code = EnvVarGuard::remove("CODEWHALE_SEARCH_PROVIDER");
    let _legacy = EnvVarGuard::remove("DEEPSEEK_SEARCH_PROVIDER");
    let _tavily = EnvVarGuard::remove("TAVILY_API_KEY");

    let root = std::env::temp_dir().join(format!("codewhale-search-auto-{}", Uuid::new_v4()));
    fs::create_dir_all(&root)?;
    let config_file = root.join("config.toml");
    fs::write(&config_file, "[search]\napi_key = \"tvly-autodetected\"\n")?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let (status, body) = post_set_config(&client, &addr, "search_provider", "tavily", true).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["persisted"], false, "body: {body}");
    assert!(
        body["message"]
            .as_str()
            .is_some_and(|m| m.contains("tavily key")),
        "the skip must explain the autodetect source: {body}"
    );
    let contents = fs::read_to_string(&config_file)?;
    assert!(
        !contents.contains("provider"),
        "GET -> POST round-trip must not pin autodetect: {contents}"
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn reload_config_endpoint_returns_success() -> Result<()> {
    // Basic smoke test that /v1/config/reload returns 200 with a message.
    let root = std::env::temp_dir().join(format!("codewhale-config-reload-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root(root, sessions_dir).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let resp = client
        .post(format!("http://{addr}/v1/config/reload"))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = resp.json().await?;
    let message = body["message"].as_str().unwrap_or_default().to_string();
    assert!(
        !message.is_empty(),
        "reload response should include a non-empty message"
    );

    handle.abort();
    Ok(())
}

/// Helper: GET `/v1/config` and return the parsed response body.
async fn get_config(client: &reqwest::Client, addr: &SocketAddr) -> serde_json::Value {
    client
        .get(format!("http://{addr}/v1/config"))
        .send()
        .await
        .expect("GET /v1/config should not fail at transport level")
        .error_for_status()
        .expect("GET /v1/config should return 200")
        .json()
        .await
        .expect("GET /v1/config should return valid JSON")
}

async fn get_providers(client: &reqwest::Client, addr: &SocketAddr) -> serde_json::Value {
    client
        .get(format!("http://{addr}/v1/providers"))
        .send()
        .await
        .expect("GET /v1/providers should not fail at transport level")
        .error_for_status()
        .expect("GET /v1/providers should return 200")
        .json()
        .await
        .expect("GET /v1/providers should return valid JSON")
}

async fn get_provider_models(
    client: &reqwest::Client,
    addr: &SocketAddr,
    provider: &str,
) -> serde_json::Value {
    client
        .get(format!("http://{addr}/v1/providers/{provider}/models"))
        .send()
        .await
        .expect("GET /v1/providers/{id}/models should not fail at transport level")
        .error_for_status()
        .expect("GET /v1/providers/{id}/models should return 200")
        .json()
        .await
        .expect("GET /v1/providers/{id}/models should return valid JSON")
}

/// Helper: GET `/v1/providers/{id}/models` for one exact configured identity —
/// the read a client performs with the `model_provider_id` the provider
/// catalog published.
async fn get_provider_models_with_identity(
    client: &reqwest::Client,
    addr: &SocketAddr,
    provider: &str,
    model_provider_id: &str,
) -> serde_json::Value {
    client
        .get({
            let mut url =
                reqwest::Url::parse(&format!("http://{addr}/v1/providers/{provider}/models"))
                    .expect("provider models URL");
            url.query_pairs_mut()
                .append_pair("model_provider_id", model_provider_id);
            url
        })
        .send()
        .await
        .expect("GET /v1/providers/{id}/models should not fail at transport level")
        .error_for_status()
        .expect("GET /v1/providers/{id}/models should return 200")
        .json()
        .await
        .expect("GET /v1/providers/{id}/models should return valid JSON")
}

/// Helper: GET `/v1/settings/schema` and return the parsed response body.
async fn get_settings_schema(client: &reqwest::Client, addr: &SocketAddr) -> serde_json::Value {
    client
        .get(format!("http://{addr}/v1/settings/schema"))
        .send()
        .await
        .expect("GET /v1/settings/schema should not fail at transport level")
        .error_for_status()
        .expect("GET /v1/settings/schema should return 200")
        .json()
        .await
        .expect("GET /v1/settings/schema should return valid JSON")
}

#[tokio::test]
async fn settings_schema_serves_every_declared_row_with_runtime_state() -> Result<()> {
    let _lock = lock_test_env();
    let root = tempfile::tempdir()?;
    let config_file = root.path().join("config.toml");
    fs::write(
        &config_file,
        "provider = \"deepseek\"\napi_key = \"runtime-api-test-key\"\nbase_url = \"http://127.0.0.1:1/v1\"\ntelemetry = false\n\n[notifications]\ncondition = \"always\"\n",
    )?;
    fs::write(root.path().join("settings.toml"), "calm_mode = true\n")?;
    let _config = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", &config_file);
    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let body = get_settings_schema(&client, &addr).await;
    handle.abort();

    assert_eq!(body["version"], 1);
    let tabs = body["tabs"].as_array().expect("tabs array");
    let tab_ids: Vec<_> = tabs.iter().filter_map(|t| t["id"].as_str()).collect();
    assert_eq!(
        tab_ids,
        codewhale_config::settings_schema::schema_tabs(),
        "tabs project the schema declaration order"
    );

    let rows = body["settings"].as_array().expect("settings array");
    assert_eq!(
        rows.len(),
        codewhale_config::settings_schema::schema_rows().count(),
        "every declared UI row is served"
    );
    let row = |key: &str| {
        rows.iter()
            .find(|row| row["key"] == key)
            .unwrap_or_else(|| panic!("schema row {key} missing"))
    };

    for entry in rows {
        let kind = entry["kind"].as_str().expect("kind");
        assert!(
            matches!(kind, "bool" | "int" | "float" | "enum" | "string"),
            "unknown kind {kind}"
        );
        assert!(
            matches!(
                entry["row"].as_str(),
                Some("setting" | "action" | "diagnostic" | "session")
            ),
            "unknown row kind for {}",
            entry["key"]
        );
        let label = entry["label"].as_str().expect("label");
        assert!(
            !label.is_empty() && !label.starts_with("Config"),
            "label must be resolved prose, not a message key: {label}"
        );
        assert!(entry["visible"].is_boolean());
        assert!(entry["editable"].is_boolean());
        if kind == "enum" {
            assert!(
                !entry["options"]
                    .as_array()
                    .expect("enum options")
                    .is_empty(),
                "enum row {} must serve its closed value set",
                entry["key"]
            );
        }
    }

    // Values resolve from the owning store, not a decorated guess.
    assert_eq!(row("calm_mode")["value"], "true");
    assert_eq!(row("calm_mode")["persisted"], true);
    assert_eq!(row("notifications.condition")["value"], "always");
    assert_eq!(row("notifications.condition")["persisted"], true);
    assert_eq!(row("telemetry")["value"], "false");

    // Row kinds and editability agree with the TUI's own semantics:
    // model/provider open pickers (actions), approval_mode is the
    // session-scoped writable, endpoint rows are read-only receipts.
    assert_eq!(row("model")["row"], "action");
    assert_eq!(row("model")["editable"], false);
    assert_eq!(row("approval_mode")["row"], "session");
    assert_eq!(row("approval_mode")["editable"], true);
    assert_eq!(row("provider_url")["editable"], false);
    assert_eq!(row("telemetry")["editable"], false);
    for action in ["mcp_open", "plugins_open"] {
        assert_eq!(row(action)["row"], "action");
        assert_eq!(row(action)["editable"], false);
    }

    // The conditional triple resolves to exactly one visible member for a
    // config with no managed policy: the editable TUI posture row.
    assert_eq!(row("permission_posture")["visible"], true);
    assert_eq!(row("approval_policy")["visible"], false);
    assert_eq!(row("managed_approval_policy")["visible"], false);
    Ok(())
}

#[tokio::test]
async fn settings_schema_write_fallthrough_persists_declared_settings_keys() -> Result<()> {
    let _lock = lock_test_env();
    let root = tempfile::tempdir()?;
    let config_file = root.path().join("config.toml");
    fs::write(
        &config_file,
        "provider = \"deepseek\"\napi_key = \"runtime-api-test-key\"\nbase_url = \"http://127.0.0.1:1/v1\"\n",
    )?;
    let _config = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", &config_file);
    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    // A schema-declared settings.toml key outside the curated arm list must
    // persist through the shared validator rather than 400.
    let response = client
        .post(format!("http://{addr}/v1/config"))
        .json(&json!({"key": "composer_density", "value": "compact", "persist": true}))
        .send()
        .await?
        .error_for_status()
        .expect("composer_density is a declared settings key")
        .json::<serde_json::Value>()
        .await?;
    assert_eq!(response["persisted"], true);
    assert_eq!(
        crate::settings::Settings::load_persisted()?.composer_density,
        "compact"
    );
    let body = get_settings_schema(&client, &addr).await;
    let row = body["settings"]
        .as_array()
        .expect("settings array")
        .iter()
        .find(|row| row["key"] == "composer_density")
        .expect("composer_density row");
    assert_eq!(row["value"], "compact");
    assert_eq!(row["persisted"], true);

    // A key the schema does not declare still fails closed.
    let status = client
        .post(format!("http://{addr}/v1/config"))
        .json(&json!({"key": "not_a_real_setting", "value": "x", "persist": true}))
        .send()
        .await?
        .status();
    assert_eq!(status, StatusCode::BAD_REQUEST);

    handle.abort();
    Ok(())
}

#[test]
fn provider_model_catalog_paginates_all_six_hundred_rows_without_truncation() {
    let models = (0..600)
        .rev()
        .map(|index| ProviderModelEntry {
            reasoning_effort: codewhale_config::route::CapabilityState::Unknown,
            reasoning_effort_levels: Vec::new(),
            reasoning_effort_source: None,
            output_token_limit: codewhale_config::route::CapabilityState::Unknown,
            id: format!("openrouter/model-{index:03}"),
            image_input: codewhale_config::route::CapabilityState::Unknown,
        })
        .collect::<Vec<_>>();
    let mut cursor = None;
    let mut observed = Vec::new();
    let mut page_count = 0usize;

    loop {
        let response = paginate_provider_models(
            "openrouter",
            models.clone(),
            &ListProviderModelsParams {
                filter: None,
                cursor,
                limit: Some(MAX_PROVIDER_MODELS_PAGE_SIZE),
                ..Default::default()
            },
            None,
        )
        .expect("page should be valid");
        page_count += 1;
        assert_eq!(response.provider, "openrouter");
        assert_eq!(response.total, 600);
        assert!(!response.models.is_empty());
        assert!(response.models.len() <= MAX_PROVIDER_MODELS_PAGE_SIZE);
        observed.extend(response.models.into_iter().map(|entry| entry.id));
        cursor = response.next_cursor;
        if cursor.is_none() {
            break;
        }
    }

    assert_eq!(page_count, 3);
    assert_eq!(observed.len(), 600);
    assert_eq!(
        observed.first().map(String::as_str),
        Some("openrouter/model-000")
    );
    assert_eq!(
        observed.last().map(String::as_str),
        Some("openrouter/model-599")
    );
    let unique = observed.iter().collect::<std::collections::BTreeSet<_>>();
    assert_eq!(unique.len(), 600);
}

#[test]
fn provider_model_catalog_applies_filter_before_cursor_and_rejects_cross_scope_replay() {
    let models = ["Alpha-One", "alpha-two", "beta"]
        .into_iter()
        .map(|id| ProviderModelEntry {
            reasoning_effort: codewhale_config::route::CapabilityState::Unknown,
            reasoning_effort_levels: Vec::new(),
            reasoning_effort_source: None,
            output_token_limit: codewhale_config::route::CapabilityState::Unknown,
            id: id.to_string(),
            image_input: codewhale_config::route::CapabilityState::Unknown,
        })
        .collect::<Vec<_>>();
    let first = paginate_provider_models(
        "openrouter",
        models.clone(),
        &ListProviderModelsParams {
            filter: Some(" ALPHA ".to_string()),
            cursor: None,
            limit: Some(1),
            ..Default::default()
        },
        None,
    )
    .expect("filtered first page should be valid");
    assert_eq!(first.total, 2);
    assert_eq!(first.models[0].id, "Alpha-One");
    let cursor = first.next_cursor.expect("a second filtered row remains");

    let second = paginate_provider_models(
        "openrouter",
        models.clone(),
        &ListProviderModelsParams {
            filter: Some("alpha".to_string()),
            cursor: Some(cursor.clone()),
            limit: Some(1),
            ..Default::default()
        },
        None,
    )
    .expect("matching filter should continue");
    assert_eq!(second.models[0].id, "alpha-two");
    assert!(second.next_cursor.is_none());

    let replay = paginate_provider_models(
        "baseten",
        models,
        &ListProviderModelsParams {
            filter: Some("alpha".to_string()),
            cursor: Some(cursor),
            limit: Some(1),
            ..Default::default()
        },
        None,
    );
    assert!(replay.is_err(), "a cursor cannot cross provider ownership");
}

#[test]
fn provider_model_cursor_rejects_catalog_change_between_pages() {
    let models = ["bravo", "charlie"]
        .into_iter()
        .map(|id| ProviderModelEntry {
            reasoning_effort: codewhale_config::route::CapabilityState::Unknown,
            reasoning_effort_levels: Vec::new(),
            reasoning_effort_source: None,
            output_token_limit: codewhale_config::route::CapabilityState::Unknown,
            id: id.to_string(),
            image_input: codewhale_config::route::CapabilityState::Unknown,
        })
        .collect::<Vec<_>>();
    let first = paginate_provider_models(
        "openrouter",
        models.clone(),
        &ListProviderModelsParams {
            filter: None,
            cursor: None,
            limit: Some(1),
            ..Default::default()
        },
        None,
    )
    .expect("first page");
    let cursor = first.next_cursor.expect("second page remains");
    let mut changed = models.clone();
    changed.push(ProviderModelEntry {
        reasoning_effort: codewhale_config::route::CapabilityState::Unknown,
        reasoning_effort_levels: Vec::new(),
        reasoning_effort_source: None,
        output_token_limit: codewhale_config::route::CapabilityState::Unknown,
        id: "alpha".to_string(),
        image_input: codewhale_config::route::CapabilityState::Unknown,
    });
    assert!(
        paginate_provider_models(
            "openrouter",
            changed,
            &ListProviderModelsParams {
                filter: None,
                cursor: Some(cursor.clone()),
                limit: Some(1),
                ..Default::default()
            },
            None,
        )
        .is_err(),
        "an insertion before the cursor must force a restart, not disappear"
    );
    let mut changed = models;
    changed[1].image_input = codewhale_config::route::CapabilityState::Supported;
    assert!(
        paginate_provider_models(
            "openrouter",
            changed,
            &ListProviderModelsParams {
                filter: None,
                cursor: Some(cursor),
                limit: Some(1),
                ..Default::default()
            },
            None,
        )
        .is_err(),
        "capability changes also invalidate a catalog snapshot"
    );
}

#[test]
fn provider_model_cursor_round_trips_multibyte_filter_at_the_allowed_limit() {
    let filter = "😀".repeat(MAX_PROVIDER_MODELS_FILTER_CHARS);
    let models = ["a", "b"]
        .into_iter()
        .map(|suffix| ProviderModelEntry {
            reasoning_effort: codewhale_config::route::CapabilityState::Unknown,
            reasoning_effort_levels: Vec::new(),
            reasoning_effort_source: None,
            output_token_limit: codewhale_config::route::CapabilityState::Unknown,
            id: format!("{filter}{suffix}"),
            image_input: codewhale_config::route::CapabilityState::Unknown,
        })
        .collect::<Vec<_>>();
    let first = paginate_provider_models(
        "openrouter",
        models.clone(),
        &ListProviderModelsParams {
            filter: Some(filter.clone()),
            cursor: None,
            limit: Some(1),
            ..Default::default()
        },
        None,
    )
    .expect("first page");
    let second = paginate_provider_models(
        "openrouter",
        models,
        &ListProviderModelsParams {
            filter: Some(filter),
            cursor: first.next_cursor,
            limit: Some(1),
            ..Default::default()
        },
        None,
    )
    .expect("all emitted cursors must be accepted, including multibyte filters");
    assert_eq!(second.models.len(), 1);
    assert!(second.next_cursor.is_none());
}

#[test]
fn runtime_chat_relay_projection_keeps_six_hundred_safe_models_without_silent_cutoff() {
    let models = (0..600)
        .map(|index| format!("openrouter/model-{index:03}"))
        .collect::<Vec<_>>();
    let projected = runtime_chat_safe_models(models.clone()).expect("600 rows are safely bounded");
    assert_eq!(projected, models);

    let oversized = (0..=MAX_PROVIDER_MODELS_CATALOG_SIZE)
        .map(|index| format!("provider/model-{index:05}"))
        .collect::<Vec<_>>();
    let error = runtime_chat_safe_models(oversized).expect_err("oversized relay must fail loudly");
    assert!(error.contains("safe"));
    assert!(error.contains(&MAX_PROVIDER_MODELS_CATALOG_SIZE.to_string()));
}

#[tokio::test]
async fn provider_catalog_and_switch_preserve_each_listed_route_identity() -> Result<()> {
    let _lock = lock_test_env();
    let root = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("CODEWHALE_HOME", root.path().join("home"));
    let variants = [
        ("modelstudio-token-plan", "token-chat-only"),
        ("modelstudio-token-plan-anthropic", "token-anthropic-only"),
        ("modelstudio-coding-plan", "coding-chat-only"),
        ("modelstudio-coding-plan-anthropic", "coding-anthropic-only"),
    ];
    let config_file = root.path().join("config.toml");
    let mut config = String::from(
        "provider = \"deepseek\"\napi_key = \"runtime-api-test-key\"\nbase_url = \"http://127.0.0.1:1/v1\"\ntelemetry = false\n",
    );
    for (provider, model) in variants {
        config.push_str(&format!(
            "\n[providers.{provider}]\nbase_url = \"http://127.0.0.1:1/v1\"\nmodel = \"{model}\"\n"
        ));
    }
    fs::write(&config_file, config)?;
    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let providers = get_providers(&client, &addr).await;
    let catalog = providers["providers"].as_array().expect("provider list");
    assert!(!catalog.is_empty());
    for provider in catalog {
        if provider["has_model_catalog"] == true {
            let id = provider["id"].as_str().expect("provider id");
            let models = get_provider_models(&client, &addr, id).await;
            assert_eq!(models["provider"], id, "catalog identity for {id}");
        }
    }

    for (provider, model) in variants {
        assert!(catalog.iter().any(|entry| entry["id"] == provider));
        let models = get_provider_models(&client, &addr, provider).await;
        let model_ids: Vec<_> = models["models"]
            .as_array()
            .expect("models array")
            .iter()
            .map(|entry| entry["id"].as_str().expect("model id"))
            .collect();
        assert_eq!(
            model_ids,
            vec![model],
            "route-specific model for {provider}"
        );

        let switched: serde_json::Value = client
            .post(format!("http://{addr}/v1/providers/{provider}/switch"))
            .json(&json!({}))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_eq!(switched["provider"], provider);
        assert_eq!(switched["model"], model);
        assert_eq!(switched["persisted"], true);
        let saved: toml::Value = toml::from_str(&fs::read_to_string(&config_file)?)?;
        assert_eq!(saved["provider"].as_str(), Some(provider));
        for (identity, expected_model) in variants {
            assert_eq!(
                saved["providers"][identity]["model"].as_str(),
                Some(expected_model)
            );
        }
    }
    let legacy_alias = get_provider_models(&client, &addr, "deepseek-cn").await;
    assert_eq!(legacy_alias["provider"], "deepseek-cn");
    for invalid in ["deepseek-cn-unknown", "antigravity", "not-a-provider"] {
        let response = client
            .get(format!("http://{addr}/v1/providers/{invalid}/models"))
            .send()
            .await?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    handle.abort();
    let _ = handle.await;
    Ok(())
}

#[tokio::test]
async fn get_config_returns_active_provider_model() -> Result<()> {
    let root = std::env::temp_dir().join(format!(
        "codewhale-config-active-provider-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&root)?;
    let config_file = root.join("custom-config.toml");
    fs::write(
        &config_file,
        format!(
            "default_text_model = \"deepseek-v4-pro\"\nprovider = \"volcengine\"\n\n[providers.volcengine]\nmodel = \"{}\"\n",
            crate::config::DEFAULT_VOLCENGINE_FLASH_MODEL
        ),
    )?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let body = get_config(&client, &addr).await;
    assert_eq!(body["provider"].as_str(), Some("volcengine"));
    assert_eq!(
        body["model"].as_str(),
        Some(crate::config::DEFAULT_VOLCENGINE_FLASH_MODEL),
        "GET /v1/config should expose the active provider model, not the root DeepSeek default"
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn api_surfaces_only_configured_model_for_custom_provider_route() -> Result<()> {
    let root = std::env::temp_dir().join(format!(
        "codewhale-config-custom-provider-model-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&root)?;
    let config_file = root.join("custom-config.toml");
    fs::write(
        &config_file,
        "provider = \"volcengine\"\n\n[providers.volcengine]\nbase_url = \"https://ark.cn-beijing.volces.com/api/plan/v3\"\nmodel = \"glm-5.2\"\napi_key = \"ark-test\"\n",
    )?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let config_body = get_config(&client, &addr).await;
    assert_eq!(config_body["provider"].as_str(), Some("volcengine"));
    assert_eq!(
        config_body["model"].as_str(),
        Some("glm-5.2"),
        "GET /v1/config should preserve the active provider's explicit custom model"
    );

    let providers = get_providers(&client, &addr).await;
    let volcengine = providers["providers"]
        .as_array()
        .and_then(|providers| {
            providers
                .iter()
                .find(|entry| entry["id"].as_str() == Some("volcengine"))
        })
        .expect("volcengine provider entry");
    assert_eq!(providers["current"].as_str(), Some("volcengine"));
    assert_eq!(
        volcengine["default_model"].as_str(),
        Some("glm-5.2"),
        "GET /v1/providers should mirror the /provider default route when a saved model override exists"
    );

    let provider_models = get_provider_models(&client, &addr, "volcengine").await;
    let model_ids: Vec<_> = provider_models["models"]
        .as_array()
        .expect("models array")
        .iter()
        .filter_map(|entry| entry["id"].as_str())
        .collect();
    assert_eq!(
        model_ids.first().copied(),
        Some("glm-5.2"),
        "configured volcengine model should be the only model exposed for a custom provider route"
    );
    assert_eq!(model_ids, vec!["glm-5.2"]);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn api_surfaces_only_active_model_when_runtime_route_passes_ids_through() -> Result<()> {
    let root = std::env::temp_dir().join(format!(
        "codewhale-config-runtime-pass-through-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&root)?;
    let config_file = root.join("custom-config.toml");
    fs::write(
        &config_file,
        "provider = \"volcengine\"\nbase_url = \"https://ark.cn-beijing.volces.com/api/plan/v3\"\n\n[providers.volcengine]\nmodel = \"glm-5.2\"\napi_key = \"ark-test\"\n",
    )?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let config_body = get_config(&client, &addr).await;
    assert_eq!(config_body["provider"].as_str(), Some("volcengine"));
    assert_eq!(config_body["model"].as_str(), Some("glm-5.2"));

    let providers = get_providers(&client, &addr).await;
    let volcengine = providers["providers"]
        .as_array()
        .and_then(|providers| {
            providers
                .iter()
                .find(|entry| entry["id"].as_str() == Some("volcengine"))
        })
        .expect("volcengine provider entry");
    assert_eq!(providers["current"].as_str(), Some("volcengine"));
    assert_eq!(volcengine["default_model"].as_str(), Some("glm-5.2"));

    let provider_models = get_provider_models(&client, &addr, "volcengine").await;
    let model_ids: Vec<_> = provider_models["models"]
        .as_array()
        .expect("models array")
        .iter()
        .filter_map(|entry| entry["id"].as_str())
        .collect();
    assert_eq!(model_ids, vec!["glm-5.2"]);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn provider_models_expose_exact_image_input_facts_and_thread_selection_stays_local()
-> Result<()> {
    let root = std::env::temp_dir().join(format!(
        "codewhale-provider-model-capabilities-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&root)?;
    let config_file = root.join("custom-config.toml");
    fs::write(
        &config_file,
        "provider = \"deepseek\"\ndefault_text_model = \"deepseek-v4-pro\"\n",
    )?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let models = get_provider_models(&client, &addr, "deepseek").await;
    let entries = models["models"].as_array().context("models array")?;
    let vision = entries
        .iter()
        .find(|entry| entry["id"] == "deepseek-v4-flash-vision-exp")
        .context("DeepSeek vision model entry")?;
    assert_eq!(vision["image_input"], "supported");
    let text_only = entries
        .iter()
        .find(|entry| entry["id"] == "deepseek-v4-pro")
        .context("DeepSeek text model entry")?;
    assert_eq!(text_only["image_input"], "unsupported");
    // The effort ladder is the one the catalog row publishes (#6396).
    assert_eq!(text_only["reasoning_effort"], "supported");
    assert!(
        text_only["reasoning_effort_levels"]
            .as_array()
            .is_some_and(|levels| !levels.is_empty())
    );

    let config_before = get_config(&client, &addr).await;
    let response = client
        .post(format!("http://{addr}/v1/threads"))
        .json(&json!({
            "model_provider": "deepseek",
            "model": "deepseek-v4-flash-vision-exp",
        }))
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::CREATED);
    let thread: serde_json::Value = response.json().await?;
    assert_eq!(thread["model_provider"], "deepseek");
    assert_eq!(thread["model"], "deepseek-v4-flash-vision-exp");

    let config_after = get_config(&client, &addr).await;
    assert_eq!(config_after["provider"], config_before["provider"]);
    assert_eq!(config_after["model"], config_before["model"]);

    handle.abort();
    Ok(())
}

#[test]
fn provider_reasoning_metadata_keeps_exact_roster_levels_and_unknown_boundaries() {
    let _lock = crate::test_support::lock_test_env();
    let root = tempfile::tempdir().unwrap();
    let canonical_home = root
        .path()
        .canonicalize()
        .expect("canonical private fixture home");
    let _codex_home = crate::test_support::EnvVarGuard::set("CODEX_HOME", &canonical_home);
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &canonical_home);
    let mut config = Config {
        provider: Some("openai-codex".to_string()),
        ..Config::default()
    };
    let access_token = crate::oauth::install_test_chatgpt_registration(&mut config).unwrap();
    crate::codex_model_cache::install_test_chatgpt_roster_with_metadata(
        &config,
        [
            (
                "gpt-6-astra",
                None,
                vec![
                    "low",
                    "medium",
                    "high",
                    "xhigh",
                    "max",
                    "ultra",
                    "unexpected-private-value",
                ],
            ),
            ("gpt-5.5", None, vec!["high", "xhigh"]),
            ("no-reasoning", Some(false), vec![]),
            ("optional-reasoning", None, vec!["none", "minimal", "low"]),
            ("no-effort-metadata", None, vec![]),
            ("unrecognized-efforts", None, vec!["future-value"]),
        ]
        .into_iter()
        .map(
            |(id, reasoning, efforts)| crate::codex_model_cache::CodexModelMetadata {
                id: id.into(),
                display_name: None,
                context_window: None,
                reasoning,
                efforts: efforts.into_iter().map(str::to_string).collect(),
            },
        )
        .collect(),
    )
    .unwrap();
    // Locate the one snapshot actually created by the account-scoped helper;
    // an external CODEX_HOME cache is not an account roster authority.
    let catalog_path = crate::models_dev_live::cache_path().unwrap();
    let snapshots: Vec<_> = fs::read_dir(catalog_path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("chatgpt-plan-"))
        })
        .collect();
    assert_eq!(snapshots.len(), 1);
    let cache_path = &snapshots[0];
    let astra = provider_model_entry_for_api(
        &config,
        &(config).test_identity_for_kind(ProviderKind::OpenaiCodex),
        "gpt-6-astra".to_string(),
    );
    assert_eq!(
        astra.reasoning_effort,
        codewhale_config::route::CapabilityState::Supported
    );
    assert_eq!(
        astra.reasoning_effort_levels,
        ["low", "medium", "high", "xhigh", "max", "ultra"]
    );
    assert_eq!(astra.reasoning_effort_source, Some("chatgpt_plan_api"));
    // The relay preserves the same model facts without elevating them into
    // a tool-execution or account-entitlement receipt.
    let _token = crate::test_support::EnvVarGuard::set("OPENAI_CODEX_ACCESS_TOKEN", "test-token");
    let _legacy = crate::test_support::EnvVarGuard::remove("CODEX_ACCESS_TOKEN");
    let relay = runtime_chat_relay_catalog(&config, &"c".repeat(32)).unwrap();
    let relayed_astra = relay["providers"][0]["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["id"] == "gpt-6-astra")
        .unwrap();
    assert_eq!(relayed_astra["reasoningEffort"], "supported");
    assert_eq!(
        relayed_astra["reasoningEffortLevels"],
        json!(astra.reasoning_effort_levels)
    );
    assert_eq!(relayed_astra["reasoningEffortSource"], "chatgpt_plan_api");
    assert_eq!(relay["runtime"]["capabilities"]["tool_execution"], false);
    assert!(!relay.to_string().contains("test-token"));
    assert!(!relay.to_string().contains(&access_token));
    let older = provider_model_entry_for_api(
        &config,
        &(config).test_identity_for_kind(ProviderKind::OpenaiCodex),
        "gpt-5.5".to_string(),
    );
    assert_eq!(older.reasoning_effort_levels, ["high", "xhigh"]);
    let unsupported = provider_model_entry_for_api(
        &config,
        &(config).test_identity_for_kind(ProviderKind::OpenaiCodex),
        "no-reasoning".to_string(),
    );
    assert_eq!(
        unsupported.reasoning_effort,
        codewhale_config::route::CapabilityState::Unsupported
    );
    assert!(unsupported.reasoning_effort_levels.is_empty());
    let optional = provider_model_entry_for_api(
        &config,
        &(config).test_identity_for_kind(ProviderKind::OpenaiCodex),
        "optional-reasoning".to_string(),
    );
    assert_eq!(optional.reasoning_effort_levels, ["low"]);
    assert_eq!(
        optional.reasoning_effort,
        codewhale_config::route::CapabilityState::Supported
    );
    for id in ["no-effort-metadata", "unrecognized-efforts"] {
        let unknown = provider_model_entry_for_api(
            &config,
            &(config).test_identity_for_kind(ProviderKind::OpenaiCodex),
            id.to_string(),
        );
        assert_eq!(
            unknown.reasoning_effort,
            codewhale_config::route::CapabilityState::Unknown
        );
        assert!(unknown.reasoning_effort_levels.is_empty());
    }
    let missing = provider_model_entry_for_api(
        &config,
        &(config).test_identity_for_kind(ProviderKind::OpenaiCodex),
        "unknown-model".to_string(),
    );
    assert_eq!(
        missing.reasoning_effort,
        codewhale_config::route::CapabilityState::Unknown
    );
    assert!(missing.reasoning_effort_levels.is_empty());
    let mut custom = config.clone();
    custom
        .provider_config_for_mut(&custom.test_identity_for_kind(ProviderKind::OpenaiCodex))
        .unwrap()
        .base_url = Some("https://proxy.example.test/v1".to_string());
    let proxy = provider_model_entry_for_api(
        &custom,
        &(custom).test_identity_for_kind(ProviderKind::OpenaiCodex),
        "gpt-6-astra".to_string(),
    );
    assert_eq!(
        proxy.reasoning_effort,
        codewhale_config::route::CapabilityState::Unknown
    );
    assert!(proxy.reasoning_effort_levels.is_empty());
    let mut stale: Value = serde_json::from_slice(&fs::read(cache_path).unwrap()).unwrap();
    stale["fetched_at"] = json!(chrono::Utc::now() - chrono::Duration::hours(48));
    fs::write(cache_path, serde_json::to_vec(&stale).unwrap()).unwrap();
    let stale = provider_model_entry_for_api(
        &config,
        &(config).test_identity_for_kind(ProviderKind::OpenaiCodex),
        "gpt-6-astra".to_string(),
    );
    assert_eq!(
        stale.reasoning_effort,
        codewhale_config::route::CapabilityState::Unknown
    );
    assert!(stale.reasoning_effort_levels.is_empty());
}

#[tokio::test]
async fn provider_models_query_keeps_named_routes_separate_without_switching() -> Result<()> {
    let root = tempfile::tempdir()?;
    let config_file = root.path().join("config.toml");
    fs::write(
        &config_file,
        r#"provider = "first"
[providers.first]
kind = "openai-compatible"
base_url = "http://127.0.0.1:18190/v1"
model = "first-model"
[providers.second]
kind = "openai-compatible"
base_url = "http://127.0.0.1:18191/v1"
model = "gpt-6-astra"
"#,
    )?;
    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let before = get_config(&client, &addr).await;
    let response = client
        .get(format!(
            "http://{addr}/v1/providers/custom/models?model_provider_id=second"
        ))
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await?;
    assert_eq!(body["model_provider_id"], "second");
    assert_eq!(body["models"].as_array().unwrap().len(), 1);
    assert_eq!(body["models"][0]["id"], "gpt-6-astra");
    assert_eq!(body["models"][0]["reasoning_effort"], "unknown");
    assert_eq!(body["models"][0]["reasoning_effort_levels"], json!([]));
    assert_eq!(
        get_config(&client, &addr).await["provider"],
        before["provider"]
    );
    for path in [
        "custom/models?model_provider_id=",
        "custom/models?model_provider_id=missing",
        "openai-codex/models?model_provider_id=second",
    ] {
        let response = client
            .get(format!("http://{addr}/v1/providers/{path}"))
            .send()
            .await?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    handle.abort();
    Ok(())
}

#[test]
fn provider_catalog_keeps_official_deepseek_facts_but_not_custom_proxy_claims() {
    for official_base_url in [
        "https://api.deepseek.com/v1",
        "https://api.deepseek.com/beta/",
    ] {
        let mut config = Config {
            provider: Some("deepseek".to_string()),
            default_text_model: Some("deepseek-v4-pro".to_string()),
            ..Config::default()
        };
        let provider_config = config
            .provider_config_for_mut(&config.test_identity_for_kind(ProviderKind::Deepseek))
            .unwrap();
        provider_config.base_url = Some(official_base_url.to_string());
        provider_config.model = Some("deepseek-v4-pro".to_string());

        assert!(
            !config.provider_uses_custom_endpoint(
                &config.test_identity_for_kind(ProviderKind::Deepseek)
            ),
            "official DeepSeek endpoint must retain the shared model catalog: {official_base_url}"
        );
        let models = provider_models_for_api(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Deepseek),
        );
        assert!(
            models
                .iter()
                .any(|model| model == "deepseek-v4-flash-vision-exp"),
            "official DeepSeek endpoint must expose the experimental vision model: {official_base_url}"
        );
    }

    let mut custom = Config {
        provider: Some("deepseek".to_string()),
        default_text_model: Some("private-deepseek-deployment".to_string()),
        ..Config::default()
    };
    let provider_config = custom
        .provider_config_for_mut(&custom.test_identity_for_kind(ProviderKind::Deepseek))
        .unwrap();
    provider_config.base_url = Some("https://deepseek-proxy.example.test/v1".to_string());
    provider_config.model = Some("private-deepseek-deployment".to_string());

    assert!(
        custom
            .provider_uses_custom_endpoint(&custom.test_identity_for_kind(ProviderKind::Deepseek))
    );
    assert_eq!(
        provider_models_for_api(
            &custom,
            &(custom).test_identity_for_kind(ProviderKind::Deepseek)
        ),
        vec!["private-deepseek-deployment".to_string()],
        "a real custom endpoint must expose only its explicitly configured model namespace"
    );

    custom.default_text_model = Some("deepseek-v4-flash-vision-exp".to_string());
    custom
        .provider_config_for_mut(&custom.test_identity_for_kind(ProviderKind::Deepseek))
        .unwrap()
        .model = Some("deepseek-v4-flash-vision-exp".to_string());
    assert_eq!(
        provider_models_for_api(
            &custom,
            &(custom).test_identity_for_kind(ProviderKind::Deepseek)
        ),
        vec!["deepseek-v4-flash-vision-exp".to_string()],
        "a custom proxy may legitimately reuse a first-party model id"
    );
    assert_eq!(
        provider_model_image_input_for_api(
            &custom,
            &(custom).test_identity_for_kind(ProviderKind::Deepseek),
            "deepseek-v4-flash-vision-exp",
        ),
        codewhale_config::route::CapabilityState::Unknown,
        "same-name custom proxy must not surface first-party vision capability as verified"
    );
}

#[test]
fn provider_credential_state_wire_values_are_stable_and_sanitized() {
    use crate::provider_readiness::CredentialState;

    for (internal, expected) in [
        (CredentialState::Saved, "configured"),
        (CredentialState::ImportedToken, "configured"),
        (CredentialState::MissingLogin, "login_required"),
        (CredentialState::ExternalConsent, "login_required"),
        (CredentialState::MissingKey, "missing"),
        (CredentialState::NoAuth, "no_auth"),
        (CredentialState::Local, "local"),
        (CredentialState::Legacy, "legacy"),
    ] {
        assert_eq!(
            serde_json::to_value(ProviderCredentialState::from(internal))
                .expect("provider credential state should serialize"),
            expected,
        );
    }
}

#[tokio::test]
async fn operate_api_uses_saved_route_for_start_patch_and_keepalive_readiness() -> Result<()> {
    let _env = lock_test_env();
    let root = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("CODEWHALE_HOME", root.path());
    let _operate = EnvVarGuard::set("CODEWHALE_OPERATE_DIR", root.path().join("operate"));
    let _cli = EnvVarGuard::remove("CODEWHALE_CLI_API_KEY");
    let _missing = EnvVarGuard::remove("CW_OPERATE_MISSING_TEST_KEY");
    let mut config: Config = toml::from_str(
        r#"
provider = "fleet-route"
[providers.fleet-route]
kind = "openai-compatible"
base_url = "https://fleet.example.test/v1"
model = "fleet-model"
auth_mode = "none"
[providers.saved-route]
kind = "openai-compatible"
base_url = "https://saved.example.test/v1"
model = "saved-model"
auth_mode = "api-key"
api_key_env = "CW_OPERATE_MISSING_TEST_KEY"
"#,
    )?;
    config.fleet_operator_route_applied = true;
    let (addr, _runtime, server) =
        spawn_test_server_with_root_token_mobile_workspace_and_overrides(
            root.path().to_path_buf(),
            root.path().join("sessions"),
            None,
            false,
            root.path().join("workspace"),
            TestServerOverrides {
                config: Some(config),
                ..Default::default()
            },
        )
        .await?
        .context("loopback server is required for Operate API acceptance")?;
    let client = crate::tls::reqwest_client();
    let url = format!("http://{addr}/v1/operate");
    let started: serde_json::Value = client
        .post(&url)
        .json(&json!({"direction":"route fixture"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(started["operation"]["credentialsPresent"], true);
    assert_eq!(started["operation"]["leadOperator"]["model"], "fleet-model");
    let manager = AutomationManager::open_for_test(root.path().join("automations"))?;
    let mut record = manager.get_automation(crate::operate::OPERATE_KEEPALIVE_ID)?;
    assert_eq!(record.model.as_deref(), Some("fleet-model"));
    assert_eq!(record.model_provider.as_deref(), Some("custom"));
    assert_eq!(record.model_provider_id.as_deref(), Some("fleet-route"));
    assert_eq!(record.auto_approve, Some(false));

    record.model = Some("saved-model".into());
    record.model_provider_id = Some("saved-route".into());
    manager.save_automation(&record)?;
    let restarted: serde_json::Value = client
        .post(&url)
        .json(&json!({"direction":"saved fixture"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(restarted["operation"]["credentialsPresent"], false);
    assert_eq!(
        restarted["operation"]["leadOperator"]["model"],
        "saved-model"
    );
    let patched: serde_json::Value = client
        .patch(&url)
        .json(&json!({"direction":"changed direction"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(patched["operation"]["credentialsPresent"], false);
    assert_eq!(patched["operation"]["leadOperator"]["model"], "saved-model");
    let observed: serde_json::Value = client
        .post(format!("{url}/keepalive"))
        .json(&json!({"credentialsPresent":true}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        observed["operation"]["credentialsPresent"], true,
        "explicit observation retained"
    );
    let inferred: serde_json::Value = client
        .post(format!("{url}/keepalive"))
        .json(&json!({}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        inferred["operation"]["credentialsPresent"], false,
        "fallback checks saved route, not ready parent"
    );

    record.model = Some("auto".into());
    manager.save_automation(&record)?;
    let auto: serde_json::Value = client
        .post(&url)
        .json(&json!({"direction":"auto fixture"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(auto["operation"]["leadOperator"]["model"], "auto");
    assert_eq!(
        manager.get_automation(&record.id)?.model.as_deref(),
        Some("auto")
    );
    record.model_provider_id = Some("removed-route".into());
    manager.save_automation(&record)?;
    let before = serde_json::to_value(manager.get_automation(&record.id)?)?;
    let operation_before = fs::read(root.path().join("operate/current.json"))?;
    let rejected = client
        .post(&url)
        .json(&json!({"direction":"must not apply"}))
        .send()
        .await?;
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        serde_json::to_value(manager.get_automation(&record.id)?)?,
        before
    );
    assert_eq!(
        fs::read(root.path().join("operate/current.json"))?,
        operation_before
    );
    assert!(
        manager.list_runs(&record.id, None)?.is_empty(),
        "API installation did not execute a provider task"
    );
    server.abort();
    Ok(())
}

#[tokio::test]
async fn provider_catalog_reports_exact_named_custom_credential_state_without_secrets() -> Result<()>
{
    let _env_lock = lock_test_env();
    let _key_source = EnvVarGuard::remove("DEEPSEEK_API_KEY_SOURCE");
    let _cli_key = EnvVarGuard::remove("CODEWHALE_CLI_API_KEY");
    let root = std::env::temp_dir().join(format!(
        "codewhale-provider-credential-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&root)?;

    let cases = [
        (
            "private-gateway",
            "configured",
            r#"provider = "private-gateway"

[providers.private-gateway]
kind = "openai-compatible"
base_url = "https://secret-route.example.test/tenant/private/v1"
model = "private-model"
api_key = "sk-provider-fixture-do-not-leak"
api_key_env = "CODEWHALE_PROVIDER_FIXTURE_ENV"
"#,
        ),
        (
            "missing-gateway",
            "missing",
            r#"provider = "missing-gateway"

[providers.missing-gateway]
kind = "openai-compatible"
base_url = "https://missing-route.example.test/private/v1"
model = "missing-model"
"#,
        ),
        (
            "local-runtime",
            "local",
            r#"provider = "local-runtime"

[providers.local-runtime]
kind = "openai-compatible"
base_url = "http://127.0.0.1:18190/private/v1"
model = "local-model"
"#,
        ),
    ];

    for (route_id, expected_state, config) in cases {
        let config_file = root.join(format!("{route_id}.toml"));
        fs::write(&config_file, config)?;
        let Some((addr, _runtime_threads, handle)) =
            spawn_test_server_with_config_path(config_file).await?
        else {
            return Ok(());
        };
        let providers = get_providers(&crate::tls::reqwest_client(), &addr).await;
        assert_eq!(providers["current"], "custom");
        let custom = providers["providers"]
            .as_array()
            .and_then(|entries| entries.iter().find(|entry| entry["id"] == "custom"))
            .context("custom provider entry")?;
        assert_eq!(
            custom["model_provider_id"], route_id,
            "credential state must describe the exact active named custom route"
        );
        assert_eq!(custom["credentialState"], expected_state);

        // An allow-list, not a snapshot: a new field here has to be argued for
        // in this test before it can reach the wire. `credentialSource` and
        // `credentialWritable` (#6179) are admissible because they are a
        // *class* and a boolean — the source is one of four fixed enum
        // spellings and can never interpolate a value, a path, or an
        // environment variable name. `credentialWritableReason` is
        // `skip_serializing_if = Option::is_none`, so it is absent for every
        // writable route and is asserted separately below when present.
        let allowed_fields: std::collections::BTreeSet<_> = [
            "credentialSource",
            "credentialState",
            "credentialWritable",
            "credentialWritableReason",
            "default_model",
            "display_name",
            "has_model_catalog",
            "id",
            "model_provider_id",
        ]
        .into_iter()
        .collect();
        const CREDENTIAL_SOURCES: [&str; 4] = ["secret_store", "config", "external_auth", "none"];
        for entry in providers["providers"]
            .as_array()
            .context("providers array")?
        {
            let object = entry.as_object().context("provider entry object")?;
            let actual_fields: std::collections::BTreeSet<_> =
                object.keys().map(String::as_str).collect();
            assert!(
                actual_fields.is_subset(&allowed_fields),
                "provider catalog must remain a non-secret projection: {actual_fields:?}"
            );
            // The source is a closed vocabulary. If it ever stops being one,
            // it has become a place a value can hide.
            let source = object["credentialSource"]
                .as_str()
                .context("credentialSource must be a string")?;
            assert!(
                CREDENTIAL_SOURCES.contains(&source),
                "credentialSource must stay a fixed class, got {source:?}"
            );
            assert!(object["credentialWritable"].is_boolean());
        }

        let serialized = serde_json::to_string(&providers)?;
        for forbidden in [
            "sk-provider-fixture-do-not-leak",
            "CODEWHALE_PROVIDER_FIXTURE_ENV",
            "secret-route.example.test",
            "missing-route.example.test",
            "127.0.0.1:18190",
            "default_base_url",
            "env_vars",
            "api_key",
            "api_key_env",
        ] {
            assert!(
                !serialized.contains(forbidden),
                "provider catalog leaked forbidden detail: {forbidden}"
            );
        }

        handle.abort();
    }

    Ok(())
}

#[tokio::test]
async fn provider_catalog_preserves_named_custom_identity_for_new_threads() -> Result<()> {
    let root = std::env::temp_dir().join(format!(
        "codewhale-provider-named-custom-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&root)?;
    let config_file = root.join("custom-config.toml");
    fs::write(
        &config_file,
        r#"provider = "lm-studio"

[providers.lm-studio]
kind = "openai-compatible"
base_url = "http://127.0.0.1:18190/v1"
model = "local-vision-model"
api_key = "local-test-key"
"#,
    )?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let config_before = get_config(&client, &addr).await;
    assert_eq!(config_before["provider"], "lm-studio");
    assert_eq!(config_before["model"], "local-vision-model");

    let providers = get_providers(&client, &addr).await;
    assert_eq!(providers["current"], "custom");
    let custom = providers["providers"]
        .as_array()
        .and_then(|entries| entries.iter().find(|entry| entry["id"] == "custom"))
        .context("custom provider entry")?;
    assert_eq!(custom["model_provider_id"], "lm-studio");
    assert_eq!(custom["default_model"], "local-vision-model");

    let response = client
        .post(format!("http://{addr}/v1/threads"))
        .json(&json!({
            "model_provider": "custom",
            "model_provider_id": custom["model_provider_id"],
            "model": custom["default_model"],
        }))
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::CREATED);
    let thread: serde_json::Value = response.json().await?;
    assert_eq!(thread["model_provider"], "custom");
    assert_eq!(thread["model_provider_id"], "lm-studio");
    assert_eq!(thread["model"], "local-vision-model");

    let config_after = get_config(&client, &addr).await;
    assert_eq!(config_after["provider"], config_before["provider"]);
    assert_eq!(config_after["model"], config_before["model"]);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn provider_catalog_lists_configured_named_custom_routes() -> Result<()> {
    // A user-defined `[providers.<name>]` route is first-class in the TUI
    // picker and in this API's own model catalog, but `GET /v1/providers`
    // walked only the built-in enum — so a route configured in the TUI never
    // reached the GUI provider picker at all. This pins the whole path a GUI
    // client needs: the route is listed as (generic kind, exact id), the pair
    // it reports is the pair the switch endpoint accepts, and switching to it
    // persists the route name the config actually selects.
    let root = std::env::temp_dir().join(format!(
        "codewhale-provider-named-custom-route-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&root)?;
    let config_file = root.join("custom-config.toml");
    fs::write(
        &config_file,
        r#"provider = "deepseek"
default_text_model = "deepseek-v4-pro"

[providers.deepseek]
api_key = "fixture-key"

[providers.bigmodel-cn]
kind = "openai-compatible"
base_url = "https://open.bigmodel.cn/api/paas/v4"
api_key = "fixture-key"
model = "glm-5.3"

[providers.not-a-routable-route]
base_url = "http://127.0.0.1:18191/v1"
"#,
    )?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let providers = get_providers(&client, &addr).await;
    assert_eq!(providers["current"], "deepseek");
    assert_eq!(
        providers["current_provider_id"], "deepseek",
        "the active route's exact id travels with the generic one"
    );

    let catalog = providers["providers"]
        .as_array()
        .context("providers array")?;
    let route_entries: Vec<_> = catalog
        .iter()
        .filter(|entry| entry["model_provider_id"] == "bigmodel-cn")
        .collect();
    assert_eq!(
        route_entries.len(),
        1,
        "a configured route must be listed exactly once: {providers}"
    );
    let route = route_entries[0];
    assert_eq!(route["id"], "custom");
    assert_eq!(route["display_name"], "bigmodel-cn (custom)");
    assert_eq!(route["default_model"], "glm-5.3");
    assert_eq!(
        route["has_model_catalog"], true,
        "the flag must describe what this route's own model endpoint returns"
    );
    let unavailable: Vec<_> = catalog
        .iter()
        .filter(|entry| entry["model_provider_id"] == "not-a-routable-route")
        .collect();
    assert_eq!(unavailable.len(), 1);
    assert_eq!(
        unavailable[0]["display_name"],
        "not-a-routable-route (unavailable)"
    );
    assert_eq!(unavailable[0]["has_model_catalog"], false);
    assert_eq!(unavailable[0]["credentialState"], "legacy");
    assert_eq!(unavailable[0]["credentialWritable"], false);
    // Every admitted built-in carries its own exact id, including inactive
    // entries. None may borrow the selected route's id.
    for entry in catalog {
        if entry["id"] != "deepseek" && entry["id"] != "custom" {
            assert_eq!(
                entry["model_provider_id"], entry["id"],
                "an inactive provider must carry its own exact id: {entry}"
            );
        }
    }
    let unavailable_models = client
        .get(format!(
            "http://{addr}/v1/providers/custom/models?model_provider_id=not-a-routable-route"
        ))
        .send()
        .await?;
    assert_eq!(unavailable_models.status(), StatusCode::BAD_REQUEST);

    // The route's model catalog is reachable with the identity the catalog
    // just published — no other spelling is needed by a client.
    let models = get_provider_models_with_identity(&client, &addr, "custom", "bigmodel-cn").await;
    let model_ids: Vec<_> = models["models"]
        .as_array()
        .context("models array")?
        .iter()
        .filter_map(|entry| entry["id"].as_str())
        .collect();
    assert_eq!(model_ids, vec!["glm-5.3"]);

    // Switching with that same pair selects and persists the route.
    let (status, body) = post_switch_provider(
        &client,
        &addr,
        "custom",
        &json!({ "model_provider_id": "bigmodel-cn" }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "switch should succeed, body: {body}"
    );
    assert_eq!(body["provider"], "bigmodel-cn");
    assert_eq!(body["model"], "glm-5.3");

    let persisted = fs::read_to_string(&config_file)?;
    assert!(
        persisted.contains("provider = \"bigmodel-cn\""),
        "the selected route name is what the config selects. Actual config:\n{persisted}"
    );

    // Now active, the route is still listed once — as the active entry rather
    // than duplicated beside it — and it keeps naming itself: an active route
    // that fell back to the unconfigured `custom` placeholder would show the
    // user a name they never chose and tell a client its catalog is empty.
    let providers = get_providers(&client, &addr).await;
    assert_eq!(providers["current"], "custom");
    assert_eq!(providers["current_provider_id"], "bigmodel-cn");
    let catalog = providers["providers"]
        .as_array()
        .context("providers array")?;
    let active_entries: Vec<_> = catalog
        .iter()
        .filter(|entry| entry["model_provider_id"] == "bigmodel-cn")
        .collect();
    assert_eq!(
        active_entries.len(),
        1,
        "the active route is listed once: {providers}"
    );
    assert_eq!(active_entries[0]["display_name"], "bigmodel-cn (custom)");
    assert_eq!(active_entries[0]["has_model_catalog"], true);
    assert_eq!(active_entries[0]["default_model"], "glm-5.3");

    // The config-key path the GUI's config panel uses accepts the same
    // route name, because that is what the persistent `provider` key holds.
    let (status, body) = post_set_config(&client, &addr, "provider", "bigmodel-cn", true).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "set provider should succeed: {body}"
    );
    let config_after = get_config(&client, &addr).await;
    assert_eq!(config_after["provider"], "bigmodel-cn");

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn switch_provider_names_a_custom_route_instead_of_denying_it() -> Result<()> {
    // Addressing a configured route by its own name in the path is the obvious
    // mistake; the refusal must point at the pair that works rather than claim
    // the route does not exist.
    let root = std::env::temp_dir().join(format!(
        "codewhale-switch-named-route-hint-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&root)?;
    let config_file = root.join("custom-config.toml");
    fs::write(
        &config_file,
        r#"provider = "deepseek"

[providers.bigmodel-cn]
kind = "openai-compatible"
base_url = "https://open.bigmodel.cn/api/paas/v4"
model = "glm-5.3"
"#,
    )?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let (status, body) = post_switch_provider(&client, &addr, "bigmodel-cn", &json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("bigmodel-cn") && message.contains("model_provider_id"),
        "the refusal must name the working pair, got: {message}"
    );

    // A pair that contradicts itself fails closed rather than picking one half:
    // the path says generic `custom`, the body names a built-in route.
    let (status, body) = post_switch_provider(
        &client,
        &addr,
        "custom",
        &json!({ "model_provider_id": "deepseek" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("custom") && message.contains("deepseek"),
        "a contradictory pair must be refused by name, got: {message}"
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn set_config_base_url_writes_the_field_the_active_route_reads() -> Result<()> {
    // `GET /v1/config` reports the active route's endpoint as `base_url`, so a
    // client reads it and writes it back. The write used to land in a root
    // `active_route_base_url` key that no reader resolves: it reported success,
    // the file gained a dead key, and the endpoint never moved. Each route shape
    // owns exactly one endpoint slot, and the write has to find it.
    let root = std::env::temp_dir().join(format!("codewhale-config-base-url-{}", Uuid::new_v4()));
    fs::create_dir_all(&root)?;
    let config_file = root.join("config.toml");
    fs::write(
        &config_file,
        r#"provider = "volcengine"
base_url = "http://127.0.0.1:1/v1"

[providers.volcengine]
api_key = "fixture-key"
base_url = "http://127.0.0.1:2/v1"
model = "glm-5.2"
"#,
    )?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let before = get_config(&client, &addr).await;
    assert_eq!(
        before["base_url"], "http://127.0.0.1:2/v1",
        "a built-in reports its own table's endpoint"
    );

    let (status, body) = post_set_config(
        &client,
        &addr,
        "base_url",
        "https://moved.example.test/v1",
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "write should succeed, body: {body}");
    let resp = client
        .post(format!("http://{addr}/v1/config/reload"))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::OK);

    let after = get_config(&client, &addr).await;
    assert_eq!(
        after["base_url"], "https://moved.example.test/v1",
        "the written value is what the route now reads"
    );
    let persisted = fs::read_to_string(&config_file)?;
    assert!(
        persisted.contains("[providers.volcengine]"),
        "the built-in keeps its endpoint in its own table:\n{persisted}"
    );
    assert!(
        persisted.contains("https://moved.example.test/v1"),
        "the endpoint moved on disk:\n{persisted}"
    );
    assert!(
        !persisted.contains("active_route_base_url"),
        "no dead key may be written:\n{persisted}"
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn set_config_base_url_moves_the_root_route_endpoint() -> Result<()> {
    // DeepSeek owns the root slot; writing it must be visible to the same read
    // that reports it, and to nothing else.
    let root =
        std::env::temp_dir().join(format!("codewhale-config-base-url-root-{}", Uuid::new_v4()));
    fs::create_dir_all(&root)?;
    let config_file = root.join("config.toml");
    fs::write(
        &config_file,
        "provider = \"deepseek\"\napi_key = \"fixture-key\"\nbase_url = \"http://127.0.0.1:1/v1\"\n",
    )?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let (status, body) = post_set_config(
        &client,
        &addr,
        "base_url",
        "https://root-moved.example.test/v1",
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "write should succeed, body: {body}");
    client
        .post(format!("http://{addr}/v1/config/reload"))
        .send()
        .await?;

    let after = get_config(&client, &addr).await;
    assert_eq!(after["base_url"], "https://root-moved.example.test/v1");
    let persisted = fs::read_to_string(&config_file)?;
    assert!(
        persisted.contains("base_url = \"https://root-moved.example.test/v1\""),
        "the root route keeps its endpoint at the root:\n{persisted}"
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn set_config_base_url_refuses_a_named_route_with_guidance() -> Result<()> {
    // Keep the existing selector while adopting the canonical named-table
    // writer: the endpoint moves only in that exact table, after reload.
    let root = std::env::temp_dir().join(format!(
        "codewhale-config-base-url-custom-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&root)?;
    let config_file = root.join("config.toml");
    fs::write(
        &config_file,
        r#"provider = "bigmodel-cn"

[providers.bigmodel-cn]
kind = "openai-compatible"
base_url = "https://open.bigmodel.cn/api/paas/v4"
api_key = "fixture-key"
model = "glm-5.3"
"#,
    )?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let (status, body) = post_set_config(
        &client,
        &addr,
        "base_url",
        "https://moved.example.test/v1",
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "named-table write should succeed");
    assert_eq!(body["persisted"], true);
    assert_eq!(body["requires_reload"], true);
    let before_reload = get_config(&client, &addr).await;
    assert_eq!(
        before_reload["base_url"],
        "https://open.bigmodel.cn/api/paas/v4"
    );
    let persisted = fs::read_to_string(&config_file)?;
    let saved: toml::Value = toml::from_str(&persisted)?;
    assert_eq!(
        saved["providers"]["bigmodel-cn"]["base_url"].as_str(),
        Some("https://moved.example.test/v1")
    );
    assert_eq!(
        saved["providers"]["bigmodel-cn"]["model"].as_str(),
        Some("glm-5.3")
    );
    assert!(saved.get("base_url").is_none());
    assert!(
        !persisted.contains("active_route_base_url"),
        "no dead endpoint key may be written"
    );
    let (status, _) = post_set_config(&client, &addr, "provider", "missing-route", true).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(fs::read_to_string(&config_file)?, persisted);
    let reload = client
        .post(format!("http://{addr}/v1/config/reload"))
        .send()
        .await?;
    assert_eq!(reload.status(), StatusCode::OK);
    let after_reload = get_config(&client, &addr).await;
    assert_eq!(after_reload["provider"], "bigmodel-cn");
    assert_eq!(after_reload["base_url"], "https://moved.example.test/v1");

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn reload_config_reads_from_config_path_and_updates_in_memory_state() -> Result<()> {
    // Fix #2 + reload behavior: This test proves that reload reads from the
    // `--config` path (not default discovery) and actually updates the
    // in-memory state visible to GET /v1/config.
    //
    // If Fix #2 is reverted (reload uses Config::load(None, None) instead of
    // state.config_path), the reload will read an empty/default config and
    // the persisted value will NOT appear in GET /v1/config → test fails.
    let root =
        std::env::temp_dir().join(format!("codewhale-config-reload-path-{}", Uuid::new_v4()));
    fs::create_dir_all(&root)?;
    let config_file = root.join("custom-config.toml");
    fs::write(&config_file, "# initial\n")?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    // Step 1: Record initial model value (should be the default, since
    // Config::default() has default_text_model = None).
    let before = get_config(&client, &addr).await;
    let initial_model = before["model"].as_str().unwrap_or_default().to_string();
    assert!(
        !initial_model.is_empty(),
        "initial model should not be empty"
    );
    // The initial subagents_max_depth should be DEFAULT_SPAWN_DEPTH (3)
    // since Config::default() has no subagents config.
    let initial_depth = before["subagents_max_depth"]
        .as_u64()
        .expect("subagents_max_depth should be a number");
    assert_eq!(
        initial_depth,
        u64::from(codewhale_config::DEFAULT_SPAWN_DEPTH),
        "initial subagents_max_depth should be DEFAULT_SPAWN_DEPTH"
    );

    // Step 2: Persist a new model value to the config file.
    // set_config must NOT mutate in-memory state (by design — the caller
    // must call /v1/config/reload to apply changes).
    // Use a valid DeepSeek model ID so Config::validate() doesn't reject
    // the reloaded config.
    let test_model = "deepseek-v4-flash";
    let (status, body) = post_set_config(&client, &addr, "model", test_model, true).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "set_config should succeed, body: {body}"
    );

    // Step 3: Verify in-memory state is NOT mutated by set_config alone.
    let after_set = get_config(&client, &addr).await;
    assert_eq!(
        after_set["model"].as_str().unwrap_or_default(),
        initial_model,
        "set_config must NOT update in-memory state before reload"
    );

    // Step 4: Also persist subagents_max_depth = 5 (below ceiling of 8).
    let (status, body) = post_set_config(&client, &addr, "subagents_max_depth", "5", true).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    // Step 5: Reload — this must read from config_file (not default discovery).
    let reload_resp = client
        .post(format!("http://{addr}/v1/config/reload"))
        .send()
        .await?;
    assert_eq!(reload_resp.status(), StatusCode::OK);

    // Step 6: Verify in-memory state IS now updated after reload.
    let after_reload = get_config(&client, &addr).await;

    // Model should reflect the persisted value.
    assert_eq!(
        after_reload["model"].as_str().unwrap_or_default(),
        test_model,
        "after reload, model should be the persisted value — \
         if this fails, reload is not reading from config_path"
    );

    // subagents_max_depth should reflect the persisted value (5).
    assert_eq!(
        after_reload["subagents_max_depth"].as_u64(),
        Some(5),
        "after reload, subagents_max_depth should be 5"
    );

    handle.abort();
    Ok(())
}

// ---------------------------------------------------------------------------
// POST /v1/providers/{id}/switch endpoint tests
//
// These tests pin down the TUI-parity contract for the GUI's provider
// picker: a bare switch (no model arg) MUST NOT overwrite the user's
// `[providers.<id>].model` config. Regression for the bug where clicking
// volcengine in the picker forced `model = "deepseek-v4-pro"` even when
// the user had configured `model = "glm-2"`.
// ---------------------------------------------------------------------------

/// Helper: POST to `/v1/providers/{id}/switch` and return the response
/// status + body JSON.
async fn post_switch_provider(
    client: &reqwest::Client,
    addr: &SocketAddr,
    provider: &str,
    body: &serde_json::Value,
) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = client
        .post(format!("http://{addr}/v1/providers/{provider}/switch"))
        .json(body)
        .send()
        .await
        .expect("POST /v1/providers/{id}/switch should not fail at transport level");
    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .await
        .unwrap_or_else(|_| serde_json::json!({"_error": "non-json response body"}));
    (status, body)
}

#[tokio::test]
async fn switch_provider_without_model_arg_preserves_user_per_provider_model() -> Result<()> {
    // Regression: clicking volcengine in the GUI picker used to send
    // `POST /v1/config { key: "model", value: "deepseek-v4-pro" }` (the
    // catalog default), clobbering the user's `[providers.volcengine].model
    // = "glm-2"`. The new /v1/providers/{id}/switch endpoint MUST NOT
    // touch the model key when no model arg is provided — mirroring the
    // TUI's `/provider volcengine` (model: None) flow in
    // `commands/groups/core/provider.rs` + `tui/ui.rs::switch_provider`.
    let root = std::env::temp_dir().join(format!("codewhale-switch-no-model-{}", Uuid::new_v4()));
    fs::create_dir_all(&root)?;
    let config_file = root.join("custom-config.toml");
    fs::write(
        &config_file,
        r#"provider = "deepseek"
default_text_model = "deepseek-v4-pro"

[providers.volcengine]
api_key = "ark-test"
base_url = "https://ark.cn-beijing.volces.com/api/plan/v3"
model = "glm-2"
"#,
    )?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    // Switch to volcengine WITHOUT a model arg — simulates a picker click.
    let (status, body) =
        post_switch_provider(&client, &addr, "volcengine", &serde_json::json!({})).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "switch should succeed, body: {body}"
    );

    // Response must report the user's configured model, NOT the catalog
    // default "deepseek-v4-pro".
    assert_eq!(
        body["provider"].as_str(),
        Some("volcengine"),
        "response should echo the switched-to provider"
    );
    assert_eq!(
        body["model"].as_str(),
        Some("glm-2"),
        "resolved model must be the user's `[providers.volcengine].model`, \
         not the catalog default — if this fails the switch endpoint is \
         clobbering per-provider config"
    );

    // The config file on disk must NOT contain a `model = "deepseek-v4-pro"`
    // override for volcengine — `glm-2` must be preserved verbatim.
    let persisted = fs::read_to_string(&config_file)?;
    assert!(
        persisted.contains("model = \"glm-2\""),
        "user's `[providers.volcengine].model = \"glm-2\"` must be preserved on disk. \
         Actual config:\n{persisted}"
    );
    assert!(
        !persisted
            .matches("model = \"deepseek-v4-pro\"")
            .count()
            .ge(&2),
        "switch must not add a second `model = \"deepseek-v4-pro\"` line for volcengine. \
         Actual config:\n{persisted}"
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn switch_provider_rolls_back_a_selection_the_runtime_cannot_apply() -> Result<()> {
    // The reload after the write fails (the runtime's profile is not in the
    // file), so the switch is not applied in memory: the file must not keep
    // it either, or the next start silently adopts a provider the caller was
    // told failed.
    let root = std::env::temp_dir().join(format!("codewhale-switch-rollback-{}", Uuid::new_v4()));
    fs::create_dir_all(&root)?;
    let config_file = root.join("custom-config.toml");
    fs::write(
        &config_file,
        r#"provider = "deepseek"
default_text_model = "deepseek-v4-pro"

[providers.volcengine]
api_key = "ark-test"
base_url = "https://ark.cn-beijing.volces.com/api/plan/v3"
model = "glm-2"
"#,
    )?;
    let before = fs::read_to_string(&config_file)?;

    let Some((addr, _runtime_threads, handle)) = spawn_test_server_with_config_path_and_profile(
        config_file.clone(),
        "absent-profile".to_string(),
    )
    .await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let (status, body) =
        post_switch_provider(&client, &addr, "volcengine", &serde_json::json!({})).await;
    assert!(!status.is_success(), "the switch cannot apply: {body}");
    assert_eq!(
        fs::read_to_string(&config_file)?,
        before,
        "a switch the runtime refused must not stay persisted"
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn switch_provider_with_explicit_model_arg_persists_model() -> Result<()> {
    // When the user explicitly chooses a model (e.g. `/provider volcengine
    // glm-2.5` or a model-picker selection), the switch endpoint MUST
    // persist that model — mirroring `switch_provider`'s
    // `if model_override.is_some()` branch (ui.rs:9400-9405).
    let root = std::env::temp_dir().join(format!("codewhale-switch-with-model-{}", Uuid::new_v4()));
    fs::create_dir_all(&root)?;
    let config_file = root.join("custom-config.toml");
    fs::write(
        &config_file,
        r#"provider = "deepseek"
default_text_model = "deepseek-v4-pro"

[providers.volcengine]
api_key = "ark-test"
base_url = "https://ark.cn-beijing.volces.com/api/plan/v3"
model = "glm-2"
"#,
    )?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    // Switch to volcengine WITH an explicit model arg.
    let (status, body) = post_switch_provider(
        &client,
        &addr,
        "volcengine",
        &serde_json::json!({ "model": "deepseek-v4-flash" }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "switch with explicit model should succeed, body: {body}"
    );

    // The persisted config must reflect the explicit override.
    let persisted = fs::read_to_string(&config_file)?;
    assert!(
        persisted.contains("model = \"deepseek-v4-flash\""),
        "explicit model arg must be persisted to `[providers.volcengine].model`. \
         Actual config:\n{persisted}"
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn switch_provider_rejects_unknown_provider_id() -> Result<()> {
    let root = std::env::temp_dir().join(format!("codewhale-switch-unknown-{}", Uuid::new_v4()));
    fs::create_dir_all(&root)?;
    let config_file = root.join("custom-config.toml");
    fs::write(&config_file, "# empty\n")?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let (status, _body) = post_switch_provider(
        &client,
        &addr,
        "not-a-real-provider",
        &serde_json::json!({}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "unknown provider id should return 400"
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn switch_provider_rejects_legacy_deepseek_cn_alias() -> Result<()> {
    // The legacy `deepseek-cn` alias has no ProviderKind metadata; the
    // GUI must use `deepseek` instead. Same guard as list_provider_models.
    let root = std::env::temp_dir().join(format!("codewhale-switch-cn-alias-{}", Uuid::new_v4()));
    fs::create_dir_all(&root)?;
    let config_file = root.join("custom-config.toml");
    fs::write(&config_file, "# empty\n")?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let (status, body) =
        post_switch_provider(&client, &addr, "deepseek-cn", &serde_json::json!({})).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "deepseek-cn should be rejected, body: {body}"
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn switch_provider_with_deepseek_and_explicit_model_preserves_root_fallback() -> Result<()> {
    // Switching TO DeepSeek with an explicit model persists that model on the
    // canonical `[providers.deepseek].model` leaf, and the incoming route
    // resolves it from there.
    //
    // The fixture also carries a root `default_text_model` left over from the
    // outgoing Volcengine route. `Config::validate` checks the *selected*
    // route's model, so that alias is inert under DeepSeek: the write neither
    // overwrites it nor is invalidated by it, and the file still loads. It is
    // saved state, and deleting it to satisfy validation of a value nothing
    // resolves would be data loss.
    let root = std::env::temp_dir().join(format!(
        "codewhale-switch-deepseek-model-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&root)?;
    let config_file = root.join("custom-config.toml");
    fs::write(
        &config_file,
        r#"provider = "volcengine"
default_text_model = "old-model"

[providers.volcengine]
api_key = "ark-test"
base_url = "https://ark.cn-beijing.volces.com/api/plan/v3"
model = "glm-2"
"#,
    )?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    // Switch to deepseek WITH an explicit model override.
    let (status, body) = post_switch_provider(
        &client,
        &addr,
        "deepseek",
        &serde_json::json!({ "model": "deepseek-v4-pro" }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "switch to deepseek with model should succeed, body: {body}"
    );

    assert_eq!(body["provider"], "deepseek");
    assert_eq!(body["model"], "deepseek-v4-pro");

    let persisted = fs::read_to_string(&config_file)?;
    let saved: toml::Value = toml::from_str(&persisted)?;
    assert_eq!(
        saved["provider"].as_str(),
        Some("deepseek"),
        "provider should be persisted as deepseek. Actual config:\n{persisted}"
    );
    assert_eq!(
        saved["default_text_model"].as_str(),
        Some("old-model"),
        "an inert root fallback is saved state and must survive the switch. \
         Actual config:\n{persisted}"
    );
    assert_eq!(
        saved["providers"]["deepseek"]["model"].as_str(),
        Some("deepseek-v4-pro"),
        "the explicit model belongs on DeepSeek's canonical leaf. \
         Actual config:\n{persisted}"
    );
    assert_eq!(
        saved["providers"]["volcengine"]["model"].as_str(),
        Some("glm-2"),
        "the outgoing route keeps its own saved model. Actual config:\n{persisted}"
    );
    // The receipt that matters: what was written still loads under the new
    // route, and resolves the selected leaf rather than the root fallback.
    // Validating the root alias instead of the selection returned 500 here.
    let reloaded = Config::load(Some(config_file.clone()), None)?;
    assert_eq!(
        reloaded.active_provider_identity().unwrap().provider,
        crate::config::ProviderKind::Deepseek
    );
    assert_eq!(reloaded.default_model(), "deepseek-v4-pro");

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn switch_provider_empty_model_string_treated_as_no_override() -> Result<()> {
    // An empty string model (`{ "model": "" }`) must be treated the same
    // as no model at all — the endpoint should NOT persist a model key,
    // matching the TUI's behavior where a blank model arg is ignored.
    let root =
        std::env::temp_dir().join(format!("codewhale-switch-empty-model-{}", Uuid::new_v4()));
    fs::create_dir_all(&root)?;
    let config_file = root.join("custom-config.toml");
    fs::write(
        &config_file,
        r#"provider = "deepseek"
default_text_model = "deepseek-v4-pro"

[providers.volcengine]
api_key = "ark-test"
base_url = "https://ark.cn-beijing.volces.com/api/plan/v3"
model = "glm-2"
"#,
    )?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let (status, body) = post_switch_provider(
        &client,
        &addr,
        "volcengine",
        &serde_json::json!({ "model": "" }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "switch with empty model should succeed, body: {body}"
    );

    // The user's `model = "glm-2"` must NOT be overwritten.
    let persisted = fs::read_to_string(&config_file)?;
    assert!(
        persisted.contains("model = \"glm-2\""),
        "user's model must be preserved when empty-string model is sent. \
         Actual config:\n{persisted}"
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn switch_provider_persists_provider_key_on_disk() -> Result<()> {
    // Verify that the root `provider = "..."` key is correctly written to
    // the config file on disk, not just in the response body.
    let root =
        std::env::temp_dir().join(format!("codewhale-switch-provider-disk-{}", Uuid::new_v4()));
    fs::create_dir_all(&root)?;
    let config_file = root.join("custom-config.toml");
    fs::write(
        &config_file,
        r#"provider = "deepseek"
default_text_model = "deepseek-v4-pro"

[providers.volcengine]
api_key = "ark-test"
base_url = "https://ark.cn-beijing.volces.com/api/plan/v3"
model = "glm-2"
"#,
    )?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let (status, _body) =
        post_switch_provider(&client, &addr, "volcengine", &serde_json::json!({})).await;
    assert_eq!(status, StatusCode::OK);

    let persisted = fs::read_to_string(&config_file)?;
    assert!(
        persisted.contains("provider = \"volcengine\""),
        "root `provider` key must be updated on disk. Actual config:\n{persisted}"
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn switch_provider_rejected_by_reload_leaves_the_config_file_unchanged() -> Result<()> {
    let root = std::env::temp_dir().join(format!(
        "codewhale-switch-provider-rejected-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&root)?;
    let config_file = root.join("custom-config.toml");
    fs::write(
        &config_file,
        r#"provider = "lm-studio"

[providers.lm-studio]
kind = "openai-compatible"
base_url = "http://127.0.0.1:18181/v1"
model = "local-model"
api_key = "local-test-key"
"#,
    )?;
    let Some((addr, runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file.clone()).await?
    else {
        return Ok(());
    };
    let thread = runtime_threads
        .create_thread(crate::runtime_threads::CreateThreadRequest {
            model: Some("local-model".to_string()),
            model_provider: Some("lm-studio".to_string()),
            ..Default::default()
        })
        .await?;
    let harness = crate::core::engine::mock_engine_handle();
    runtime_threads
        .install_test_engine(&thread.id, harness.handle.clone())
        .await?;

    // The loaded thread's route is removed on disk outside this runtime, so
    // any reload now rejects it.
    let edited = r#"provider = "deepseek"

[providers.volcengine]
api_key = "ark-test"
base_url = "https://ark.cn-beijing.volces.com/api/plan/v3"
model = "glm-2"
"#;
    fs::write(&config_file, edited)?;

    let client = crate::tls::reqwest_client();
    let (status, body) =
        post_switch_provider(&client, &addr, "volcengine", &serde_json::json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body.to_string().contains("Config reload rejected"),
        "{body}"
    );
    assert_eq!(
        fs::read_to_string(&config_file)?,
        edited,
        "a switch the API reported as rejected must not remain in config.toml"
    );

    drop(harness);
    handle.abort();
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn switch_provider_waiting_for_config_lock_keeps_the_async_worker_running() -> Result<()> {
    let root = tempfile::tempdir()?;
    let config_file = root.path().join("config.toml");
    fs::write(&config_file, "provider = \"deepseek\"\n")?;
    let (addr, _runtime_threads, handle) = spawn_test_server_with_config_path(config_file.clone())
        .await?
        .context("loopback Runtime API server must be available")?;

    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let safety_timeout = ci_scaled(Duration::from_secs(3));
    let locker = std::thread::spawn(move || {
        codewhale_config::with_config_write_lock(&config_file, |_| {
            let _ = entered_tx.send(());
            // A reverted synchronous implementation must fail promptly,
            // rather than leave this current-thread runtime deadlocked.
            let _ = release_rx.recv_timeout(safety_timeout);
            Ok(())
        })
    });
    entered_rx.await?;

    let client = crate::tls::reqwest_client();
    let switch_client = client.clone();
    let request = tokio::spawn(async move {
        post_switch_provider(&switch_client, &addr, "deepseek", &json!({})).await
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    // The helper serves on a separate product-stack current-thread runtime.
    // Probe that same server, not a timer on this test's client runtime:
    // a blocking config save must not prevent an unrelated health request.
    let started = std::time::Instant::now();
    let health = client.get(format!("http://{addr}/health")).send().await;
    let elapsed = started.elapsed();
    let was_waiting = !request.is_finished();
    let _ = release_tx.send(());
    locker.join().expect("config lock holder must finish")?;
    let (status, body) = request.await?;
    handle.abort();

    assert!(
        elapsed < ci_scaled(Duration::from_secs(1)),
        "the serving async worker must run while the provider switch waits for a filesystem lock: {elapsed:?}"
    );
    assert_eq!(health?.status(), StatusCode::OK);
    assert!(
        was_waiting,
        "the real switch must await the held config lock"
    );
    assert_eq!(status, StatusCode::OK, "{body}");
    Ok(())
}

#[tokio::test]
async fn switch_provider_that_fails_to_load_removes_the_config_file_it_created() -> Result<()> {
    let root = std::env::temp_dir().join(format!(
        "codewhale-switch-provider-load-failure-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&root)?;
    // No config file yet, and a profile the saved file will not define, so
    // the reload after the save fails to load.
    let config_file = root.join("absent-config.toml");
    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path_and_profile(config_file.clone(), "missing".into())
            .await?
    else {
        return Ok(());
    };

    let client = crate::tls::reqwest_client();
    let (status, body) =
        post_switch_provider(&client, &addr, "deepseek", &serde_json::json!({})).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert!(
        body.to_string().contains("Failed to reload config"),
        "{body}"
    );
    assert!(
        !config_file.exists(),
        "a refused switch must leave no config file behind, not an empty one"
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn zai_model_update_is_provider_scoped_and_preserves_deepseek_fallback() -> Result<()> {
    let root = std::env::temp_dir().join(format!(
        "codewhale-config-zai-model-scope-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&root)?;
    let config_file = root.join("custom-config.toml");
    fs::write(
        &config_file,
        r#"provider = "zai"
default_text_model = "deepseek-v4-pro"

[providers.zai]
api_key = "zai-test-key"
model = "GLM-5.2"
"#,
    )?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let response = client
        .post(format!("http://{addr}/v1/config/reload"))
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::OK);

    let before = get_config(&client, &addr).await;
    assert_eq!(before["provider"], "zai");
    assert_eq!(before["model"], "GLM-5.2");
    assert_eq!(before["default_model"], "deepseek-v4-pro");

    let (status, body) = post_set_config(&client, &addr, "model", "glm-5-turbo", true).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["value"], "GLM-5-Turbo");

    let persisted = fs::read_to_string(&config_file)?;
    let persisted: toml::Value = toml::from_str(&persisted)?;
    assert_eq!(
        persisted["default_text_model"].as_str(),
        Some("deepseek-v4-pro"),
        "the active Z.ai model must not overwrite the DeepSeek fallback"
    );
    assert_eq!(
        persisted["providers"]["zai"]["model"].as_str(),
        Some("GLM-5-Turbo")
    );

    let (status, body) = post_set_config(&client, &addr, "model", "deepseek-v4-flash", true).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");

    let response = client
        .post(format!("http://{addr}/v1/config/reload"))
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let after = get_config(&client, &addr).await;
    assert_eq!(after["provider"], "zai");
    assert_eq!(after["model"], "GLM-5-Turbo");
    assert_eq!(after["default_model"], "deepseek-v4-pro");

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn reload_config_preserves_profile_selected_named_custom_route() -> Result<()> {
    let root = std::env::temp_dir().join(format!(
        "codewhale-config-reload-profile-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&root)?;
    let config_file = root.join("custom-config.toml");
    fs::write(
        &config_file,
        r#"provider = "deepseek"
default_text_model = "deepseek-v4-pro"

[profiles.local]
provider = "lm-studio"

[profiles.local.providers.lm-studio]
kind = "openai-compatible"
base_url = "http://127.0.0.1:18190/v1"
model = "profile-local-model"
api_key = "profile-test-key"
"#,
    )?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path_and_profile(config_file, "local".to_string()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let response = client
        .post(format!("http://{addr}/v1/config/reload"))
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::OK);

    let config = get_config(&client, &addr).await;
    assert_eq!(config["provider"], "lm-studio");
    assert_eq!(config["model"], "profile-local-model");
    assert_eq!(config["base_url"], "http://127.0.0.1:18190/v1");

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn reload_config_refreshes_mcp_config_path() -> Result<()> {
    // Fix #3: After reload, list_mcp_servers should see the new mcp_config_path
    // from the reloaded config (not a stale cached value).
    //
    // This test works by:
    // 1. Starting with config_path pointing to custom-config.toml (initially empty)
    // 2. Writing mcp_config_path = <new_path> to the config file via set_config
    // 3. Reloading
    // 4. GET /v1/config and verifying mcp_config_path field changed
    //
    // If Fix #3 were still needed (stale mcp_config_path field in state),
    // this test would fail because the old field wouldn't update. Since we
    // removed the stale field and read directly from config, this test also
    // validates that architectural decision.
    let root =
        std::env::temp_dir().join(format!("codewhale-config-mcp-refresh-{}", Uuid::new_v4()));
    fs::create_dir_all(&root)?;
    let config_file = root.join("custom-config.toml");
    fs::write(&config_file, "# initial\n")?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    // Record initial mcp_config_path (set by test helper to root/mcp.json).
    let before = get_config(&client, &addr).await;
    let initial_mcp_path = before["mcp_config_path"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !initial_mcp_path.is_empty(),
        "initial mcp_config_path should not be empty"
    );

    // Persist a new mcp_config_path to the config file.
    let new_mcp_path = root.join("custom-mcp.json");
    let new_mcp_path_str = new_mcp_path.to_string_lossy().to_string();
    let (status, body) =
        post_set_config(&client, &addr, "mcp_config_path", &new_mcp_path_str, true).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    // Before reload, GET should still return the old path.
    let after_set = get_config(&client, &addr).await;
    assert_eq!(
        after_set["mcp_config_path"].as_str().unwrap_or_default(),
        initial_mcp_path,
        "set_config must NOT update in-memory mcp_config_path before reload"
    );

    // Reload.
    let reload_resp = client
        .post(format!("http://{addr}/v1/config/reload"))
        .send()
        .await?;
    assert_eq!(reload_resp.status(), StatusCode::OK);

    // After reload, GET should return the new path.
    let after_reload = get_config(&client, &addr).await;
    let reloaded_mcp_path = after_reload["mcp_config_path"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        reloaded_mcp_path, new_mcp_path_str,
        "after reload, mcp_config_path should reflect the persisted value — \
         if this fails, the MCP path is stale after reload"
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn set_config_with_persist_false_does_not_write_to_disk() -> Result<()> {
    // Verify the persist:false branch: response reports persisted:false and
    // the config file on disk is NOT modified. This is the "dry run" path
    // the GUI can use to validate input without committing changes.
    let root = std::env::temp_dir().join(format!("codewhale-config-nopersist-{}", Uuid::new_v4()));
    fs::create_dir_all(&root)?;
    let config_file = root.join("custom-config.toml");
    let initial_contents = "# initial empty config\n";
    fs::write(&config_file, initial_contents)?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let (status, body) = post_set_config(&client, &addr, "model", "deepseek-v4-flash", false).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "persist:false should still return 200, body: {body}"
    );
    assert_eq!(
        body["persisted"].as_bool(),
        Some(false),
        "persisted should be false when persist:false, body: {body}"
    );
    assert_eq!(
        body["requires_reload"].as_bool(),
        Some(false),
        "requires_reload should be false when persist:false, body: {body}"
    );
    assert_eq!(
        body["key"].as_str().unwrap_or_default(),
        "model",
        "key should echo the request key, body: {body}"
    );

    // The config file on disk must NOT have been modified.
    let contents = fs::read_to_string(&config_file)?;
    assert_eq!(
        contents, initial_contents,
        "persist:false must not modify the config file on disk"
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn set_config_subagents_max_depth_below_ceiling_not_clamped() -> Result<()> {
    // Verify that values at and below the ceiling pass through unchanged.
    // The existing clamping test only verifies over-ceiling clamping; this
    // test ensures legitimate values are not accidentally modified.
    let root =
        std::env::temp_dir().join(format!("codewhale-config-depth-noclamp-{}", Uuid::new_v4()));
    fs::create_dir_all(&root)?;
    let config_file = root.join("custom-config.toml");
    fs::write(&config_file, "# initial\n")?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    // Test a value at the ceiling (should not be clamped).
    let ceiling = u64::from(codewhale_config::MAX_SPAWN_DEPTH_CEILING);
    let (status, body) = post_set_config(
        &client,
        &addr,
        "subagents_max_depth",
        &ceiling.to_string(),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let contents = fs::read_to_string(&config_file)?;
    let expected = format!("max_depth = {ceiling}");
    assert!(
        contents.contains(&expected),
        "value at ceiling should be written as-is: expected '{expected}', got: {contents}"
    );

    // Test a value below the ceiling (should not be clamped).
    let below = ceiling.saturating_sub(1);
    let (status, body) = post_set_config(
        &client,
        &addr,
        "subagents_max_depth",
        &below.to_string(),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let contents = fs::read_to_string(&config_file)?;
    let expected = format!("max_depth = {below}");
    assert!(
        contents.contains(&expected),
        "value below ceiling should be written as-is: expected '{expected}', got: {contents}"
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn set_config_subagents_enabled_false_persists() -> Result<()> {
    // Verify that subagents_enabled=false is properly persisted. The
    // existing test only verifies the true branch; this covers the false
    // branch to ensure both boolean values round-trip correctly.
    let root = std::env::temp_dir().join(format!("codewhale-config-subfalse-{}", Uuid::new_v4()));
    fs::create_dir_all(&root)?;
    let config_file = root.join("custom-config.toml");
    fs::write(&config_file, "[subagents]\nenabled = true\n")?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let (status, body) = post_set_config(&client, &addr, "subagents_enabled", "false", true).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(
        body["persisted"].as_bool().unwrap_or(false),
        "should report persisted=true, body: {body}"
    );

    let contents = fs::read_to_string(&config_file)?;
    assert!(
        contents.contains("enabled = false"),
        "config file should contain 'enabled = false', got: {contents}"
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn reload_config_with_malformed_file_returns_error() -> Result<()> {
    // Verify error handling: if the config file contains invalid TOML,
    // reload should return 500 instead of crashing or silently succeeding.
    // This catches regressions where the map_err is accidentally removed.
    let root = std::env::temp_dir().join(format!("codewhale-config-malformed-{}", Uuid::new_v4()));
    fs::create_dir_all(&root)?;
    let config_file = root.join("custom-config.toml");
    fs::write(&config_file, "# initial\n")?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    // Corrupt the config file with invalid TOML.
    fs::write(&config_file, "this is = = not valid toml [[[\n")?;

    let resp = client
        .post(format!("http://{addr}/v1/config/reload"))
        .send()
        .await?;
    assert_eq!(
        resp.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "reload with malformed config should return 500"
    );

    // Verify the error response has a meaningful message.
    let body: serde_json::Value = resp.json().await.unwrap_or_default();
    let message = body["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_lowercase();
    assert!(
        message.contains("failed to reload config"),
        "error message should mention reload failure, got: {message}"
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn set_config_model_follows_persisted_provider_before_reload() -> Result<()> {
    let root = std::env::temp_dir().join(format!(
        "codewhale-config-provider-model-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&root)?;
    let config_file = root.join("custom-config.toml");
    fs::write(
        &config_file,
        format!(
            "provider = \"deepseek\"\ndefault_text_model = \"deepseek-v4-pro\"\n\n[providers.volcengine]\nmodel = \"{}\"\n",
            crate::config::DEFAULT_VOLCENGINE_MODEL
        ),
    )?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let (status, body) = post_set_config(&client, &addr, "provider", "volcengine", true).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let target_model = crate::config::DEFAULT_VOLCENGINE_FLASH_MODEL;
    let (status, body) = post_set_config(&client, &addr, "model", target_model, true).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let config_body = fs::read_to_string(&config_file)?;
    assert!(
        config_body.contains("provider = \"volcengine\""),
        "provider should be persisted before reload"
    );
    // The persisted value is the normalized wire id (lowercase), not the
    // display spelling of DEFAULT_VOLCENGINE_FLASH_MODEL.
    let expected_wire = crate::config::normalize_model_name_for_provider(
        crate::config::ProviderKind::Volcengine,
        target_model,
    )
    .expect("volcengine flash model should normalize");
    assert!(
        config_body.contains(&format!("model = \"{expected_wire}\"")),
        "volcengine model should be written to the provider table as its wire id"
    );
    let saved: toml::Value = toml::from_str(&config_body)?;
    assert_eq!(
        saved["providers"]["deepseek"]["model"].as_str(),
        Some("deepseek-v4-pro"),
        "switching provider model must preserve DeepSeek's migrated model table"
    );
    assert!(saved.get("default_text_model").is_none());

    let reload_resp = client
        .post(format!("http://{addr}/v1/config/reload"))
        .send()
        .await?;
    assert_eq!(reload_resp.status(), StatusCode::OK);

    let after_reload = get_config(&client, &addr).await;
    assert_eq!(after_reload["provider"].as_str(), Some("volcengine"));
    assert_eq!(after_reload["model"].as_str(), Some(expected_wire.as_str()));

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn reload_config_applies_multiple_persisted_keys() -> Result<()> {
    // Verify that multiple set_config calls accumulate on disk and a single
    // reload picks up ALL changes. This catches regressions where reload
    // only applies the last-written key or where set_config overwrites
    // prior keys unexpectedly.
    let root = std::env::temp_dir().join(format!("codewhale-config-multi-{}", Uuid::new_v4()));
    fs::create_dir_all(&root)?;
    let config_file = root.join("custom-config.toml");
    fs::write(&config_file, "# initial\n")?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    // Record initial values.
    let before = get_config(&client, &addr).await;
    let initial_model = before["model"].as_str().unwrap_or_default().to_string();
    let initial_depth = before["subagents_max_depth"].as_u64().unwrap_or(0);
    let initial_enabled = before["subagents_enabled"].as_bool().unwrap_or(false);

    // Persist three different keys.
    // Use a valid DeepSeek model ID so Config::validate() doesn't reject
    // the reloaded config.
    let test_model = "deepseek-v4-pro";
    let (status, body) = post_set_config(&client, &addr, "model", test_model, true).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let (status, body) = post_set_config(&client, &addr, "subagents_max_depth", "4", true).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    // Flip subagents_enabled to the opposite of its initial value.
    let target_enabled = !initial_enabled;
    let (status, body) = post_set_config(
        &client,
        &addr,
        "subagents_enabled",
        &target_enabled.to_string(),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    // Before reload, in-memory state should be unchanged for all three keys.
    let after_set = get_config(&client, &addr).await;
    assert_eq!(
        after_set["model"].as_str().unwrap_or_default(),
        initial_model,
        "model should be unchanged before reload"
    );
    assert_eq!(
        after_set["subagents_max_depth"].as_u64(),
        Some(initial_depth),
        "subagents_max_depth should be unchanged before reload"
    );
    assert_eq!(
        after_set["subagents_enabled"].as_bool(),
        Some(initial_enabled),
        "subagents_enabled should be unchanged before reload"
    );

    // Reload.
    let reload_resp = client
        .post(format!("http://{addr}/v1/config/reload"))
        .send()
        .await?;
    assert_eq!(reload_resp.status(), StatusCode::OK);

    // After reload, ALL three keys should reflect their persisted values.
    let after_reload = get_config(&client, &addr).await;
    assert_eq!(
        after_reload["model"].as_str().unwrap_or_default(),
        test_model,
        "model should be updated after reload"
    );
    assert_eq!(
        after_reload["subagents_max_depth"].as_u64(),
        Some(4),
        "subagents_max_depth should be 4 after reload"
    );
    assert_eq!(
        after_reload["subagents_enabled"].as_bool(),
        Some(target_enabled),
        "subagents_enabled should be {} after reload",
        target_enabled
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn set_config_response_contains_all_expected_fields() -> Result<()> {
    // Verify the SetConfigResponse shape: key, value, message, persisted,
    // requires_reload. This catches serialization regressions and ensures
    // the GUI client can rely on these fields being present and correct.
    let root = std::env::temp_dir().join(format!("codewhale-config-shape-{}", Uuid::new_v4()));
    fs::create_dir_all(&root)?;
    let config_file = root.join("custom-config.toml");
    fs::write(&config_file, "# initial\n")?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    // persist:true → persisted=true, requires_reload=true
    let (status, body) = post_set_config(&client, &addr, "model", "deepseek-v4-flash", true).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["key"].as_str(),
        Some("model"),
        "key field, body: {body}"
    );
    assert_eq!(
        body["value"].as_str(),
        Some("deepseek-v4-flash"),
        "value field, body: {body}"
    );
    assert!(
        body["message"].as_str().is_some_and(|m| !m.is_empty()),
        "message should be non-empty, body: {body}"
    );
    assert_eq!(
        body["persisted"].as_bool(),
        Some(true),
        "persisted should be true, body: {body}"
    );
    assert_eq!(
        body["requires_reload"].as_bool(),
        Some(true),
        "requires_reload should be true when persist:true, body: {body}"
    );

    // persist:false → persisted=false, requires_reload=false
    let (status, body) = post_set_config(&client, &addr, "model", "deepseek-v4-pro", false).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["key"].as_str(),
        Some("model"),
        "key field, body: {body}"
    );
    assert_eq!(
        body["value"].as_str(),
        Some("deepseek-v4-pro"),
        "value field, body: {body}"
    );
    assert_eq!(
        body["persisted"].as_bool(),
        Some(false),
        "persisted should be false, body: {body}"
    );
    assert_eq!(
        body["requires_reload"].as_bool(),
        Some(false),
        "requires_reload should be false when persist:false, body: {body}"
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn cors_layer_advertises_exact_supported_headers_and_never_an_extra() -> Result<()> {
    let layer = cors_layer(&[]);
    let router: Router = Router::new()
        .route("/probe", get(|| async { "ok" }))
        .layer(layer);

    let listener = match TcpListener::bind("127.0.0.1:0").await {
        Ok(listener) => listener,
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => return Ok(()),
        Err(err) => return Err(err.into()),
    };
    let addr = listener.local_addr()?;
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    let client = crate::tls::reqwest_client();

    let allowed = client
        .request(reqwest::Method::OPTIONS, format!("http://{addr}/probe"))
        .header("Origin", "http://localhost:1420")
        .header("Access-Control-Request-Method", "POST")
        .header(
            "Access-Control-Request-Headers",
            "authorization, content-type, accept, if-match, x-codewhale-runtime-token, x-deepseek-runtime-token",
        )
        .send()
        .await?;

    assert!(allowed.status().is_success());
    assert_eq!(
        allowed
            .headers()
            .get("access-control-allow-origin")
            .and_then(|value| value.to_str().ok()),
        Some("http://localhost:1420")
    );
    let allow_headers = allowed
        .headers()
        .get("access-control-allow-headers")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let advertised = allow_headers
        .split(',')
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .collect::<std::collections::BTreeSet<_>>();
    let expected = [
        "accept",
        "authorization",
        "content-type",
        "if-match",
        "x-codewhale-runtime-token",
        "x-deepseek-runtime-token",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<std::collections::BTreeSet<_>>();

    assert_eq!(advertised, expected);

    // Every method a `/v1` route is mounted on must be advertised, or the
    // browser blocks it at preflight (PUT saves keys, files, sessions, goals).
    let advertised_methods = allowed
        .headers()
        .get("access-control-allow-methods")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .map(str::to_ascii_uppercase)
        .collect::<std::collections::BTreeSet<_>>();
    let expected_methods = ["DELETE", "GET", "OPTIONS", "PATCH", "POST", "PUT"]
        .into_iter()
        .map(str::to_string)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(advertised_methods, expected_methods);

    let unapproved = client
        .request(reqwest::Method::OPTIONS, format!("http://{addr}/probe"))
        .header("Origin", "http://localhost:1420")
        .header("Access-Control-Request-Method", "POST")
        .header(
            "Access-Control-Request-Headers",
            "authorization, x-malicious-header",
        )
        .send()
        .await?;
    let unapproved_headers = unapproved
        .headers()
        .get("access-control-allow-headers")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    assert!(
        !unapproved_headers.contains("x-malicious-header"),
        "an unapproved request header must never be advertised to the browser"
    );

    handle.abort();
    Ok(())
}

// ---------------------------------------------------------------------------
// Goal-loop endpoint tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn thread_goal_crud_and_invalid_transition() -> Result<()> {
    let Some((addr, runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    // Create a thread to associate goals with.
    let thread: serde_json::Value = client
        .post(format!("http://{addr}/v1/threads"))
        .json(&json!({}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let thread_id = thread["id"].as_str().expect("thread id").to_string();

    // GET before any goal exists → 404.
    let no_goal = client
        .get(format!("http://{addr}/v1/threads/{thread_id}/goal"))
        .send()
        .await?;
    assert_eq!(no_goal.status(), 404, "no goal yet");

    // PUT creates the goal (201).
    let created: serde_json::Value = client
        .put(format!("http://{addr}/v1/threads/{thread_id}/goal"))
        .json(&json!({"objective": "write tests", "token_budget": 5000}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        created["status"].as_str(),
        Some("active"),
        "new goal is active"
    );
    assert_eq!(
        created["objective"].as_str(),
        Some("write tests"),
        "objective round-trips"
    );
    assert_eq!(created["token_budget"], 5000, "budget round-trips");

    // GET after create → 200 with the same fields.
    let fetched: serde_json::Value = client
        .get(format!("http://{addr}/v1/threads/{thread_id}/goal"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(fetched["objective"], created["objective"]);
    assert_eq!(fetched["goal_id"], created["goal_id"]);

    // Simulate progress on the first goal before replacing it.
    let mut accrued: codewhale_protocol::ThreadGoal = serde_json::from_value(created.clone())?;
    accrued.tokens_used = 1_234;
    accrued.time_used_seconds = 45;
    accrued.continuation_count = 3;
    accrued.created_at -= 60;
    runtime_threads.save_goal(accrued.clone()).await?;

    // PUT again (replacement) → 200 with a fresh goal lifecycle.
    let updated: serde_json::Value = client
        .put(format!("http://{addr}/v1/threads/{thread_id}/goal"))
        .json(&json!({"objective": "write even better tests"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        updated["objective"].as_str(),
        Some("write even better tests")
    );
    assert_ne!(updated["goal_id"], created["goal_id"]);
    assert!(
        updated["goal_id"]
            .as_str()
            .is_some_and(|goal_id| goal_id.starts_with("goal-"))
    );
    assert_eq!(updated["status"].as_str(), Some("active"));
    assert_eq!(updated["tokens_used"], 0);
    assert_eq!(updated["time_used_seconds"], 0);
    assert_eq!(updated["continuation_count"], 0);
    assert_eq!(updated["token_budget"], serde_json::Value::Null);
    assert!(updated["created_at"].as_i64() > Some(accrued.created_at));
    assert_eq!(updated["created_at"], updated["updated_at"]);

    // POST /complete → 200 with status = complete.
    let completed: serde_json::Value = client
        .post(format!(
            "http://{addr}/v1/threads/{thread_id}/goal/complete"
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(completed["status"].as_str(), Some("complete"));

    // POST /complete again on a terminal goal → 409 Conflict.
    let double_complete = client
        .post(format!(
            "http://{addr}/v1/threads/{thread_id}/goal/complete"
        ))
        .send()
        .await?;
    assert_eq!(
        double_complete.status(),
        409,
        "completing an already-complete goal must be 409"
    );

    // POST /block on a terminal goal → 409 Conflict.
    let block_after_complete = client
        .post(format!("http://{addr}/v1/threads/{thread_id}/goal/block"))
        .send()
        .await?;
    assert_eq!(
        block_after_complete.status(),
        409,
        "blocking a complete goal must be 409"
    );

    // DELETE → 204.
    let deleted = client
        .delete(format!("http://{addr}/v1/threads/{thread_id}/goal"))
        .send()
        .await?;
    assert_eq!(deleted.status(), 204, "delete returns 204");

    // DELETE again → 404.
    let double_delete = client
        .delete(format!("http://{addr}/v1/threads/{thread_id}/goal"))
        .send()
        .await?;
    assert_eq!(double_delete.status(), 404, "second delete is 404");

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn thread_goal_block_transition() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let thread: serde_json::Value = client
        .post(format!("http://{addr}/v1/threads"))
        .json(&json!({}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let thread_id = thread["id"].as_str().expect("thread id").to_string();

    // Create an active goal.
    client
        .put(format!("http://{addr}/v1/threads/{thread_id}/goal"))
        .json(&json!({"objective": "block me"}))
        .send()
        .await?
        .error_for_status()?;

    // Block it.
    let blocked: serde_json::Value = client
        .post(format!("http://{addr}/v1/threads/{thread_id}/goal/block"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(blocked["status"].as_str(), Some("blocked"));

    // Can still complete from blocked state.
    let completed: serde_json::Value = client
        .post(format!(
            "http://{addr}/v1/threads/{thread_id}/goal/complete"
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(completed["status"].as_str(), Some("complete"));

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn thread_goal_on_unknown_thread_returns_404() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let resp = client
        .get(format!("http://{addr}/v1/threads/nonexistent-id/goal"))
        .send()
        .await?;
    assert_eq!(resp.status(), 404);

    let put_resp = client
        .put(format!("http://{addr}/v1/threads/nonexistent-id/goal"))
        .json(&json!({"objective": "ghost"}))
        .send()
        .await?;
    assert_eq!(put_resp.status(), 404);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn runtime_info_advertises_thread_goals_capability() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let info: serde_json::Value = client
        .get(format!("http://{addr}/v1/runtime/info"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        info["capabilities"]["thread_goals"].as_bool(),
        Some(true),
        "runtime info must advertise thread_goals capability"
    );

    handle.abort();
    Ok(())
}

#[test]
fn fleet_receipt_json_pass_result_has_no_failure_fields() {
    use codewhale_protocol::fleet::{FleetReceipt, FleetRunId, FleetTaskResult};
    let receipt = FleetReceipt {
        run_id: FleetRunId::from("run-1"),
        task_id: "task-a".to_string(),
        worker_id: "worker-1".to_string(),
        attempt: Some(1),
        terminal_seq: Some(42),
        completed_at: "2025-01-01T00:00:00Z".to_string(),
        result: FleetTaskResult::Pass,
        failure_kind: None,
        artifacts: Vec::new(),
        score: None,
        resolved_route: None,
        saved_session_id: None,
        effective_permissions: None,
    };
    let value = fleet_receipt_json(&receipt);
    assert_eq!(value["run_id"], "run-1");
    assert_eq!(value["task_id"], "task-a");
    assert_eq!(value["worker_id"], "worker-1");
    assert_eq!(value["attempt"], 1);
    assert_eq!(value["terminal_seq"], 42);
    assert_eq!(value["result"], "pass");
    assert!(value["failure_kind"].is_null());
    assert!(value["failure_class"].is_null());
    assert_eq!(value["retry_eligible"], false);
    assert_eq!(value["evidence_available"], false);
    assert!(value["score"].is_null());
}

#[test]
fn fleet_receipt_json_verifier_failure_is_not_retry_eligible() {
    use codewhale_protocol::fleet::{
        FleetReceipt, FleetRunId, FleetTaskFailureKind, FleetTaskResult,
    };
    let receipt = FleetReceipt {
        run_id: FleetRunId::from("run-v"),
        task_id: "task-v".to_string(),
        worker_id: "worker-v".to_string(),
        attempt: Some(1),
        terminal_seq: None,
        completed_at: "2025-01-01T00:00:00Z".to_string(),
        result: FleetTaskResult::Fail,
        failure_kind: Some(FleetTaskFailureKind::Verifier),
        artifacts: Vec::new(),
        score: None,
        resolved_route: None,
        saved_session_id: None,
        effective_permissions: None,
    };
    let value = fleet_receipt_json(&receipt);
    assert_eq!(value["result"], "fail");
    assert_eq!(value["failure_kind"], "verifier");
    assert_eq!(value["retry_eligible"], false);
    assert!(
        value["failure_class"]
            .as_str()
            .is_some_and(|s| s.contains("Verifier")),
        "failure_class should describe a verifier rejection"
    );
}

#[test]
fn fleet_receipt_json_transport_failure_is_retry_eligible() {
    use codewhale_protocol::fleet::{
        FleetReceipt, FleetRunId, FleetTaskFailureKind, FleetTaskResult,
    };
    let receipt = FleetReceipt {
        run_id: FleetRunId::from("run-t"),
        task_id: "task-t".to_string(),
        worker_id: "worker-t".to_string(),
        attempt: Some(1),
        terminal_seq: None,
        completed_at: "2025-01-01T00:00:00Z".to_string(),
        result: FleetTaskResult::Fail,
        failure_kind: Some(FleetTaskFailureKind::Transport),
        artifacts: Vec::new(),
        score: None,
        resolved_route: None,
        saved_session_id: None,
        effective_permissions: None,
    };
    let value = fleet_receipt_json(&receipt);
    assert_eq!(value["failure_kind"], "transport");
    assert_eq!(value["retry_eligible"], true);
}

#[test]
fn fleet_receipt_json_receipt_artifact_sets_evidence_available() {
    use codewhale_protocol::fleet::{
        FleetArtifactKind, FleetArtifactRef, FleetReceipt, FleetRunId, FleetScore, FleetTaskResult,
    };
    use std::path::PathBuf;
    let receipt = FleetReceipt {
        run_id: FleetRunId::from("run-e"),
        task_id: "task-e".to_string(),
        worker_id: "worker-e".to_string(),
        attempt: Some(1),
        terminal_seq: None,
        completed_at: "2025-01-01T00:00:00Z".to_string(),
        result: FleetTaskResult::Pass,
        failure_kind: None,
        artifacts: vec![FleetArtifactRef {
            kind: FleetArtifactKind::Receipt,
            path: PathBuf::from(".codewhale/fleet/run-e/task-e/worker-e/receipt.json"),
            checksum: Some("sha256:abc123".to_string()),
            mime_type: Some("application/json".to_string()),
            size_bytes: Some(512),
        }],
        score: Some(FleetScore {
            value: 1.0,
            max: Some(1.0),
            notes: Some("all checks pass".to_string()),
        }),
        resolved_route: None,
        saved_session_id: None,
        effective_permissions: None,
    };
    let value = fleet_receipt_json(&receipt);
    assert_eq!(value["evidence_available"], true);
    assert_eq!(value["score"]["value"], 1.0);
    assert_eq!(value["score"]["max"], 1.0);
    assert_eq!(value["score"]["notes"], "all checks pass");
    assert_eq!(value["artifacts"][0]["kind"], "receipt");
}

#[tokio::test]
async fn fleet_receipt_api_list_and_get_round_trip() -> Result<()> {
    use crate::fleet::ledger::FleetLedger;
    use crate::fleet::task_spec::FleetTaskVerificationInput;
    use crate::fleet::task_spec::{
        FleetTaskSpecDocument, FleetTaskVerification, prepare_verification_receipt,
    };
    use codewhale_protocol::fleet::{FleetScore, FleetTaskResult};

    let root = std::env::temp_dir().join(format!("codewhale-receipt-api-{}", Uuid::new_v4()));
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace)?;

    let task = codewhale_protocol::fleet::FleetTaskSpec {
        id: "task-receipt".to_string(),
        name: "Receipt Task".to_string(),
        description: None,
        objective: Some("Test receipt API".to_string()),
        instructions: "run tests".to_string(),
        worker: Some(codewhale_protocol::fleet::FleetTaskWorkerProfile {
            agent_profile: None,
            role: Some("reviewer".to_string()),
            loadout: None,
            model_class: None,
            model: None,
            tool_profile: Some("read-only".to_string()),
            tools: Vec::new(),
            capabilities: Vec::new(),
        }),
        workspace: None,
        input_files: Vec::new(),
        context: Vec::new(),
        budget: None,
        tags: Vec::new(),
        expected_artifacts: Vec::new(),
        scorer: None,
        retry_policy: None,
        alert_policy: None,
        timeout_seconds: None,
        metadata: std::collections::BTreeMap::new(),
    };
    let manager = crate::fleet::manager::FleetManager::open(&workspace)?
        .with_session_model(crate::config::DEFAULT_TEXT_MODEL)
        .with_route_config(test_fleet_route_config());
    let report = manager.create_run(
        FleetTaskSpecDocument {
            name: Some("receipt-api-smoke".to_string()),
            labels: std::collections::BTreeMap::new(),
            security_policy: None,
            workers: Vec::new(),
            tasks: vec![task],
            usage_ceiling: None,
        },
        1,
    )?;
    let run_id = report.run_id.clone();

    // Directly record a synthetic receipt so we don't need a live worker.
    let ledger = FleetLedger::open(&workspace)?;
    let verification_input = FleetTaskVerificationInput {
        run_id: run_id.clone(),
        task_id: "task-receipt".to_string(),
        worker_id: "worker-1".to_string(),
        attempt: 1,
        exit_code: Some(0),
        artifacts: Vec::new(),
        final_answer: None,
        saved_session_id: None,
        resolved_route: None,
        effective_permissions: None,
    };
    let receipt = prepare_verification_receipt(
        &workspace,
        &verification_input,
        FleetTaskVerification {
            result: FleetTaskResult::Pass,
            failure_kind: None,
            score: FleetScore {
                value: 1.0,
                max: Some(1.0),
                notes: Some("exit_code=0".to_string()),
            },
            evidence: vec!["exit_code=0".to_string()],
        },
    )?;
    let evidence_path = workspace.join(&receipt.artifacts.last().unwrap().path);
    let original_evidence = fs::read(&evidence_path)?;
    ledger.record_receipt(receipt)?;

    let sessions_dir = root.join("sessions");
    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root_token_mobile_workspace(
            root.clone(),
            sessions_dir,
            None,
            false,
            workspace,
        )
        .await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    // List receipts for the run.
    let list: serde_json::Value = client
        .get(format!("http://{addr}/v1/fleet/runs/{}/receipts", run_id.0))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(list["run_id"], run_id.0.as_str());
    assert_eq!(list["receipts"].as_array().map(|a| a.len()), Some(1));
    let receipt_entry = &list["receipts"][0];
    assert_eq!(receipt_entry["task_id"], "task-receipt");
    assert_eq!(receipt_entry["result"], "pass");
    assert_eq!(receipt_entry["retry_eligible"], false);
    assert_eq!(receipt_entry["evidence_available"], true);

    // Get specific receipt by task_id.
    let detail: serde_json::Value = client
        .get(format!(
            "http://{addr}/v1/fleet/runs/{}/receipts/task-receipt",
            run_id.0
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(detail["run_id"], run_id.0.as_str());
    assert_eq!(detail["task_id"], "task-receipt");
    assert_eq!(detail["worker_id"], "worker-1");
    assert_eq!(detail["attempt"], 1);
    assert_eq!(detail["result"], "pass");
    assert!(detail["failure_kind"].is_null());
    assert_eq!(detail["evidence_available"], true);

    // Inspect evidence content.
    let evidence: serde_json::Value = client
        .get(format!(
            "http://{addr}/v1/fleet/runs/{}/receipts/task-receipt/evidence",
            run_id.0
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(evidence["run_id"], run_id.0.as_str());
    assert_eq!(evidence["task_id"], "task-receipt");
    assert_eq!(evidence["truncated"], false);
    assert!(
        evidence["content"].is_object(),
        "evidence content should parse as JSON object"
    );
    assert_eq!(evidence["content"]["task_id"], "task-receipt");

    // Read the actual route after same-size tampering and symlink swaps. The
    // ledger still names the original digest: metadata alone is not evidence.
    let evidence_url = format!(
        "http://{addr}/v1/fleet/runs/{}/receipts/task-receipt/evidence",
        run_id.0
    );
    let mut rejected = Vec::new();
    let mut changed = original_evidence.clone();
    *changed.last_mut().unwrap() ^= 1;
    fs::write(&evidence_path, changed)?;
    rejected.push(client.get(&evidence_url).send().await?.status().as_u16());
    fs::write(&evidence_path, &original_evidence)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let outside = root.join("outside-receipt.txt");
        let canary = "OUTSIDE_SYNTHETIC_RECEIPT_CANARY";
        fs::write(&outside, canary)?;
        fs::remove_file(&evidence_path)?;
        symlink(&outside, &evidence_path)?;
        let response = client.get(&evidence_url).send().await?;
        rejected.push(response.status().as_u16());
        let body = response.text().await?;
        assert!(!body.contains(canary));
        fs::remove_file(&evidence_path)?;
        fs::write(&evidence_path, &original_evidence)?;

        let parent = evidence_path.parent().unwrap();
        let retained = parent.with_extension("retained");
        let outside_dir = root.join("outside-directory");
        fs::create_dir_all(&outside_dir)?;
        fs::write(outside_dir.join(evidence_path.file_name().unwrap()), canary)?;
        fs::rename(parent, &retained)?;
        symlink(&outside_dir, parent)?;
        let response = client.get(&evidence_url).send().await?;
        rejected.push(response.status().as_u16());
        let body = response.text().await?;
        assert!(!body.contains(canary));
        fs::remove_file(parent)?;
        fs::rename(retained, parent)?;
        assert_eq!(fs::read_to_string(outside)?, canary);
    }
    assert!(
        rejected.iter().all(|status| *status == 400),
        "unverified evidence status codes: {rejected:?}"
    );
    let restored = client.get(&evidence_url).send().await?;
    assert_eq!(restored.status(), 200);

    // Missing task returns 404.
    let missing = client
        .get(format!(
            "http://{addr}/v1/fleet/runs/{}/receipts/no-such-task",
            run_id.0
        ))
        .send()
        .await?;
    assert_eq!(missing.status(), 404);

    handle.abort();
    Ok(())
}

// ── Memory API tests ──

#[tokio::test]
async fn memory_info_capability_is_advertised() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let info: serde_json::Value = client
        .get(format!("http://{addr}/v1/runtime/info"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        info["capabilities"]["memory"], true,
        "memory capability must be advertised in runtime/info"
    );
    handle.abort();
    Ok(())
}

#[tokio::test]
async fn memory_list_returns_empty_for_fresh_store() -> Result<()> {
    let root = std::env::temp_dir().join(format!("cw-memory-list-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    let _lock = lock_test_env();
    let home = root.join("home");
    fs::create_dir_all(&home)?;
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", &home);
    let Some((addr, _rt, handle)) = spawn_test_server_with_root(root, sessions_dir).await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let body: serde_json::Value = client
        .get(format!("http://{addr}/v1/memory"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(body["entries"].as_array().map(Vec::len), Some(0));
    assert_eq!(body["total"], 0);

    // scope=global should also be empty.
    let global: serde_json::Value = client
        .get(format!("http://{addr}/v1/memory?scope=global"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(global["total"], 0);

    // Invalid scope returns 400.
    let bad = client
        .get(format!("http://{addr}/v1/memory?scope=invalid"))
        .send()
        .await?;
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);

    // limit=0 returns 400.
    let bad_limit = client
        .get(format!("http://{addr}/v1/memory?limit=0"))
        .send()
        .await?;
    assert_eq!(bad_limit.status(), StatusCode::BAD_REQUEST);

    // limit above max returns 400.
    let over_limit = client
        .get(format!("http://{addr}/v1/memory?limit=201"))
        .send()
        .await?;
    assert_eq!(over_limit.status(), StatusCode::BAD_REQUEST);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn memory_create_list_and_get_entry() -> Result<()> {
    let root = std::env::temp_dir().join(format!("cw-memory-create-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    let _lock = lock_test_env();
    let home = root.join("home");
    fs::create_dir_all(&home)?;
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", &home);
    let Some((addr, _rt, handle)) = spawn_test_server_with_root(root.clone(), sessions_dir).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    // Create a global memory entry.
    let create_resp = client
        .post(format!("http://{addr}/v1/memory"))
        .json(&json!({ "text": "prefer snake_case for identifiers", "scope": "global" }))
        .send()
        .await?;
    assert_eq!(create_resp.status(), StatusCode::CREATED);
    let created: serde_json::Value = create_resp.json().await?;
    assert_eq!(created["entry"]["scope"], "global");
    assert_eq!(created["entry"]["status"], "active");
    assert!(created["entry"]["id"].is_number());
    assert!(
        created["entry"]["summary"]
            .as_str()
            .unwrap_or("")
            .contains("snake_case"),
        "summary must include the note text"
    );

    let entry_id = created["entry"]["id"].as_i64().unwrap();

    // GET /v1/memory lists the entry.
    let list: serde_json::Value = client
        .get(format!("http://{addr}/v1/memory?scope=global"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(list["total"], 1);
    assert_eq!(list["entries"][0]["id"], entry_id);
    assert_eq!(list["entries"][0]["scope"], "global");
    // workspace_id must be absent for global entries.
    assert!(list["entries"][0]["workspace_id"].is_null());

    // GET /v1/memory/{id} returns the entry.
    let single: serde_json::Value = client
        .get(format!("http://{addr}/v1/memory/{entry_id}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(single["entry"]["id"], entry_id);
    assert_eq!(single["entry"]["scope"], "global");
    assert!(single["entry"]["stale"].is_boolean());

    // GET /v1/memory/{id} for a missing id returns 404.
    let missing = client
        .get(format!("http://{addr}/v1/memory/999999"))
        .send()
        .await?;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn memory_summary_is_redacted_to_max_chars() -> Result<()> {
    let root = std::env::temp_dir().join(format!("cw-memory-redact-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    let _lock = lock_test_env();
    let home = root.join("home");
    fs::create_dir_all(&home)?;
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", &home);
    let Some((addr, _rt, handle)) = spawn_test_server_with_root(root.clone(), sessions_dir).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let long_note = "x".repeat(350);
    let create_resp = client
        .post(format!("http://{addr}/v1/memory"))
        .json(&json!({ "text": long_note, "scope": "global" }))
        .send()
        .await?;
    assert_eq!(create_resp.status(), StatusCode::CREATED);
    let created: serde_json::Value = create_resp.json().await?;
    let summary = created["entry"]["summary"].as_str().unwrap_or("");
    // Summary must be bounded: at most 300 chars + 3 for the "…" suffix.
    assert!(
        summary.chars().count() <= 303,
        "summary must be bounded; got {} chars",
        summary.chars().count()
    );
    assert!(
        summary.ends_with("…") || summary.chars().count() <= 300,
        "overlong text must be truncated with an ellipsis"
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn memory_clear_removes_global_scope() -> Result<()> {
    let root = std::env::temp_dir().join(format!("cw-memory-clear-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    let _lock = lock_test_env();
    let home = root.join("home");
    fs::create_dir_all(&home)?;
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", &home);
    let Some((addr, _rt, handle)) = spawn_test_server_with_root(root.clone(), sessions_dir).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    // Seed two entries.
    for note in ["first note", "second note"] {
        client
            .post(format!("http://{addr}/v1/memory"))
            .json(&json!({ "text": note, "scope": "global" }))
            .send()
            .await?
            .error_for_status()?;
    }

    let before: serde_json::Value = client
        .get(format!("http://{addr}/v1/memory?scope=global"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        before["total"], 2,
        "seed entries must be present before clear"
    );

    // Clear global scope.
    let clear: serde_json::Value = client
        .delete(format!("http://{addr}/v1/memory?scope=global"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(clear["cleared"], true);

    let after: serde_json::Value = client
        .get(format!("http://{addr}/v1/memory?scope=global"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(after["total"], 0, "global scope must be empty after clear");

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn memory_create_rejects_empty_text_and_bad_scope() -> Result<()> {
    let root = std::env::temp_dir().join(format!("cw-memory-invalid-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    let _lock = lock_test_env();
    let home = root.join("home");
    fs::create_dir_all(&home)?;
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", &home);
    let Some((addr, _rt, handle)) = spawn_test_server_with_root(root.clone(), sessions_dir).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    // Empty text must be rejected with 400.
    let empty = client
        .post(format!("http://{addr}/v1/memory"))
        .json(&json!({ "text": "", "scope": "global" }))
        .send()
        .await?;
    assert_eq!(empty.status(), StatusCode::BAD_REQUEST);

    // An unknown scope must be rejected with 400.
    let bad_scope = client
        .post(format!("http://{addr}/v1/memory"))
        .json(&json!({ "text": "valid note", "scope": "thread" }))
        .send()
        .await?;
    assert_eq!(bad_scope.status(), StatusCode::BAD_REQUEST);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn memory_search_query_filters_results() -> Result<()> {
    let root = std::env::temp_dir().join(format!("cw-memory-search-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    let _lock = lock_test_env();
    let home = root.join("home");
    fs::create_dir_all(&home)?;
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", &home);
    let Some((addr, _rt, handle)) = spawn_test_server_with_root(root.clone(), sessions_dir).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    // Seed two entries with distinct text.
    for note in ["prefer functional style", "always use snake_case"] {
        client
            .post(format!("http://{addr}/v1/memory"))
            .json(&json!({ "text": note, "scope": "global" }))
            .send()
            .await?
            .error_for_status()?;
    }

    // Searching for "functional" returns only the matching entry.
    let resp: serde_json::Value = client
        .get(format!("http://{addr}/v1/memory?q=functional&scope=global"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(resp["total"], 1);
    let summary = resp["entries"][0]["summary"].as_str().unwrap_or("");
    assert!(
        summary.contains("functional"),
        "search must return the matching entry"
    );

    // An empty q must be rejected with 400.
    let empty_q = client
        .get(format!("http://{addr}/v1/memory?q="))
        .send()
        .await?;
    assert_eq!(empty_q.status(), StatusCode::BAD_REQUEST);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn memory_all_scope_includes_workspace_notes() -> Result<()> {
    let root = std::env::temp_dir().join(format!("cw-memory-all-{}", Uuid::new_v4()));
    let _lock = lock_test_env();
    let home = root.join("home");
    fs::create_dir_all(&home)?;
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", &home);
    let repo = root.join("repo");
    fs::create_dir_all(&repo)?;
    run_test_git(&repo, &["init", "-b", "main"])?;
    run_test_git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/memory-scope.git",
        ],
    )?;
    let Some((addr, _rt, handle)) = spawn_test_server_with_root_token_mobile_workspace(
        root.clone(),
        root.join("sessions"),
        None,
        false,
        repo,
    )
    .await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    for (text, scope) in [
        ("global cedar note", "global"),
        ("workspace cedar note", "workspace"),
    ] {
        let created = client
            .post(format!("http://{addr}/v1/memory"))
            .json(&json!({ "text": text, "scope": scope }))
            .send()
            .await?;
        assert_eq!(created.status(), StatusCode::CREATED);
    }

    // Omitted scope means "all": global plus this repository's notes, with
    // and without a search query.
    for url in [
        format!("http://{addr}/v1/memory"),
        format!("http://{addr}/v1/memory?scope=all"),
        format!("http://{addr}/v1/memory?q=cedar"),
    ] {
        let listed: serde_json::Value = client
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let scopes = listed["entries"]
            .as_array()
            .context("entries array")?
            .iter()
            .filter_map(|entry| entry["scope"].as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            scopes,
            ["global", "workspace"].into_iter().collect(),
            "{url}: {listed}"
        );
    }
    handle.abort();
    Ok(())
}

#[tokio::test]
async fn memory_workspace_scope_without_a_remote_is_a_client_error() -> Result<()> {
    // A workspace with no origin remote has no workspace memory: asking for
    // that scope is the caller's error, as it already is for create.
    let root = std::env::temp_dir().join(format!("cw-memory-no-remote-{}", Uuid::new_v4()));
    let _lock = lock_test_env();
    let home = root.join("home");
    fs::create_dir_all(&home)?;
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", &home);
    let plain = root.join("plain");
    fs::create_dir_all(&plain)?;
    let Some((addr, _rt, handle)) = spawn_test_server_with_root_token_mobile_workspace(
        root.clone(),
        root.join("sessions"),
        None,
        false,
        plain,
    )
    .await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    client
        .post(format!("http://{addr}/v1/memory"))
        .json(&json!({ "text": "global cedar note", "scope": "global" }))
        .send()
        .await?
        .error_for_status()?;
    let listed = client
        .get(format!("http://{addr}/v1/memory?scope=workspace"))
        .send()
        .await?;
    assert_eq!(listed.status(), StatusCode::BAD_REQUEST);
    let cleared = client
        .delete(format!("http://{addr}/v1/memory?scope=workspace"))
        .send()
        .await?;
    assert_eq!(cleared.status(), StatusCode::BAD_REQUEST);
    let all: serde_json::Value = client
        .get(format!("http://{addr}/v1/memory"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(all["total"], 1, "global memory stays listed: {all}");

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn mcp_server_management_crud() -> Result<()> {
    let root = std::env::temp_dir().join(format!("codewhale-mcp-mgmt-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    fs::create_dir_all(&root)?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root(root.clone(), sessions_dir).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let base = format!("http://{addr}/v1/apps/mcp/servers");

    // 1. Create a new server.
    let created: serde_json::Value = client
        .post(&base)
        .header(header::IF_MATCH, mcp_test_revision(&client, &base).await?)
        .json(&serde_json::json!({
            "name": "test-stdio",
            "command": "echo",
            "args": ["hello"],
            "url": "https://example.com/mcp",
            "transport": "sse",
            "connect_timeout": 11,
            "execute_timeout": 22,
            "read_timeout": 33,
            "bearer_token_env_var": "MCP_TOKEN",
            "oauth_resource": "https://example.com/resource",
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(created["name"], "test-stdio");
    assert_eq!(created["enabled"], true);
    assert_eq!(created["command"], "echo");

    // 2. GET the server back — config should be on disk.
    let fetched: serde_json::Value = client
        .get(format!("{base}/test-stdio"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(fetched["name"], "test-stdio");
    assert_eq!(fetched["command"], "echo");

    // 3. Duplicate create returns 409 Conflict.
    let conflict = client
        .post(&base)
        .header(header::IF_MATCH, mcp_test_revision(&client, &base).await?)
        .json(&serde_json::json!({
            "name": "test-stdio",
            "command": "echo",
        }))
        .send()
        .await?;
    assert_eq!(conflict.status(), 409);

    // 4. PATCH (update) the server.
    let updated: serde_json::Value = client
        .patch(format!("{base}/test-stdio"))
        .header(header::IF_MATCH, mcp_test_revision(&client, &base).await?)
        .json(&serde_json::json!({
            "args": ["updated"],
            "required": true,
            "url": null,
            "bearer_token_env_var": null,
            "oauth_resource": null,
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(updated["required"], true);
    assert_eq!(updated["args"][0], "updated");
    assert_eq!(updated["url"], serde_json::Value::Null);
    assert_eq!(updated["transport"], "sse", "missing fields are retained");

    let cleared: serde_json::Value = client
        .patch(format!("{base}/test-stdio"))
        .header(header::IF_MATCH, mcp_test_revision(&client, &base).await?)
        .json(&json!({
            "url": "https://example.com/replacement",
            "command": null,
            "transport": null,
            "connect_timeout": null,
            "execute_timeout": null,
            "read_timeout": null,
            "bearer_token_env_var": null,
            "oauth_resource": null
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    for field in [
        "command",
        "transport",
        "connect_timeout",
        "execute_timeout",
        "read_timeout",
        "oauth_resource",
    ] {
        assert_eq!(cleared[field], serde_json::Value::Null, "{field}");
    }
    assert_eq!(cleared["url"], "https://example.com/replacement");
    assert_eq!(cleared["has_bearer_token_env_var"], false);

    // Clearing the final endpoint is invalid and must not alter persisted state.
    let invalid = client
        .patch(format!("{base}/test-stdio"))
        .header(header::IF_MATCH, mcp_test_revision(&client, &base).await?)
        .json(&json!({ "url": null }))
        .send()
        .await?;
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    let retained: serde_json::Value = client
        .get(format!("{base}/test-stdio"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(retained["url"], "https://example.com/replacement");

    // 5. Disable the server.
    let disabled: serde_json::Value = client
        .post(format!("{base}/test-stdio/disable"))
        .header(header::IF_MATCH, mcp_test_revision(&client, &base).await?)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(disabled["action"], "disabled");
    assert_eq!(disabled["ok"], true);

    // Verify disabled state via GET.
    let after_disable: serde_json::Value = client
        .get(format!("{base}/test-stdio"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(after_disable["enabled"], false);

    // 6. Re-enable the server.
    let enabled: serde_json::Value = client
        .post(format!("{base}/test-stdio/enable"))
        .header(header::IF_MATCH, mcp_test_revision(&client, &base).await?)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(enabled["action"], "enabled");
    assert_eq!(enabled["ok"], true);

    // 7. Reconnect reports the real failure; use a missing local executable
    // so this management test never attempts the example remote endpoint.
    client
        .patch(format!("{base}/test-stdio"))
        .header(header::IF_MATCH, mcp_test_revision(&client, &base).await?)
        .json(&json!({"command":root.join("missing-mcp-executable"),"url":null}))
        .send()
        .await?
        .error_for_status()?;
    let reconnected: serde_json::Value = client
        .post(format!("{base}/test-stdio/reconnect"))
        .header(header::IF_MATCH, mcp_test_revision(&client, &base).await?)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(reconnected["action"], "reconnect_failed");
    assert_eq!(reconnected["ok"], false);
    assert_eq!(reconnected["connection"]["connected"], false);
    assert!(reconnected["connection"]["error"].as_str().is_some());

    // 8. Delete the server.
    let deleted: serde_json::Value = client
        .delete(format!("{base}/test-stdio"))
        .header(header::IF_MATCH, mcp_test_revision(&client, &base).await?)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(deleted["action"], "deleted");
    assert_eq!(deleted["ok"], true);

    // 9. GET after delete returns 404.
    let not_found = client.get(format!("{base}/test-stdio")).send().await?;
    assert_eq!(not_found.status(), 404);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn mcp_server_management_create_requires_command_or_url() -> Result<()> {
    let root =
        std::env::temp_dir().join(format!("codewhale-mcp-mgmt-validation-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    fs::create_dir_all(&root)?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root(root.clone(), sessions_dir).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    // Missing both command and url → 400.
    let resp = client
        .post(format!("http://{addr}/v1/apps/mcp/servers"))
        .json(&serde_json::json!({ "name": "bad-server" }))
        .send()
        .await?;
    assert_eq!(resp.status(), 400);

    // Missing name → 400.
    let resp = client
        .post(format!("http://{addr}/v1/apps/mcp/servers"))
        .json(&serde_json::json!({ "command": "echo" }))
        .send()
        .await?;
    assert_eq!(resp.status(), 400);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn mcp_server_management_redacts_credentials() -> Result<()> {
    let root = std::env::temp_dir().join(format!("codewhale-mcp-mgmt-redact-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    fs::create_dir_all(&root)?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root(root.clone(), sessions_dir).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let base = format!("http://{addr}/v1/apps/mcp/servers");

    // Create a server with sensitive fields.
    let created: serde_json::Value = client
        .post(&base)
        .header(header::IF_MATCH, mcp_test_revision(&client, &base).await?)
        .json(&serde_json::json!({
            "name": "secret-server",
            "url": "https://example.com/mcp",
            "env_headers": { "Authorization": "MY_SECRET_ENV_VAR" },
            "bearer_token_env_var": "MY_BEARER_ENV",
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    // The response must NOT contain the actual header value or env variable value.
    assert!(
        !created.to_string().contains("MY_SECRET_ENV_VAR_VALUE"),
        "credential values must be redacted from API responses"
    );
    // env_header_keys should list the header name (not the env-var value).
    assert_eq!(
        created["env_header_keys"]
            .as_array()
            .map(|a| a.iter().any(|v| v.as_str() == Some("Authorization"))),
        Some(true),
        "env_header_keys should list header names"
    );
    // has_bearer_token_env_var should be true, not the env var name.
    assert_eq!(created["has_bearer_token_env_var"], true);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn runtime_info_advertises_mcp_server_management() -> Result<()> {
    let root = std::env::temp_dir().join(format!("codewhale-mcp-capability-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root(root, sessions_dir).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let info: serde_json::Value = client
        .get(format!("http://{addr}/v1/runtime/info"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    assert_eq!(
        info["capabilities"]["mcp_server_management"], true,
        "runtime/info must advertise mcp_server_management capability"
    );

    handle.abort();
    Ok(())
}

// ─── Skill lifecycle API tests ──────────────────────────────────────────────

/// Create a minimal skill package under `root_dir/.codewhale/skills/<name>`.
/// Returns the skill dir path plus a digest that can be used in requests.
fn create_managed_skill(root_dir: &std::path::Path, name: &str) -> Result<(PathBuf, String)> {
    let skill_dir = root_dir.join(".codewhale").join("skills").join(name);
    fs::create_dir_all(&skill_dir)?;
    fs::write(
        skill_dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: test skill\n---\nbody\n"),
    )?;
    // Write an `.installed-from` marker so the mutation module considers it
    // managed (and therefore eligible for update/remove/trust).
    let digest = crate::skills::audit::compute_package_digest(&skill_dir).expect("package digest");
    crate::skills::install::write_installed_from_v2(
        &skill_dir,
        &format!("github:test/{name}"),
        None,
        "sha256:test",
        &digest,
        name,
    )?;
    Ok((skill_dir, digest))
}

#[tokio::test]
async fn skill_detail_returns_body_and_routing_metadata() -> Result<()> {
    let _env = lock_test_env();
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("runtime");
    let workspace = tmp.path().to_path_buf();
    let _config = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", tmp.path().join("config.toml"));
    crate::test_support::trust_workspace(&workspace);
    let sessions_dir = root.join("sessions");
    fs::create_dir_all(&root)?;

    // A skill with routing metadata and a body a client can activate.
    let skill_dir = workspace
        .join(".codewhale")
        .join("skills")
        .join("activatable");
    fs::create_dir_all(&skill_dir)?;
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: activatable\ndescription: Activate me\naliases-for: activate-me\nargument-hint: <target>\n---\nDo the thing.\n",
    )?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root_token_mobile_workspace(
            root,
            sessions_dir,
            None,
            false,
            workspace,
        )
        .await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let detail: serde_json::Value = client
        .get(format!("http://{addr}/v1/skills/activatable"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    assert_eq!(detail["name"], "activatable");
    assert_eq!(detail["description"], "Activate me");
    assert_eq!(detail["source"], "native");
    assert_eq!(detail["invocation"], "model+user");
    assert_eq!(detail["aliases"][0], "activate-me");
    assert!(
        detail["body"]
            .as_str()
            .is_some_and(|b| b.contains("Do the thing.")),
        "detail body must carry the SKILL.md instructions, got {:?}",
        detail["body"]
    );
    assert!(
        detail["path"]
            .as_str()
            .is_some_and(|p| p.ends_with("SKILL.md")),
        "native skill detail must name its SKILL.md path"
    );
    // The alias resolves to the same body, so a client can activate by either
    // spelling without a second lookup table.
    let by_alias: serde_json::Value = client
        .get(format!("http://{addr}/v1/skills/activate-me"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(by_alias["name"], "activatable");

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn skill_detail_404s_for_unknown_skill() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let resp = client
        .get(format!("http://{addr}/v1/skills/no-such-skill"))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn skill_list_rows_carry_routing_metadata() -> Result<()> {
    let _env = lock_test_env();
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("runtime");
    let workspace = tmp.path().to_path_buf();
    let _config = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", tmp.path().join("config.toml"));
    crate::test_support::trust_workspace(&workspace);
    let sessions_dir = root.join("sessions");
    fs::create_dir_all(&root)?;

    let skill_dir = workspace.join(".codewhale").join("skills").join("listed");
    fs::create_dir_all(&skill_dir)?;
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: listed\ndescription: Listed skill\ninvocation: explicit-only\n---\nbody\n",
    )?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root_token_mobile_workspace(
            root,
            sessions_dir,
            None,
            false,
            workspace,
        )
        .await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let list: serde_json::Value = client
        .get(format!("http://{addr}/v1/skills"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let listed = list["skills"]
        .as_array()
        .expect("skills array")
        .iter()
        .find(|sk| sk["name"] == "listed")
        .expect("listed skill present");
    assert_eq!(listed["invocation"], "explicit-only");
    assert_eq!(listed["is_bundled"], false);
    // A custom skill has no curated tier; the field is present but null.
    assert!(listed["bundled_tier"].is_null());

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn listed_tier_belongs_to_the_bundled_copy_only() -> Result<()> {
    let _env = lock_test_env();
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("runtime");
    let workspace = tmp.path().to_path_buf();
    let _config = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", tmp.path().join("config.toml"));
    crate::test_support::trust_workspace(&workspace);
    let sessions_dir = root.join("sessions");
    fs::create_dir_all(&root)?;

    // A bundled name at the bundled path: the shipped skill, grouped.
    let bundled = workspace.join(".codewhale").join("skills").join("help");
    fs::create_dir_all(&bundled)?;
    fs::write(
        bundled.join("SKILL.md"),
        "---\nname: help\ndescription: Shipped\n---\nbody\n",
    )?;
    // A bundled *name* somewhere else: a community skill that shares the name
    // of a shipped one. It is not the shipped copy, so it is not in the
    // shipped tier either — the two fields have to agree.
    let impostor = workspace.join(".agents").join("skills").join("pdf");
    fs::create_dir_all(&impostor)?;
    fs::write(
        impostor.join("SKILL.md"),
        "---\nname: pdf\ndescription: Mine, not the shipped one\n---\nbody\n",
    )?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root_token_mobile_workspace(
            root,
            sessions_dir,
            None,
            false,
            workspace,
        )
        .await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let list: serde_json::Value = client
        .get(format!("http://{addr}/v1/skills"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let rows = list["skills"].as_array().expect("skills array");
    let row = |name: &str| {
        rows.iter()
            .find(|sk| sk["name"] == name)
            .unwrap_or_else(|| panic!("{name} must be listed"))
    };
    assert_eq!(row("help")["is_bundled"], true);
    assert_eq!(row("help")["bundled_tier"], "tools");
    assert_eq!(row("pdf")["is_bundled"], false);
    assert!(
        row("pdf")["bundled_tier"].is_null(),
        "a non-bundled copy of a bundled name must not claim the shipped tier: {}",
        row("pdf")["bundled_tier"]
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn runtime_info_advertises_skill_detail_capability() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let info: serde_json::Value = client
        .get(format!("http://{addr}/v1/runtime/info"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        info["capabilities"]["skill_detail"], true,
        "runtime/info must advertise skill_detail capability"
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn skill_lifecycle_uninstall_removes_installed_skill() -> Result<()> {
    let _env = lock_test_env();
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("runtime");
    let workspace = tmp.path().to_path_buf();
    // Project skills load only in a trusted workspace; a sealed config path
    // lets the server's threads read the same trust record.
    let _config = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", tmp.path().join("config.toml"));
    crate::test_support::trust_workspace(&workspace);
    let sessions_dir = root.join("sessions");
    fs::create_dir_all(&root)?;

    create_managed_skill(&workspace, "hello")?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root_token_mobile_workspace(
            root,
            sessions_dir,
            None,
            false,
            workspace,
        )
        .await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    // Confirm skill is visible.
    let list: serde_json::Value = client
        .get(format!("http://{addr}/v1/skills"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert!(
        list["skills"]
            .as_array()
            .is_some_and(|s| s.iter().any(|sk| sk["name"] == "hello")),
        "hello skill must appear in GET /v1/skills before uninstall"
    );

    // Uninstall it.
    let resp = client
        .delete(format!("http://{addr}/v1/skills/hello"))
        .send()
        .await?
        .error_for_status()?
        .json::<serde_json::Value>()
        .await?;
    assert_eq!(resp["outcome"], "removed");
    assert_eq!(resp["name"], "hello");

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn skill_lifecycle_uninstall_404s_for_unknown_skill() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let resp = client
        .delete(format!("http://{addr}/v1/skills/no-such-skill"))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn skill_lifecycle_uninstall_rejects_invalid_scope() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let resp = client
        .delete(format!("http://{addr}/v1/skills/hello?scope=badscope"))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn skill_lifecycle_trust_marks_installed_skill() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("runtime");
    let workspace = tmp.path().to_path_buf();
    let sessions_dir = root.join("sessions");
    fs::create_dir_all(&root)?;

    create_managed_skill(&workspace, "trustme")?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root_token_mobile_workspace(
            root,
            sessions_dir,
            None,
            false,
            workspace,
        )
        .await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let resp = client
        .post(format!("http://{addr}/v1/skills/trustme/trust"))
        .json(&json!({}))
        .send()
        .await?
        .error_for_status()?
        .json::<serde_json::Value>()
        .await?;
    assert_eq!(resp["outcome"], "trusted");
    assert_eq!(resp["name"], "trustme");
    // The trust note must be present and must carry the advisory wording.
    let trust_note = resp["trust_note"].as_str().expect("trust_note");
    assert!(
        trust_note.contains("advisory") && trust_note.contains("digest-bound"),
        "trust_note must preserve advisory and digest-bound wording"
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn skill_lifecycle_trust_404s_for_unknown_skill() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let resp = client
        .post(format!("http://{addr}/v1/skills/no-such/trust"))
        .json(&json!({}))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn skill_lifecycle_trust_rejects_invalid_scope() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let resp = client
        .post(format!("http://{addr}/v1/skills/hello/trust"))
        .json(&json!({ "scope": "invalid" }))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn skill_lifecycle_trust_rejects_digest_drift() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("runtime");
    let workspace = tmp.path().to_path_buf();
    let sessions_dir = root.join("sessions");
    fs::create_dir_all(&root)?;

    create_managed_skill(&workspace, "drifted")?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root_token_mobile_workspace(
            root,
            sessions_dir,
            None,
            false,
            workspace,
        )
        .await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    // Pass a deliberately wrong digest to confirm drift detection.
    let resp = client
        .post(format!("http://{addr}/v1/skills/drifted/trust"))
        .json(&json!({ "expected_digest": "sha256:definitely_wrong_digest" }))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn skill_lifecycle_audit_returns_receipt_for_installed_skill() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("runtime");
    let workspace = tmp.path().to_path_buf();
    let sessions_dir = root.join("sessions");
    fs::create_dir_all(&root)?;

    create_managed_skill(&workspace, "auditable")?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root_token_mobile_workspace(
            root,
            sessions_dir,
            None,
            false,
            workspace,
        )
        .await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let resp: serde_json::Value = client
        .get(format!("http://{addr}/v1/skills/auditable/audit"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(resp["ambiguous"], false);
    let skills = resp["skills"].as_array().expect("skills array");
    assert_eq!(skills.len(), 1);
    let entry = &skills[0];
    assert_eq!(entry["name"], "auditable");
    assert_eq!(entry["source_kind"], "codewhale_managed");
    // Digest must be known for a properly written managed skill.
    assert_eq!(entry["digest"]["state"], "known");
    assert!(
        entry["digest"]["value"].as_str().is_some(),
        "digest.value must be present when state=known"
    );
    // Trust state: untrusted because we haven't run trust.
    assert_eq!(entry["trust"], "untrusted");

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn skill_lifecycle_audit_404s_for_unknown_skill() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let resp = client
        .get(format!("http://{addr}/v1/skills/no-such-skill/audit"))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn skill_lifecycle_audit_rejects_invalid_scope() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let resp = client
        .get(format!("http://{addr}/v1/skills/hello/audit?scope=nope"))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn skill_lifecycle_install_rejects_empty_source() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let resp = client
        .post(format!("http://{addr}/v1/skills/install"))
        .json(&json!({ "source": "  " }))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn skill_lifecycle_install_rejects_invalid_scope() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let resp = client
        .post(format!("http://{addr}/v1/skills/install"))
        .json(&json!({ "source": "github:owner/repo", "scope": "badscope" }))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn skill_lifecycle_update_rejects_invalid_scope() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let resp = client
        .post(format!("http://{addr}/v1/skills/hello/update"))
        .json(&json!({ "scope": "wrong" }))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn skill_lifecycle_endpoints_require_auth_when_token_is_set() -> Result<()> {
    let root = std::env::temp_dir().join(format!("codewhale-skill-auth-{}", Uuid::new_v4()));
    let sessions_dir = root.join("sessions");
    let token = "skill-lifecycle-test-token".to_string();
    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root_and_token(root, sessions_dir, Some(token)).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    // All skill lifecycle endpoints must require auth.
    for (method, path) in &[
        ("GET", "/v1/skills/any/audit"),
        ("POST", "/v1/skills/install"),
        ("POST", "/v1/skills/any/update"),
        ("DELETE", "/v1/skills/any"),
        ("POST", "/v1/skills/any/trust"),
        // The detail route hands back a SKILL.md body, so it is as sensitive
        // as the listing plus the file it names.
        ("GET", "/v1/skills/any"),
    ] {
        let resp = client
            .request(
                reqwest::Method::from_bytes(method.as_bytes()).unwrap(),
                format!("http://{addr}{path}"),
            )
            .json(&json!({}))
            .send()
            .await?;
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "{method} {path} must require auth"
        );
    }

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn skill_lifecycle_runtime_info_advertises_skill_lifecycle_capability() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let info: serde_json::Value = client
        .get(format!("http://{addr}/v1/runtime/info"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        info["capabilities"]["skill_lifecycle"], true,
        "runtime/info must advertise skill_lifecycle capability"
    );

    handle.abort();
    Ok(())
}

#[test]
fn receipt_evidence_paths_must_stay_confined_to_the_workspace() {
    use std::path::Path;

    assert!(crate::fleet::artifacts::path_is_confined(Path::new(
        "receipts/run-1/task-a.json"
    )));
    assert!(!crate::fleet::artifacts::path_is_confined(Path::new(
        "/etc/passwd"
    )));
    assert!(!crate::fleet::artifacts::path_is_confined(Path::new(
        "receipts/../../escape.json"
    )));
    assert!(!crate::fleet::artifacts::path_is_confined(Path::new(
        "../escape.json"
    )));
}

// ─── Plugin management API tests ────────────────────────────────────────────

/// Write a minimal but real plugin bundle: manifest plus one skill, so the
/// bundle has a supported declarative component and can be trusted+enabled.
fn write_plugin_source_bundle(dir: &Path, name: &str) -> Result<PathBuf> {
    let bundle = dir.join(name);
    fs::create_dir_all(bundle.join("skills").join("greet"))?;
    fs::write(
        bundle.join("plugin.toml"),
        format!(
            "schema_version = 1\n[plugin]\nname = \"{name}\"\nversion = \"1.0.0\"\n\
             description = \"test bundle\"\n\n[skills]\npath = \"skills\"\n"
        ),
    )?;
    fs::write(
        bundle.join("skills").join("greet").join("SKILL.md"),
        "---\nname: greet\ndescription: Greets a person\n---\nSay hi.\n",
    )?;
    Ok(bundle)
}

/// An isolated plugin discovery context: user plugins, workspace plugins,
/// state, and marketplaces all live under the test root instead of the real
/// `~/.codewhale`.
fn isolated_plugin_discovery(
    root: &Path,
    workspace: &Path,
) -> Arc<crate::plugins::PluginDiscoveryContext> {
    crate::plugins::PluginDiscoveryContext::from_config_and_environment(
        &crate::plugins::discovery::DiscoveryConfig {
            workspace: workspace.to_path_buf(),
            user_plugins_dir: root.join("plugins"),
            workspace_plugins_dir: crate::plugins::discovery::default_workspace_plugins_dir(
                workspace,
            ),
            builtin_plugin_dirs: Vec::new(),
            state_path: root.join("plugins").join("state.json"),
        },
        crate::plugins::HostEnvironment::from_entries(Vec::new()),
    )
}

async fn spawn_plugin_api_server(
    root: PathBuf,
    workspace: PathBuf,
) -> Result<Option<(SocketAddr, tokio::task::JoinHandle<()>)>> {
    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root_token_mobile_workspace_and_overrides(
            root.clone(),
            root.join("sessions"),
            None,
            false,
            workspace.clone(),
            TestServerOverrides {
                plugin_discovery: Some(isolated_plugin_discovery(&root, &workspace)),
                ..TestServerOverrides::default()
            },
        )
        .await?
    else {
        return Ok(None);
    };
    Ok(Some((addr, handle)))
}

#[tokio::test]
async fn runtime_info_advertises_plugin_management_capability() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let info: serde_json::Value = client
        .get(format!("http://{addr}/v1/runtime/info"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        info["capabilities"]["plugin_management"], true,
        "runtime/info must advertise plugin_management capability"
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn runtime_info_advertises_terminal_capabilities() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let info: serde_json::Value = client
        .get(format!("http://{addr}/v1/runtime/info"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    // A GPUI client gates its terminal pane on these. They are true where the
    // routes serve bytes and false where they answer 501 (Windows, OpenHarmony)
    // — the flag must not claim a capability the build cannot serve, so assert
    // the routes' own gate rather than `true`.
    let expected = cfg!(all(unix, not(target_env = "ohos")));
    for capability in [
        "terminal_stream",
        "terminal_input",
        "terminal_resize",
        "terminal_kill",
    ] {
        assert_eq!(
            info["capabilities"][capability], expected,
            "runtime/info must advertise {capability}={expected}"
        );
    }

    handle.abort();
    Ok(())
}

#[tokio::test]
#[cfg(unix)]
async fn terminal_routes_serve_a_live_engine_session_over_http() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("runtime");
    let workspace = tmp.path().join("ws");
    fs::create_dir_all(&root)?;
    fs::create_dir_all(&workspace)?;
    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root_token_mobile_workspace(
            root.clone(),
            root.join("sessions"),
            None,
            false,
            workspace.clone(),
        )
        .await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let base = format!("http://{addr}/v1/terminal/pane");

    // The agent's terminal tools own session creation; stand in for that
    // producer on the same workspace the server was started with, which is
    // what makes this the Engine's own shell rather than a second one.
    let _session = crate::tools::terminal_session::get_or_create(
        "pane",
        &workspace,
        crate::sandbox::SandboxPolicy::DangerFullAccess,
    )
    .map_err(anyhow::Error::msg)?;

    // Input and command output through the route, without direct session
    // access. Start output on its own line even if the shell paints a prompt.
    let write: serde_json::Value = client
        .post(format!("{base}/input"))
        .json(&serde_json::json!({
            "data": "printf '\\nterminal-route-proof\\n'\n",
            "encoding": "text"
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert!(write["written"].as_u64().unwrap_or_default() > 0);

    let read_chunk = |base: String, client: reqwest::Client| async move {
        let chunk: serde_json::Value = client
            .get(format!("{base}/output?cursor=0&format=text"))
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json()
            .await
            .ok()?;
        Some(chunk)
    };

    let deadline = std::time::Instant::now() + ci_scaled(Duration::from_secs(10));
    loop {
        let chunk = read_chunk(base.clone(), client.clone())
            .await
            .expect("terminal output route answers");
        let data = chunk["data"].as_str().unwrap_or_default();
        if data
            .lines()
            .any(|line| line.trim() == "terminal-route-proof")
        {
            // Wait for the command's output, not its echoed input. Reads are
            // non-consuming, but the shell can append its prompt between them.
            let again = read_chunk(base.clone(), client.clone())
                .await
                .expect("terminal output route answers");
            assert!(
                again["data"].as_str().unwrap_or_default().starts_with(data),
                "a repeated read must retain every byte already observed"
            );
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "route never delivered the shell's output: {data}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Resize is checked through the shell, not the handler: `stty size` reads
    // the kernel's window, so a handler that only stored the numbers fails.
    client
        .post(format!("{base}/resize"))
        .json(&serde_json::json!({"rows": 40, "cols": 100}))
        .send()
        .await?
        .error_for_status()?;
    client
        .post(format!("{base}/input"))
        .json(&serde_json::json!({"data": "stty size\n", "encoding": "text"}))
        .send()
        .await?
        .error_for_status()?;
    let deadline = std::time::Instant::now() + ci_scaled(Duration::from_secs(10));
    loop {
        let chunk = read_chunk(base.clone(), client.clone())
            .await
            .expect("terminal output route answers");
        let data = chunk["data"].as_str().unwrap_or_default();
        if data.contains("40 100") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "resize never reached the shell: {data}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Kill, then learn the truth from the stream rather than the ack.
    client
        .post(format!("{base}/kill"))
        .send()
        .await?
        .error_for_status()?;
    let deadline = std::time::Instant::now() + ci_scaled(Duration::from_secs(10));
    loop {
        let chunk = read_chunk(base.clone(), client.clone())
            .await
            .expect("terminal output route answers");
        if chunk["running"] == serde_json::json!(false) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "killed session still reports running: {chunk}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    handle.abort();
    Ok(())
}

#[tokio::test]
#[cfg(unix)]
async fn terminal_output_for_an_unknown_session_is_not_found_and_creates_nothing() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let base = format!("http://{addr}/v1/terminal");

    // The route exists and answers for a name that has no live session: the
    // Engine attaches to shells it owns, it does not conjure one per request.
    let missing = client
        .get(format!("{base}/no-such-session/output"))
        .send()
        .await?;
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

    // An over-long name is rejected as a miss too, so the registry is never
    // asked to allocate for it.
    let long_name = "n".repeat(200);
    let oversized = client
        .get(format!("{base}/{long_name}/output"))
        .send()
        .await?;
    assert_eq!(oversized.status(), reqwest::StatusCode::NOT_FOUND);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn plugin_lifecycle_over_http_installs_reviews_enables_and_uninstalls() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("runtime");
    let workspace = tmp.path().join("ws");
    fs::create_dir_all(&root)?;
    let source = write_plugin_source_bundle(tmp.path(), "demo")?;

    let Some((addr, handle)) = spawn_plugin_api_server(root, workspace).await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let base = format!("http://{addr}/v1/apps/plugins");

    // Empty inventory to start.
    let list: serde_json::Value = client
        .get(&base)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert!(list["plugins"].as_array().is_some_and(Vec::is_empty));

    // Install from a local path: lands disabled and untrusted.
    let install: serde_json::Value = client
        .post(format!("{base}/install"))
        .json(&serde_json::json!({
            "source": format!("path:{}", source.display())
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(install["outcome"], "installed");
    assert_eq!(install["name"], "demo");
    assert_eq!(install["plugin"]["state"], "disabled");
    assert_eq!(install["plugin"]["trust_status"], "not-reviewed");
    assert!(
        install["note"]
            .as_str()
            .is_some_and(|n| n.contains("untrusted")),
        "install note must route to review: {}",
        install["note"]
    );

    // Detail carries the structured review payload and token. The summary is
    // flattened into the detail object.
    let detail: serde_json::Value = client
        .get(format!("{base}/demo"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(detail["inventory"]["skills"], 1);
    assert_eq!(detail["review"]["token"], {
        let summary = &detail;
        format!(
            "{}.{}",
            summary["content_hash"].as_str().unwrap(),
            summary["capability_hash"].as_str().unwrap()
        )
    });
    assert!(
        detail["review"]["capabilities"]
            .as_array()
            .is_some_and(|c| c.iter().any(|label| label == "skills"))
    );

    // Trust requires the exact review token.
    let wrong = client
        .post(format!("{base}/demo/trust"))
        .json(&serde_json::json!({"token": "not-the-token"}))
        .send()
        .await?;
    assert_eq!(wrong.status(), StatusCode::BAD_REQUEST);

    let token = detail["review"]["token"].as_str().unwrap().to_string();
    let trusted: serde_json::Value = client
        .post(format!("{base}/demo/trust"))
        .json(&serde_json::json!({"token": token}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(trusted["action"], "trusted");
    assert_eq!(trusted["state"], "disabled");

    // Enable only works after trust; the bundle becomes active.
    let enabled: serde_json::Value = client
        .post(format!("{base}/demo/enable"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(enabled["action"], "enabled");
    assert_eq!(enabled["state"], "active");

    let disabled: serde_json::Value = client
        .post(format!("{base}/demo/disable"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(disabled["action"], "disabled");
    assert_eq!(disabled["state"], "disabled");

    // Uninstall removes it.
    let uninstalled: serde_json::Value = client
        .delete(format!("{base}/demo"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(uninstalled["outcome"], "uninstalled");

    let list: serde_json::Value = client
        .get(&base)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert!(list["plugins"].as_array().is_some_and(Vec::is_empty));

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn plugin_api_404s_for_unknown_selector() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("runtime");
    let workspace = tmp.path().join("ws");
    fs::create_dir_all(&root)?;

    let Some((addr, handle)) = spawn_plugin_api_server(root, workspace).await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let detail = client
        .get(format!("http://{addr}/v1/apps/plugins/ghost"))
        .send()
        .await?;
    assert_eq!(detail.status(), StatusCode::NOT_FOUND);

    let trust = client
        .post(format!("http://{addr}/v1/apps/plugins/ghost/trust"))
        .json(&serde_json::json!({"token": "x"}))
        .send()
        .await?;
    assert_eq!(trust.status(), StatusCode::NOT_FOUND);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn dsh_package_preview_then_exact_install_over_http() -> Result<()> {
    let _home = crate::test_support::SealedHome::new();
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("runtime");
    let workspace = tmp.path().join("ws");
    fs::create_dir_all(&root)?;
    let package = tmp.path().join("dsh-package");
    fs::create_dir_all(&package)?;
    fs::write(
        package.join("package.json"),
        r#"{"name": "@demo/docs-dsh", "dsh": {"bundle": {"patch": "./cordis.patch.yml"}}}"#,
    )?;
    fs::write(
        package.join("cordis.patch.yml"),
        "- insert:\n  - id: docs\n    name: '@deepseek-ai/dsh-mcp-client'\n    config: {serverName: docs, transport: streamable-http, url: 'https://docs.example.invalid/mcp'}\n",
    )?;
    let Some((addr, handle)) = spawn_plugin_api_server(root, workspace).await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let mut preview_response = client
        .post(format!("http://{addr}/v1/apps/plugins/import/dsh/preview"))
        .json(&serde_json::json!({"path": package.display().to_string()}))
        .send()
        .await?;
    if !preview_response.status().is_success() {
        let status = preview_response.status();
        let mut diagnostic = Vec::new();
        while let Some(chunk) = preview_response.chunk().await? {
            let remaining = 1024 - diagnostic.len();
            diagnostic.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
            if diagnostic.len() == 1024 {
                break;
            }
        }
        anyhow::bail!(
            "DSH fixture preview {status}: {}",
            String::from_utf8_lossy(&diagnostic)
        );
    }
    let preview: serde_json::Value = preview_response.json().await?;
    assert_eq!(preview["conversion"]["plugin_name"], "docs-dsh");
    assert_eq!(
        preview["conversion"]["network_hosts"],
        serde_json::json!(["docs.example.invalid"])
    );
    let plugins: serde_json::Value = client
        .get(format!("http://{addr}/v1/apps/plugins"))
        .send()
        .await?
        .json()
        .await?;
    assert!(
        !plugins["plugins"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "docs-dsh"),
        "preview installs nothing"
    );

    let installed = client
        .post(format!("http://{addr}/v1/apps/plugins/install"))
        .json(&serde_json::json!({
            "source": preview["install_source"],
            "expected_content_hash": preview["content_hash"],
        }))
        .send()
        .await?;
    assert_eq!(installed.status(), StatusCode::CREATED);
    let installed: serde_json::Value = installed.json().await?;
    assert_eq!(installed["name"], "docs-dsh");
    assert_eq!(installed["plugin"]["enabled"], false);
    assert_ne!(installed["plugin"]["trust_status"], "trusted");

    let refused = client
        .post(format!("http://{addr}/v1/apps/plugins/import/dsh/preview"))
        .json(&serde_json::json!({"path": tmp.path().join("missing").display().to_string()}))
        .send()
        .await?;
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    handle.abort();
    Ok(())
}

#[tokio::test]
async fn marketplace_catalog_lifecycle_over_http_lists_installs_and_removes() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("runtime");
    let workspace = tmp.path().join("ws");
    fs::create_dir_all(&root)?;

    // The catalog directory holds both the document and the bundle it points
    // at via a relative `./demo` source.
    let catalog_dir = tmp.path().join("catalog");
    fs::create_dir_all(&catalog_dir)?;
    write_plugin_source_bundle(&catalog_dir, "demo")?;
    let catalog_path = catalog_dir.join("catalog.json");
    fs::write(
        &catalog_path,
        r#"{
            "name": "team",
            "description": "Team plugins",
            "version": "1",
            "plugins": [
                {"name": "demo", "source": "path:./demo", "description": "Demo bundle"}
            ]
        }"#,
    )?;

    let Some((addr, handle)) = spawn_plugin_api_server(root, workspace).await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let base = format!("http://{addr}/v1/apps/marketplaces");

    let add_resp = client
        .post(&base)
        .json(&serde_json::json!({
            "name": "team",
            "path": catalog_path.display().to_string()
        }))
        .send()
        .await?;
    assert_eq!(add_resp.status(), StatusCode::CREATED);
    let add: serde_json::Value = add_resp.json().await?;
    assert!(add["action"] == "added", "marketplace add failed: {add}");
    assert_eq!(add["candidate_count"], 1);

    // Listing shows the candidate with an honest, resolved install plan.
    let list: serde_json::Value = client
        .get(&base)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let team = list["marketplaces"]
        .as_array()
        .expect("marketplace list")
        .iter()
        .find(|catalog| catalog["name"] == "team")
        .expect("added team catalog");
    let candidate = &team["candidates"][0];
    assert_eq!(candidate["name"], "demo");
    assert_eq!(candidate["install"]["installable"], true);
    assert!(
        candidate["install"]["spec"]
            .as_str()
            .is_some_and(|spec| spec.contains("demo")),
        "relative ./demo must resolve against the catalog directory: {}",
        candidate["install"]["spec"]
    );

    // Install through the marketplace routes into the reviewed installer and
    // lands disabled + untrusted, exactly like a direct install.
    let install: serde_json::Value = client
        .post(format!("{base}/team/install"))
        .json(&serde_json::json!({"candidate": "demo"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(install["outcome"], "installed");
    assert_eq!(install["plugin"]["trust_status"], "not-reviewed");

    // An installed name routes to its actual local bundle. Catalog metadata
    // cannot authorize a replacement or certify matching publisher/bytes.
    let after: serde_json::Value = client
        .get(format!("{base}/team"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(after["candidates"][0]["install"]["installable"], false);
    assert_eq!(
        after["candidates"][0]["existing_plugin"]["content_hash"],
        install["plugin"]["content_hash"]
    );
    assert_eq!(
        after["candidates"][0]["existing_plugin"]["trust_status"],
        "not-reviewed"
    );
    let again = client
        .post(format!("{base}/team/install"))
        .json(&serde_json::json!({"candidate":"demo"}))
        .send()
        .await?;
    assert_eq!(again.status(), StatusCode::CONFLICT);
    let direct = client
        .post(format!("http://{addr}/v1/apps/plugins/install"))
        .json(&serde_json::json!({"source":catalog_dir.join("demo").display().to_string()}))
        .send()
        .await?;
    assert_eq!(direct.status(), StatusCode::CONFLICT);

    // Unknown candidate and unknown catalog are honest 404s.
    let missing = client
        .post(format!("{base}/team/install"))
        .json(&serde_json::json!({"candidate": "ghost"}))
        .send()
        .await?;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    // Removing the catalog never touches installed bundles.
    let plugins: serde_json::Value = client
        .get(format!("http://{addr}/v1/apps/plugins"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert!(
        plugins["plugins"]
            .as_array()
            .is_some_and(|p| p.iter().any(|entry| entry["name"] == "demo"))
    );

    let removed: serde_json::Value = client
        .delete(format!("{base}/team"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(removed["action"], "removed");

    let plugins_after: serde_json::Value = client
        .get(format!("http://{addr}/v1/apps/plugins"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert!(
        plugins_after["plugins"]
            .as_array()
            .is_some_and(|p| p.iter().any(|entry| entry["name"] == "demo"))
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn marketplace_builtin_name_is_reviewable_without_install_or_network_access() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("runtime");
    let workspace = tmp.path().join("ws");
    fs::create_dir_all(&root)?;
    let bundle = write_plugin_source_bundle(&tmp.path().join("builtin"), "computer-use")?;
    let discovery = crate::plugins::PluginDiscoveryContext::from_config_and_environment(
        &crate::plugins::discovery::DiscoveryConfig {
            workspace: workspace.clone(),
            user_plugins_dir: root.join("plugins"),
            workspace_plugins_dir: crate::plugins::discovery::default_workspace_plugins_dir(
                &workspace,
            ),
            builtin_plugin_dirs: vec![bundle.parent().unwrap().to_path_buf()],
            state_path: root.join("plugins/state.json"),
        },
        crate::plugins::HostEnvironment::from_entries(Vec::new()),
    );
    let Some((addr, _, handle)) = spawn_test_server_with_root_token_mobile_workspace_and_overrides(
        root.clone(),
        root.join("sessions"),
        None,
        false,
        workspace,
        TestServerOverrides {
            plugin_discovery: Some(discovery),
            ..TestServerOverrides::default()
        },
    )
    .await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let base = format!("http://{addr}/v1/apps");
    let catalog: serde_json::Value = client
        .get(format!("{base}/marketplaces/codewhale"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let candidates = catalog["candidates"].as_array().unwrap();
    let candidate = candidates
        .iter()
        .find(|p| p["name"] == "computer-use")
        .unwrap();
    assert_eq!(candidate["install"]["installable"], false);
    assert_eq!(candidate["existing_plugin"]["scope"], "builtin");
    assert_eq!(candidate["existing_plugin"]["version"], "1.0.0");
    assert_eq!(candidate["existing_plugin"]["trust_status"], "not-reviewed");
    assert!(
        candidates
            .iter()
            .filter(|p| p["name"] != "computer-use")
            .all(|p| p["install"]["installable"] == true)
    );
    // No host is approved: getting 409 rather than network-policy 403 proves
    // the occupied-name preflight ran before any download was admitted.
    let response = client
        .post(format!("{base}/marketplaces/codewhale/install"))
        .json(&serde_json::json!({"candidate":"computer-use"}))
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert!(!root.join("plugins/computer-use").exists());
    let detail: serde_json::Value = client
        .get(format!("{base}/plugins/computer-use"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        detail["content_hash"],
        candidate["existing_plugin"]["content_hash"]
    );
    assert_eq!(detail["trust_status"], "not-reviewed");
    assert_eq!(detail["enabled"], false);
    assert!(!detail["review"]["token"].as_str().unwrap().is_empty());
    assert!(bundle.join("plugin.toml").is_file());
    handle.abort();
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn marketplace_add_rejects_symlink_documents_over_http() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("runtime");
    let workspace = tmp.path().join("ws");
    fs::create_dir_all(&root)?;

    let catalog_dir = tmp.path().join("catalog");
    fs::create_dir_all(&catalog_dir)?;
    let real = catalog_dir.join("real.json");
    fs::write(&real, r#"{"plugins":[]}"#)?;
    let link = catalog_dir.join("link.json");
    std::os::unix::fs::symlink(&real, &link)?;

    let Some((addr, handle)) = spawn_plugin_api_server(root, workspace).await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let resp = client
        .post(format!("http://{addr}/v1/apps/marketplaces"))
        .json(&serde_json::json!({
            "name": "team",
            "path": link.display().to_string()
        }))
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn stream_compat_mapping_forwards_runtime_store_failures() -> Result<()> {
    // #5931: compat clients see the operator's store fault with its file.
    let event = RuntimeEventRecord {
        schema_version: 1,
        seq: 9,
        timestamp: chrono::Utc::now(),
        thread_id: "thr_test".to_string(),
        turn_id: Some("turn_test".to_string()),
        item_id: Some("item_1".to_string()),
        event: crate::runtime_threads::RUNTIME_STORE_FAILURE_EVENT.to_string(),
        payload: json!({
            "operation": "read",
            "record_kind": "item",
            "record_id": "item_1",
            "path": "/tmp/runtime/items/item_1.json",
            "error": "Failed to read item /tmp/runtime/items/item_1.json: No such file",
            "reason": "No such file",
            "next_action": "Move /tmp/runtime/items/item_1.json aside (or delete it) and retry.",
            "message": "Session runtime store: item item_1 at /tmp/runtime/items/item_1.json could not be read: No such file.",
        }),
    };
    let mapped = map_compat_stream_event(&event).context("missing store failure event")?;
    let stream = async_stream::stream! {
        yield Ok::<_, Infallible>(mapped);
    };
    let body =
        axum::body::to_bytes(Sse::new(stream).into_response().into_body(), usize::MAX).await?;
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("event: runtime.store_failure"), "{text}");
    assert!(text.contains("/tmp/runtime/items/item_1.json"), "{text}");
    Ok(())
}

#[tokio::test]
async fn reload_config_updates_openrouter_vendor_for_new_clients_only() -> Result<()> {
    let _env = crate::test_support::lock_test_env();
    let temp = tempfile::tempdir()?;
    let config_file = temp.path().join("vendor-pin.toml");
    fs::write(&config_file, "# initial\n")?;
    let (addr, manager, handle) = spawn_test_server_with_config_path(config_file.clone())
        .await?
        .expect("local Runtime API must be available for reload regression");
    let client = crate::tls::reqwest_client();
    for vendor in ["deepinfra/turbo", "another-vendor/region", ""] {
        fs::write(
            &config_file,
            format!(
                r#"
provider = "openrouter"
[providers.openrouter]
api_key = "vendor-reload-local-fixture"
base_url = "https://openrouter.ai/api/v1"
model = "deepseek/deepseek-v4-pro"
vendor = "{vendor}"
"#
            ),
        )?;
        let old_config = manager.read_config().clone();
        let old_vendor = old_config.openrouter_vendor()?;
        let old_client = if old_config.active_provider_identity().unwrap().provider
            == crate::config::ProviderKind::Openrouter
        {
            Some(crate::client::CodewhaleClient::new(&old_config)?)
        } else {
            None
        };
        let reload = client
            .post(format!("http://{addr}/v1/config/reload"))
            .send()
            .await?;
        assert_eq!(reload.status(), StatusCode::OK);
        let fresh = crate::client::CodewhaleClient::new(&manager.read_config())?;
        assert_eq!(
            fresh.openrouter_vendor(),
            (!vendor.is_empty()).then_some(vendor)
        );
        if let Some(old) = old_client {
            assert_eq!(old.openrouter_vendor(), old_vendor.as_deref());
        }
    }
    handle.abort();
    Ok(())
}

#[test]
fn oauth_pass_through_provider_lists_catalog_models_without_a_live_listing() {
    let _env = crate::test_support::lock_test_env();
    let _live = crate::provider_lake::lock_live_snapshot();
    let home = tempfile::tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
    crate::provider_catalog_live::reset_cache_for_test();
    crate::provider_lake::clear_live_snapshot();
    let mut config = Config {
        provider: Some("deepseek".into()),
        ..Default::default()
    };
    // Before the OAuth opt-in, a pass-through provider with no live listing
    // offers only its configured model.
    assert!(
        provider_models_for_api(&config, &(config).test_identity_for_kind(ProviderKind::Xai))
            .is_empty()
    );

    config
        .provider_config_for_mut(&config.test_identity_for_kind(ProviderKind::Xai))
        .unwrap()
        .auth_mode = Some("oauth".into());
    assert!(crate::provider_lake::live_catalog_unavailable(
        &config,
        &(config).test_identity_for_kind(ProviderKind::Xai)
    ));
    let models =
        provider_models_for_api(&config, &(config).test_identity_for_kind(ProviderKind::Xai));
    assert!(
        models.iter().any(|model| model == "grok-4.7"),
        "OAuth xAI must fall back to the bundled catalog: {models:?}"
    );
}

#[test]
fn api_provider_default_and_model_list_follow_exact_local_catalog() {
    use codewhale_config::catalog::{
        CatalogOffering, CatalogRefreshError, CatalogSource, ProviderCatalogDelta,
        base_url_fingerprint, now_unix,
    };

    let _env = crate::test_support::lock_test_env();
    let _live = crate::provider_lake::lock_live_snapshot();
    let home = tempfile::tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
    crate::provider_catalog_live::reset_cache_for_test();
    crate::provider_lake::clear_live_snapshot();
    let provider = ProviderKind::Ollama;
    let endpoint = "http://localhost:11455/v1";
    let mut config = Config {
        provider: Some("ollama".into()),
        ..Default::default()
    };
    config
        .provider_config_for_mut(&config.test_identity_for_kind(provider))
        .unwrap()
        .base_url = Some(endpoint.into());
    assert_eq!(
        provider_default_model_for_api(&config, &(config).test_identity_for_kind(provider)),
        ""
    );
    assert!(
        !provider_models_for_api(&config, &(config).test_identity_for_kind(provider))
            .contains(&"unknown".to_string())
    );
    assert!(runtime_request_model(&config, None).is_err());
    assert_eq!(
        runtime_request_model(&config, Some("auto")).unwrap(),
        "auto"
    );
    assert_eq!(
        runtime_request_model(&config, Some("chosen:tag")).unwrap(),
        "chosen:tag"
    );
    let fingerprint = base_url_fingerprint(endpoint);
    let fetched_at = now_unix();
    let ticket =
        crate::provider_catalog_live::begin_refresh_for_identity(provider, "ollama", endpoint);
    crate::provider_catalog_live::record_success_if_current(
        &ticket,
        ProviderCatalogDelta {
            provider: "ollama".into(),
            base_url_fingerprint: fingerprint.clone(),
            fetched_at,
            offerings: vec![CatalogOffering {
                provider: "ollama".into(),
                wire_model_id: "local-default:tag".into(),
                endpoint_key: "chat".into(),
                source: CatalogSource::Live {
                    base_url_fingerprint: fingerprint.clone(),
                    fetched_at,
                },
                ..Default::default()
            }],
        },
    );
    assert_eq!(
        provider_default_model_for_api(&config, &(config).test_identity_for_kind(provider)),
        "local-default:tag"
    );
    assert_eq!(
        provider_models_for_api(&config, &(config).test_identity_for_kind(provider)),
        vec!["local-default:tag"]
    );
    assert_eq!(
        runtime_request_model(&config, None).unwrap(),
        "local-default:tag"
    );
    let mut other = config.clone();
    other
        .provider_config_for_mut(&other.test_identity_for_kind(provider))
        .unwrap()
        .base_url = Some("http://localhost:11456/v1".into());
    assert_eq!(
        provider_default_model_for_api(&other, &(other).test_identity_for_kind(provider)),
        ""
    );
    assert!(provider_models_for_api(&other, &(other).test_identity_for_kind(provider)).is_empty());
    crate::provider_catalog_live::record_failure_if_current(
        &ticket,
        "ollama",
        &fingerprint,
        CatalogRefreshError::Network,
    );
    assert_eq!(
        provider_default_model_for_api(&config, &(config).test_identity_for_kind(provider)),
        ""
    );
    assert!(runtime_request_model(&config, None).is_err());
    config
        .set_provider_model_override(
            &config.test_identity_for_kind(provider),
            Some("chosen:tag".into()),
        )
        .unwrap();
    assert_eq!(
        provider_default_model_for_api(&config, &(config).test_identity_for_kind(provider)),
        "chosen:tag"
    );
    crate::provider_catalog_live::reset_cache_for_test();
    crate::provider_lake::clear_live_snapshot();
}

#[tokio::test(flavor = "current_thread")]
async fn api_config_reports_local_default_availability_without_blocking_config_reads() -> Result<()>
{
    use codewhale_config::catalog::{
        CatalogOffering, CatalogSource, ProviderCatalogDelta, base_url_fingerprint, now_unix,
    };

    let _env = lock_test_env();
    let _live = crate::provider_lake::lock_live_snapshot();
    let home = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
    crate::provider_catalog_live::reset_cache_for_test();
    crate::provider_lake::clear_live_snapshot();
    let config_file = home.path().join("config.toml");
    let endpoint = "http://localhost:11457/v1";
    fs::write(
        &config_file,
        format!("provider = \"ollama\"\n[providers.ollama]\nbase_url = \"{endpoint}\"\n"),
    )?;
    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let unavailable = get_config(&client, &addr).await;
    assert_eq!(unavailable["model"], "");
    assert_eq!(unavailable["model_available"], false);
    assert_eq!(
        unavailable["default_model"], DEFAULT_TEXT_MODEL,
        "legacy root field stays unchanged"
    );
    for path in ["tasks", "stream"] {
        let response = client
            .post(format!("http://{addr}/v1/{path}"))
            .json(&json!({"prompt": "missing local model"}))
            .send()
            .await?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(
            response
                .text()
                .await?
                .contains("no available default model")
        );
    }
    let fetched_at = now_unix();
    let fingerprint = base_url_fingerprint(endpoint);
    let ticket = crate::provider_catalog_live::begin_refresh_for_identity(
        ProviderKind::Ollama,
        "ollama",
        endpoint,
    );
    crate::provider_catalog_live::record_success_if_current(
        &ticket,
        ProviderCatalogDelta {
            provider: "ollama".into(),
            base_url_fingerprint: fingerprint.clone(),
            fetched_at,
            offerings: vec![CatalogOffering {
                provider: "ollama".into(),
                wire_model_id: "available:tag".into(),
                endpoint_key: "chat".into(),
                source: CatalogSource::Live {
                    base_url_fingerprint: fingerprint,
                    fetched_at,
                },
                ..Default::default()
            }],
        },
    );
    let available = get_config(&client, &addr).await;
    assert_eq!(available["model"], "available:tag");
    assert_eq!(available["model_available"], true);
    assert_eq!(available["default_model"], DEFAULT_TEXT_MODEL);
    handle.abort();
    crate::provider_catalog_live::reset_cache_for_test();
    crate::provider_lake::clear_live_snapshot();
    Ok(())
}

#[tokio::test]
async fn native_notification_settings_use_the_shared_profile_leaf_writer_and_reload() -> Result<()>
{
    let _env = lock_test_env();
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("config.toml");
    let original = r#"# retained owner comment
[notifications]
quiet = true
sound = "off"
[profiles.desktop.notifications]
quiet = false # selected owner
sound = "off"
[profiles.other.notifications]
quiet = true
[future_plugin]
keep = "untouched"
"#;
    fs::write(&path, original)?;
    let (addr, _, handle) =
        spawn_test_server_with_config_path_and_profile(path.clone(), "desktop".into())
            .await?
            .expect("loopback Runtime must be available");
    let client = crate::tls::reqwest_client();
    let reload = || client.post(format!("http://{addr}/v1/config/reload"));
    assert_eq!(reload().send().await?.status(), StatusCode::OK);
    let before = get_config(&client, &addr).await;
    assert_eq!(before["notifications"].as_object().unwrap().len(), 19);
    assert_eq!(before["notifications"]["quiet"], "false");
    for persist in [false, true] {
        for (key, value) in [
            ("notifications.quiet", "maybe"),
            ("notifications.typo", "true"),
            ("notifications.threshold_secs", "-1"),
            ("notifications.method", "macos"),
        ] {
            let (status, _) = post_set_config(&client, &addr, key, value, persist).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{key}");
        }
    }
    assert_eq!(fs::read_to_string(&path)?, original);
    let (status, dry) =
        post_set_config(&client, &addr, "notifications.threshold", "0", false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(dry["key"], "notifications.threshold_secs");
    assert_eq!(dry["persisted"], false);
    assert_eq!(fs::read_to_string(&path)?, original);
    let (status, saved) = post_set_config(&client, &addr, "notifications.quiet", "on", true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["value"], "true");
    assert_eq!(saved["persisted"], true);
    assert_eq!(saved["requires_reload"], true);
    assert_eq!(
        get_config(&client, &addr).await["notifications"]["quiet"],
        "false",
        "persist is not apply"
    );
    let text = fs::read_to_string(&path)?;
    assert!(text.contains("# retained owner comment") && text.contains("# selected owner"));
    let doc: toml::Value = toml::from_str(&text)?;
    assert_eq!(doc["notifications"]["quiet"].as_bool(), Some(true));
    assert_eq!(
        doc["profiles"]["desktop"]["notifications"]["quiet"].as_bool(),
        Some(true)
    );
    assert_eq!(
        doc["profiles"]["other"]["notifications"]["quiet"].as_bool(),
        Some(true)
    );
    assert_eq!(doc["future_plugin"]["keep"].as_str(), Some("untouched"));
    assert_eq!(reload().send().await?.status(), StatusCode::OK);
    assert_eq!(
        get_config(&client, &addr).await["notifications"]["quiet"],
        "true"
    );
    handle.abort();
    Ok(())
}

#[tokio::test]
async fn native_notification_preparation_authenticates_current_events_and_reuses_live_gates()
-> Result<()> {
    let _env = lock_test_env();
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("config.toml");
    fs::write(
        &path,
        "[notifications]\nmethod = 'auto'\ncondition = 'unfocused'\nthreshold_secs = 0\nsound = 'off'\n",
    )?;
    let (addr, manager, handle) = spawn_test_server_with_root_token_mobile_workspace_and_overrides(
        temp.path().join("runtime"),
        temp.path().join("sessions"),
        Some("notification-fixture-token".into()),
        false,
        temp.path().to_path_buf(),
        TestServerOverrides {
            config_path: Some(path),
            ..Default::default()
        },
    )
    .await?
    .expect("loopback Runtime must be available");
    let client = crate::tls::reqwest_client();
    let token = "notification-fixture-token";
    let reload = || {
        client
            .post(format!("http://{addr}/v1/config/reload"))
            .bearer_auth(token)
    };
    assert_eq!(reload().send().await?.status(), StatusCode::OK);
    let mut thread = manager
        .create_thread(crate::runtime_threads::CreateThreadRequest::default())
        .await?;
    thread.latest_turn_id = Some("test-turn".into());
    manager.test_store().save_thread(&thread)?;
    let mut turn: crate::runtime_threads::TurnRecord = serde_json::from_value(json!({
        "id": "test-turn", "thread_id": thread.id, "status": "completed", "input_summary": "private user text",
        "created_at": Utc::now(), "duration_ms": 30000
    }))?;
    manager.test_store().save_turn(&turn)?;
    let event = manager
        .emit_event_for_test(
            &thread.id,
            Some(&turn.id),
            "turn.completed",
            json!({"answer": "private answer must not be copied"}),
        )
        .await?;
    let endpoint = format!(
        "http://{addr}/v1/threads/{}/notifications/prepare",
        thread.id
    );
    let request = |seq, focused, away| json!({"seq":seq,"focused":focused,"unfocused_for_ms":away,"locale":"en"});
    let post = |value| client.post(&endpoint).bearer_auth(token).json(&value);
    assert_eq!(
        client
            .post(&endpoint)
            .json(&request(event.seq, false, 2500))
            .send()
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        post(request(0, false, 2500)).send().await?.status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        post(request(event.seq + 100, false, 2500))
            .send()
            .await?
            .status(),
        StatusCode::BAD_REQUEST
    );
    let mut arbitrary = request(event.seq, false, 2500);
    arbitrary["body"] = json!("renderer injection");
    assert_eq!(
        post(arbitrary).send().await?.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    for (focused, away, expected) in [
        (true, 2500, "suppressed"),
        (false, 1999, "suppressed"),
        (false, 2500, "prepared"),
    ] {
        let result: Value = post(request(event.seq, focused, away))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_eq!(result["status"], expected);
        assert!(!result.to_string().contains("private"));
        assert_eq!(result["sound"], "off");
    }
    for (key, value, expected) in [
        ("quiet", "true", "suppressed"),
        ("quiet", "false", "prepared"),
        ("method", "osc9", "unsupported_method"),
        ("method", "auto", "prepared"),
        ("events.turn-complete", "false", "suppressed"),
        ("events.turn-complete", "true", "prepared"),
        ("condition", "never", "suppressed"),
        ("condition", "always", "prepared"),
    ] {
        client
            .post(format!("http://{addr}/v1/config"))
            .bearer_auth(token)
            .json(&json!({"key":format!("notifications.{key}"),"value":value,"persist":true}))
            .send()
            .await?
            .error_for_status()?;
        reload().send().await?.error_for_status()?;
        let result: Value = post(request(event.seq, false, 2500))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_eq!(result["status"], expected, "{key}={value}");
    }
    let other = manager
        .create_thread(crate::runtime_threads::CreateThreadRequest::default())
        .await?;
    let other_event = manager
        .emit_event_for_test(&other.id, None, "turn.completed", json!({}))
        .await?;
    assert_eq!(
        post(request(other_event.seq, false, 2500))
            .send()
            .await?
            .status(),
        StatusCode::BAD_REQUEST,
        "an existing sequence in another authenticated thread cannot be selected"
    );
    // Age a durable fixture record without a wall-clock wait or production
    // timeout changes. Restore its original bytes before later settlement cases.
    let event_path =
        RuntimeThreadManagerConfig::from_task_data_dir(temp.path().join("runtime/runtime"))
            .data_dir
            .join("events")
            .join(format!("{}.jsonl", thread.id));
    let original_events = fs::read_to_string(&event_path)?;
    let mut aged = String::new();
    for line in original_events.lines() {
        let mut record: Value = serde_json::from_str(line)?;
        if record["seq"] == event.seq {
            record["timestamp"] = json!(Utc::now() - chrono::Duration::seconds(120));
        }
        aged.push_str(&serde_json::to_string(&record)?);
        aged.push('\n');
    }
    fs::write(&event_path, aged)?;
    let expired: Value = post(request(event.seq, false, 2500))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(expired["status"], "expired");
    fs::write(&event_path, original_events)?;
    // Approval/input notifications are eligible only while the matching current
    // request is actionable, even if a stale pending projection survived.
    turn.status = crate::runtime_threads::RuntimeTurnStatus::InProgress;
    manager.test_store().save_turn(&turn)?;
    let (approval_id, _approval) =
        manager.register_pending_approval_for_thread_for_test(&thread.id, "approval-current");
    manager.register_pending_user_input_for_thread_for_test(&thread.id, "input-current");
    let approval = manager
        .emit_event_for_test(
            &thread.id,
            Some(&turn.id),
            "approval.required",
            json!({"id": approval_id, "description":"private approval prompt"}),
        )
        .await?;
    let input = manager
        .emit_event_for_test(
            &thread.id,
            Some(&turn.id),
            "user_input.required",
            json!({"id":"input-current","questions":["private question"]}),
        )
        .await?;
    for seq in [approval.seq, input.seq] {
        let value: Value = post(request(seq, false, 2500))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_eq!(value["status"], "prepared");
        assert!(!value.to_string().contains("private"));
    }
    turn.status = crate::runtime_threads::RuntimeTurnStatus::Completed;
    manager.test_store().save_turn(&turn)?;
    for seq in [approval.seq, input.seq] {
        let value: Value = post(request(seq, false, 2500))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_eq!(value["status"], "settled");
    }
    let recovered = manager
        .emit_event_for_test(
            &thread.id,
            Some(&turn.id),
            "turn.completed",
            json!({"recovered":true}),
        )
        .await?;
    let value: Value = post(request(recovered.seq, false, 2500))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(value["status"], "expired");
    thread.latest_turn_id = Some("next-turn".into());
    manager.test_store().save_thread(&thread)?;
    let value: Value = post(request(event.seq, false, 2500))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(value["status"], "settled");
    handle.abort();
    Ok(())
}

#[tokio::test]
async fn native_notification_replay_rechecks_requests_settled_during_the_read() -> Result<()> {
    let _env = lock_test_env();
    let temp = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("CODEWHALE_HOME", temp.path().join("home"));
    let _backend = EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    let _proxy = EnvVarGuard::set("NO_PROXY", "*");
    let path = temp.path().join("config.toml");
    fs::write(
        &path,
        "[notifications]\nmethod = 'auto'\ncondition = 'always'\nsound = 'off'\n",
    )?;
    let (addr, manager, handle) = spawn_test_server_with_root_token_mobile_workspace_and_overrides(
        temp.path().join("runtime"),
        temp.path().join("sessions"),
        Some("notification-race-fixture-token".into()),
        false,
        temp.path().to_path_buf(),
        TestServerOverrides {
            config_path: Some(path),
            ..Default::default()
        },
    )
    .await?
    .expect("loopback Runtime must be available");
    let client = crate::tls::reqwest_client();
    let token = "notification-race-fixture-token";
    client
        .post(format!("http://{addr}/v1/config/reload"))
        .bearer_auth(token)
        .send()
        .await?
        .error_for_status()?;

    let mut after_settlement = Vec::new();
    for event_name in ["approval.required", "user_input.required"] {
        let mut thread = manager
            .create_thread(CreateThreadRequest::default())
            .await?;
        thread.latest_turn_id = Some("test-turn".into());
        manager.test_store().save_thread(&thread)?;
        let turn: crate::runtime_threads::TurnRecord = serde_json::from_value(json!({
            "id": "test-turn", "thread_id": thread.id, "status": "in_progress",
            "input_summary": "silent settlement fixture", "created_at": Utc::now()
        }))?;
        manager.test_store().save_turn(&turn)?;
        let mut mock = crate::core::engine::mock_engine_handle();
        manager
            .install_test_engine(&thread.id, mock.handle.clone())
            .await?;
        let approval = if event_name == "approval.required" {
            Some(manager.register_pending_approval_for_thread_for_test(&thread.id, "request"))
        } else {
            manager.register_pending_user_input_for_thread_for_test(&thread.id, "request");
            None
        };
        // The event has to name the same identity the pending projection holds,
        // which for an approval is the runtime-minted one.
        let request_id = approval
            .as_ref()
            .map(|(approval_id, _)| approval_id.clone())
            .unwrap_or_else(|| "request".to_string());
        let event = manager
            .emit_event_for_test(
                &thread.id,
                Some(&turn.id),
                event_name,
                json!({"id": request_id}),
            )
            .await?;
        let endpoint = format!(
            "http://{addr}/v1/threads/{}/notifications/prepare",
            thread.id
        );
        let request =
            json!({"seq": event.seq, "focused": false, "unfocused_for_ms": 2500, "locale": "en"});
        let baseline: Value = client
            .post(&endpoint)
            .bearer_auth(token)
            .json(&request)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_eq!(baseline["status"], "prepared", "{event_name}");

        let (hook_tx, mut hook_rx) = mpsc::unbounded_channel();
        manager.set_replay_test_hook(hook_tx);
        let pending_request = tokio::spawn(async move {
            crate::tls::reqwest_client()
                .post(endpoint)
                .bearer_auth(token)
                .json(&request)
                .send()
                .await
        });
        let point = tokio::time::timeout(ci_scaled(Duration::from_secs(2)), hook_rx.recv())
            .await
            .context("notification preparation did not reach the held durable read")?
            .context("replay hook closed")?;
        assert_eq!(point.thread_id, thread.id);
        if let Some((approval_id, approval)) = approval {
            assert!(manager.deliver_external_approval(
                &approval_id,
                ExternalApprovalDecision::Allow { remember: false },
            ));
            assert_eq!(
                approval.await?,
                ExternalApprovalDecision::Allow { remember: false }
            );
        } else {
            let (canceled, consumed) = tokio::join!(
                manager.cancel_user_input(&thread.id, "request"),
                mock.recv_user_input_cancellation(),
            );
            assert!(canceled?);
            assert_eq!(consumed.as_deref(), Some("request"));
        }
        let settled = manager.get_thread_detail(&thread.id).await?;
        assert!(settled.pending_approvals.is_empty());
        assert!(settled.pending_user_inputs.is_empty());
        assert_eq!(
            settled.turns[0].status,
            crate::runtime_threads::RuntimeTurnStatus::InProgress,
            "settlement must happen within the same active turn"
        );
        point
            .resume
            .send(())
            .map_err(|_| anyhow::anyhow!("replay dropped its resume signal"))?;
        let result: Value =
            tokio::time::timeout(ci_scaled(Duration::from_secs(2)), pending_request)
                .await???
                .error_for_status()?
                .json()
                .await?;
        after_settlement.push((event_name, result));
        drop(mock);
    }
    handle.abort();
    assert!(
        after_settlement.iter().all(|(_, result)| {
            result["status"] != "prepared"
                && result.get("headline").is_none()
                && result.get("body").is_none()
                && result["sound"] == "off"
        }),
        "requests settled during replay must stay silent: {after_settlement:?}"
    );
    Ok(())
}

#[tokio::test]
async fn runtime_image_http_rejects_before_dispatch_and_accepts_large_canonical_bytes() -> Result<()>
{
    use crate::image_attach::tests::runtime_image_fixture;
    use base64::Engine as _;
    let _env = crate::test_support::lock_test_env();
    let dir = tempfile::tempdir()?;
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
    let mut config = Config {
        provider: Some("deepseek".into()),
        default_text_model: Some("deepseek-v4-flash-vision-exp".into()),
        ..Default::default()
    }
    .with_legacy_root(Some("synthetic-image-key".into()), None);
    config
        .set_provider_model_override(
            &config.test_identity_for_kind(ProviderKind::Deepseek),
            Some("deepseek-v4-flash-vision-exp".into()),
        )
        .unwrap();
    let (addr, manager, server) = spawn_test_server_with_root_token_mobile_workspace_and_overrides(
        dir.path().to_path_buf(),
        dir.path().join("sessions"),
        None,
        false,
        dir.path().join("workspace"),
        TestServerOverrides {
            config: Some(config),
            ..Default::default()
        },
    )
    .await?
    .context("loopback listener required for image contract proof")?;
    let thread = manager.create_thread(Default::default()).await?;
    let mut harness = crate::core::engine::mock_engine_handle();
    manager
        .install_test_engine(&thread.id, harness.handle.clone())
        .await?;
    let client = crate::tls::reqwest_client();
    let url = format!("http://{addr}/v1/threads/{}/turns", thread.id);
    let good = runtime_image_fixture(7);
    for body in [
        json!({"prompt":"look", "images":[{"mime":"image/png", "dataBase64":"garbage"}]}),
        json!({"prompt":"", "images":[good.clone()]}),
        json!({"prompt":"look", "model":"auto", "images":[good.clone()]}),
        json!({"prompt":"look", "model":"deepseek-v4-pro", "images":[good.clone()]}),
        json!({"prompt":"look", "images":[{"mime":"image/png", "dataBase64":good.data_base64, "path":"/private/host-only"}]}),
    ] {
        let response = client.post(&url).json(&body).send().await?;
        assert!(response.status().is_client_error());
        assert!(
            harness.rx_op.try_recv().is_err(),
            "rejection must not dispatch any Engine op"
        );
        assert!(
            manager
                .get_thread_detail(&thread.id)
                .await?
                .turns
                .is_empty()
        );
    }
    // An actual incompressible PNG crosses the former 2 MiB HTTP ceiling.
    let mut random = 0x4a11_3317_u32;
    let bytes: Vec<u8> = (0..768 * 768 * 4)
        .map(|_| {
            random ^= random << 13;
            random ^= random >> 17;
            random ^= random << 5;
            random as u8
        })
        .collect();
    let image =
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_raw(768, 768, bytes).unwrap());
    let mut encoded = std::io::Cursor::new(Vec::new());
    image.write_to(&mut encoded, image::ImageFormat::Png)?;
    let large = codewhale_protocol::runtime::RuntimeImageInput {
        mime: "image/png".into(),
        data_base64: base64::engine::general_purpose::STANDARD.encode(encoded.into_inner()),
    };
    let body = json!({"prompt":"compare screenshot", "operation_key":"http-image-once", "images":[large.clone(),good.clone()]});
    assert!(serde_json::to_vec(&body)?.len() > 2 * 1024 * 1024);
    let response = client.post(&url).json(&body).send().await?;
    assert_eq!(response.status(), StatusCode::CREATED);
    let accepted: Value = response.json().await?;
    let Op::SendMessage(TurnSpec { images, .. }) =
        harness.rx_op.recv().await.context("accepted Engine op")?
    else {
        bail!("expected SendMessage");
    };
    assert_eq!(images, [large, good]);
    let replay: Value = client
        .post(&url)
        .json(&body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(replay["turn"]["id"], accepted["turn"]["id"]);
    assert!(harness.rx_op.try_recv().is_err());
    harness
        .tx_event
        .send(EngineEvent::TurnStarted {
            turn_id: "image-http-fixture".into(),
            created_at: Utc::now(),
            route: None,
            submission_id: None,
        })
        .await?;
    harness
        .tx_event
        .send(EngineEvent::TurnComplete {
            usage: Default::default(),
            parent_route_usage: Default::default(),
            routed_usage_dropped_records: 0,
            status: crate::core::events::TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await?;
    server.abort();
    Ok(())
}

#[tokio::test]
async fn runtime_image_stream_rejection_does_not_leave_empty_threads() -> Result<()> {
    use crate::image_attach::tests::{runtime_image_fixture, runtime_image_fixture_bytes};
    let _env = crate::test_support::lock_test_env();
    let dir = tempfile::tempdir()?;
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
    let mut config = Config {
        provider: Some("deepseek".into()),
        default_text_model: Some("deepseek-v4-flash-vision-exp".into()),
        ..Default::default()
    }
    .with_legacy_root(Some("synthetic-image-key".into()), None);
    config
        .set_provider_model_override(
            &config.test_identity_for_kind(ProviderKind::Deepseek),
            Some("deepseek-v4-flash-vision-exp".into()),
        )
        .unwrap();
    let (addr, manager, server) = spawn_test_server_with_root_token_mobile_workspace_and_overrides(
        dir.path().to_path_buf(),
        dir.path().join("sessions"),
        None,
        false,
        dir.path().join("workspace"),
        TestServerOverrides {
            config: Some(config),
            ..Default::default()
        },
    )
    .await?
    .context("loopback listener required")?;
    let client = crate::tls::reqwest_client();
    let good = runtime_image_fixture(3);
    for body in [
        json!({"prompt":"look", "images":[{"mime":"image/png","dataBase64":"garbage"}]}),
        json!({"prompt":"", "images":[good.clone()]}),
        json!({"prompt":"look", "model":"auto", "images":[good.clone()]}),
        json!({"prompt":"look", "model":"deepseek-v4-pro", "images":[good]}),
        json!({"prompt":"look", "images":[runtime_image_fixture_bytes(4 * 1024 * 1024 + 1)]}),
    ] {
        let response = client
            .post(format!("http://{addr}/v1/stream"))
            .json(&body)
            .send()
            .await?;
        assert!(response.status().is_client_error(), "{}", response.status());
        assert!(
            manager
                .list_threads(
                    crate::runtime_threads::ThreadListFilter::IncludeArchived,
                    None
                )
                .await?
                .is_empty(),
            "failed admission left an empty thread"
        );
    }
    // Cleanup refuses a loaded or accepted session instead of erasing uncertain work.
    let retained = manager.create_thread(Default::default()).await?;
    let harness = crate::core::engine::mock_engine_handle();
    manager
        .install_test_engine(&retained.id, harness.handle.clone())
        .await?;
    assert!(manager.discard_empty_thread(&retained.id).await.is_err());
    assert!(manager.get_thread(&retained.id).await.is_ok());
    server.abort();
    Ok(())
}

#[tokio::test]
async fn runtime_image_named_catalog_resolves_and_echoes_exact_configured_identity() -> Result<()> {
    let _env = crate::test_support::lock_test_env();
    let dir = tempfile::tempdir()?;
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
    let route = crate::config::ProviderConfig {
        kind: Some("openai-compatible".into()),
        base_url: Some("http://127.0.0.1:9/v1".into()),
        model: Some("same-model".into()),
        ..Default::default()
    };
    let config = Config {
        provider: Some("vision-personal".into()),
        providers: Some(crate::config::ProvidersConfig {
            custom: std::collections::HashMap::from([
                ("vision-personal".into(), route.clone()),
                ("vision-work".into(), route),
            ]),
            ..Default::default()
        }),
        ..Default::default()
    };
    let (addr, _, server) = spawn_test_server_with_root_token_mobile_workspace_and_overrides(
        dir.path().to_path_buf(),
        dir.path().join("sessions"),
        None,
        false,
        dir.path().join("workspace"),
        TestServerOverrides {
            config: Some(config),
            ..Default::default()
        },
    )
    .await?
    .context("loopback listener required")?;
    let client = crate::tls::reqwest_client();
    for identity in ["vision-personal", "vision-work"] {
        let response: Value = client
            .get({
                let mut url =
                    reqwest::Url::parse(&format!("http://{addr}/v1/providers/custom/models"))?;
                url.query_pairs_mut()
                    .append_pair("model_provider_id", identity);
                url
            })
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_eq!(response["model_provider_id"], identity);
        assert_eq!(response["provider"], "custom");
        assert_eq!(response["models"][0]["id"], "same-model");
        assert_eq!(response["models"][0]["image_input"], "unknown");
    }
    for (kind, identity) in [
        ("custom", "missing"),
        ("deepseek", "vision-work"),
        ("custom", " vision-work"),
    ] {
        assert_eq!(
            client
                .get({
                    let mut url =
                        reqwest::Url::parse(&format!("http://{addr}/v1/providers/{kind}/models"))?;
                    url.query_pairs_mut()
                        .append_pair("model_provider_id", identity);
                    url
                })
                .send()
                .await?
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    let legacy = client
        .get(format!("http://{addr}/v1/providers/custom/models"))
        .send()
        .await?;
    assert_eq!(legacy.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        get_config(&client, &addr).await["provider"],
        "vision-personal"
    );
    server.abort();
    Ok(())
}

#[test]
fn runtime_image_named_catalog_cursor_binds_identity_endpoint_and_catalog() -> Result<()> {
    let models = ["one", "two"]
        .into_iter()
        .map(|id| ProviderModelEntry {
            reasoning_effort: codewhale_config::route::CapabilityState::Unknown,
            reasoning_effort_levels: Vec::new(),
            reasoning_effort_source: None,
            id: id.into(),
            output_token_limit: codewhale_config::route::CapabilityState::Unknown,
            image_input: codewhale_config::route::CapabilityState::Supported,
        })
        .collect::<Vec<_>>();
    let fingerprint = |id: &str, base: &str| {
        crate::hashing::sha256_hex(serde_json::to_vec(&("custom", id, base)).unwrap())
    };
    let params = ListProviderModelsParams {
        model_provider_id: Some("vision-work".into()),
        limit: Some(1),
        ..Default::default()
    };
    let route = fingerprint("vision-work", "http://127.0.0.1:9/v1");
    let first = paginate_provider_models("custom", models.clone(), &params, Some(route.clone()))
        .map_err(|e| anyhow::anyhow!(e.message))?;
    let mut next = params;
    next.cursor = first.next_cursor;
    assert!(paginate_provider_models("custom", models.clone(), &next, Some(route)).is_ok());
    for changed in [
        Some(fingerprint("vision-personal", "http://127.0.0.1:9/v1")),
        Some(fingerprint("vision-work", "http://127.0.0.1:10/v1")),
        None,
    ] {
        assert!(paginate_provider_models("custom", models.clone(), &next, changed).is_err());
    }
    Ok(())
}

#[tokio::test]
async fn runtime_image_saved_session_import_validates_before_creating_a_thread() -> Result<()> {
    use codewhale_models::{ContentBlock, ImageUrlContent};
    let _env = crate::test_support::lock_test_env();
    let dir = tempfile::tempdir()?;
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir)?;
    let (addr, manager, server) =
        spawn_test_server_with_root(dir.path().to_path_buf(), sessions_dir.clone())
            .await?
            .context("loopback listener required")?;
    let client = crate::tls::reqwest_client();
    let expected =
        vec![crate::image_attach::tests::runtime_image_fixture_bytes(3 * 1024 * 1024); 2];
    let historical = crate::image_attach::prepare_stored_images(&expected)?;
    let cases = [
        vec![ContentBlock::ImageUrl {
            image_url: ImageUrlContent {
                url: "https://host.invalid/never-fetch".into(),
            },
        }],
        vec![ContentBlock::ImageUrl {
            image_url: ImageUrlContent {
                url: "data:image/png;base64,iVBORw0KGgo=".into(),
            },
        }],
        historical,
    ];
    for (index, mut content) in cases.into_iter().enumerate() {
        content.insert(
            0,
            ContentBlock::Text {
                text: "saved image prompt".into(),
                cache_control: None,
            },
        );
        let id = format!("sess_image_import_{index}");
        let session = json!({
            "schema_version": 1,
            "metadata": { "id": id, "title": "Image import fixture", "created_at": "2026-09-08T00:00:00Z", "updated_at": "2026-09-08T00:00:00Z", "message_count": 1, "total_tokens": 0, "model": "deepseek-v4-pro", "model_provider": "deepseek", "workspace": dir.path(), "mode": "agent" },
            "messages": [{ "role": "user", "content": content }], "system_prompt": null,
        });
        fs::write(
            sessions_dir.join(format!("{id}.json")),
            serde_json::to_vec(&session)?,
        )?;
        let response = client
            .post(format!("http://{addr}/v1/sessions/{id}/resume-thread"))
            .json(&json!({}))
            .send()
            .await?;
        if index < 2 {
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert!(
                manager
                    .list_threads(
                        crate::runtime_threads::ThreadListFilter::IncludeArchived,
                        None
                    )
                    .await?
                    .is_empty()
            );
        } else {
            assert_eq!(response.status(), StatusCode::CREATED);
            let response: Value = response.json().await?;
            let detail = manager
                .get_thread_detail(
                    response["thread_id"]
                        .as_str()
                        .context("imported thread id")?,
                )
                .await?;
            let item = detail
                .items
                .iter()
                .find(|item| item.kind == crate::runtime_threads::TurnItemKind::UserMessage)
                .context("imported user item")?;
            let blocks: Vec<ContentBlock> = serde_json::from_value(
                item.metadata.as_ref().context("image metadata")?["runtime_image_content"].clone(),
            )?;
            assert_eq!(
                crate::image_attach::runtime_images_from_blocks(&blocks)?,
                expected
            );
            assert_eq!(detail.thread.schema_version, 3);
        }
    }
    server.abort();
    Ok(())
}

#[test]
fn output_cap_catalog_reports_exact_transport_support() {
    use codewhale_config::route::CapabilityState;
    let _env = crate::test_support::lock_test_env();
    let config = Config::default();
    assert_eq!(
        provider_model_output_token_limit_for_api(
            &config,
            &(config).test_identity_for_kind(ProviderKind::OpenaiCodex),
            "gpt-5.5"
        ),
        CapabilityState::Unsupported
    );
    assert_eq!(
        provider_model_output_token_limit_for_api(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Deepseek),
            "deepseek-v4-pro"
        ),
        CapabilityState::Supported
    );
    assert!(default_runtime_capabilities().turn_output_token_limit);
}

#[tokio::test]
async fn output_cap_compatibility_stream_rejects_before_thread_creation() -> Result<()> {
    let _env = lock_test_env();
    let temp = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("CODEWHALE_HOME", temp.path());
    let root = temp.path().join("server");
    let (addr, manager, server) = spawn_test_server_with_root_token_mobile_workspace_and_overrides(
        root.clone(),
        root.join("sessions"),
        None,
        false,
        root.join("workspace"),
        TestServerOverrides {
            config: Some(Config {
                provider: Some("openai-codex".into()),
                default_text_model: Some("gpt-5.5".into()),
                ..Config::default()
            }),
            ..Default::default()
        },
    )
    .await?
    .context("loopback fixture must bind")?;
    let client = crate::tls::reqwest_client();
    for input in [
        json!({"prompt":"review","model":"gpt-5.5","maxOutputTokens":1500}),
        json!({"prompt":"review","model":"auto","maxOutputTokens":1500}),
        json!({"prompt":"review","maxOutputTokens":0}),
        json!({"prompt":"review","maxOutputTokens":1.5}),
    ] {
        let response = client
            .post(format!("http://{addr}/v1/stream"))
            .json(&input)
            .send()
            .await?;
        assert!(
            response.status().is_client_error(),
            "{:?}",
            response.status()
        );
    }
    assert!(
        manager
            .list_threads(ThreadListFilter::IncludeArchived, None)
            .await?
            .is_empty()
    );
    server.abort();
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn workspace_file_put_keeps_the_edited_files_mode() -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = tempfile::tempdir()?;
    let workspace = tmp.path().join("workspace");
    fs::create_dir_all(&workspace)?;
    let (addr, _, handle) = spawn_test_server_with_root_token_mobile_workspace(
        tmp.path().join("runtime"),
        tmp.path().join("sessions"),
        Some("files-token".to_string()),
        false,
        workspace.clone(),
    )
    .await?
    .context("files test requires a loopback listener")?;
    let client = crate::tls::reqwest_client();
    let base = format!("http://{addr}");

    // An executable script keeps its bits (set-user-id is never carried
    // over); an owner-only file stays owner-only.
    for (name, before, after) in [("deploy.sh", 0o4755, 0o755), ("secret.env", 0o600, 0o600)] {
        let path = workspace.join(name);
        fs::write(&path, "old\n")?;
        fs::set_permissions(&path, fs::Permissions::from_mode(before))?;
        let read: Value = client
            .get(format!("{base}/v1/workspace/files/read?path={name}"))
            .bearer_auth("files-token")
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        client
            .put(format!("{base}/v1/workspace/files"))
            .bearer_auth("files-token")
            .json(&json!({
                "path": name,
                "content": "new\n",
                "expected_revision": read["revision"],
            }))
            .send()
            .await?
            .error_for_status()?;
        assert_eq!(fs::read_to_string(&path)?, "new\n");
        let mode = fs::symlink_metadata(&path)?.permissions().mode() & 0o7777;
        assert_eq!(mode, after, "{name}: {mode:o}");
    }

    handle.abort();
    Ok(())
}

#[cfg(any(unix, windows))]
#[test]
fn workspace_file_concurrent_creates_preserve_the_first_publication() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let relative = Path::new("notes/new.txt");
    fs::write(tmp.path().join("unrelated.txt"), b"keep me\n")?;

    // Admit both creators while the destination is absent, before either
    // publishes. This makes the stale-create race independent of scheduling.
    let files = [
        crate::fleet::files::WorkspaceFile::open_shared(tmp.path(), relative, true)?,
        crate::fleet::files::WorkspaceFile::open_shared(tmp.path(), relative, true)?,
    ];
    assert!(!tmp.path().join(relative).exists());
    let payloads = [
        b"first creator\n".as_slice(),
        b"second creator\n".as_slice(),
    ];
    let barrier = std::sync::Barrier::new(2);
    let results = std::thread::scope(|scope| {
        let handles: Vec<_> = files
            .into_iter()
            .zip(payloads)
            .map(|(file, bytes)| {
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    workspace::publish_workspace_file(&file, bytes, true)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("creator worker must not panic"))
            .collect::<Vec<_>>()
    });

    let winners: Vec<_> = results
        .iter()
        .enumerate()
        .filter_map(|(index, result)| result.is_ok().then_some(index))
        .collect();
    assert_eq!(winners.len(), 1, "exactly one create may publish");
    let error = results
        .iter()
        .find_map(|result| result.as_ref().err())
        .unwrap();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("read it before overwriting"));
    assert_eq!(fs::read(tmp.path().join(relative))?, payloads[winners[0]]);
    assert_eq!(fs::read(tmp.path().join("unrelated.txt"))?, b"keep me\n");
    Ok(())
}

#[tokio::test]
async fn workspace_files_list_read_write_bounds_and_confinement() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let workspace = tmp.path().join("workspace");
    fs::create_dir_all(workspace.join("src"))?;
    fs::create_dir_all(workspace.join("bin"))?;
    fs::create_dir_all(workspace.join("empty"))?;
    fs::create_dir_all(workspace.join(".git/hooks"))?;
    fs::write(workspace.join("src/main.rs"), "fn main() {}\n")?;
    fs::write(workspace.join("README.md"), "# readme\n")?;
    fs::write(workspace.join("bin/blob"), [0u8, 1, 2, 255])?;
    fs::write(workspace.join(".git/config"), "[core]\n")?;
    let outside = tmp.path().join("outside");
    fs::create_dir_all(&outside)?;
    fs::write(outside.join("secret.txt"), "never served")?;
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&outside, workspace.join("link-dir"))?;
        std::os::unix::fs::symlink(outside.join("secret.txt"), workspace.join("link-file"))?;
    }
    let (addr, _, handle) = spawn_test_server_with_root_token_mobile_workspace(
        tmp.path().join("runtime"),
        tmp.path().join("sessions"),
        Some("files-token".to_string()),
        false,
        workspace.clone(),
    )
    .await?
    .context("files test requires a loopback listener")?;
    let client = crate::tls::reqwest_client();
    let base = format!("http://{addr}");
    let url = |route: &str, pairs: &[(&str, &str)]| {
        let mut url = reqwest::Url::parse(&format!("{base}{route}")).unwrap();
        url.query_pairs_mut().extend_pairs(pairs.iter().copied());
        url
    };

    // Authentication is required on every route.
    for route in [
        "/v1/workspace/files",
        "/v1/workspace/files/read?path=README.md",
    ] {
        let status = client.get(format!("{base}{route}")).send().await?.status();
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{route}");
    }

    // Listing: directories first, `.git` hidden, links named but never followed.
    let root_listing: Value = client
        .get(url("/v1/workspace/files", &[]))
        .bearer_auth("files-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let names: Vec<(String, String)> = root_listing["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            (
                entry["name"].as_str().unwrap().to_string(),
                entry["kind"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert!(!names.iter().any(|(name, _)| name == ".git"));
    assert!(names.contains(&("bin".to_string(), "directory".to_string())));
    assert!(names.contains(&("src".to_string(), "directory".to_string())));
    assert!(names.contains(&("README.md".to_string(), "file".to_string())));
    #[cfg(unix)]
    {
        assert!(names.contains(&("link-dir".to_string(), "symlink".to_string())));
        assert!(names.contains(&("link-file".to_string(), "symlink".to_string())));
    }
    let last_directory = names
        .iter()
        .rposition(|(_, kind)| kind == "directory")
        .unwrap();
    assert!(
        names[..=last_directory]
            .iter()
            .all(|(_, kind)| kind == "directory"),
        "directories are listed first: {names:?}"
    );
    let readme = root_listing["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "README.md")
        .unwrap();
    assert_eq!(readme["size"], 9);
    assert_eq!(readme["path"], "README.md");
    assert!(readme["modified"].is_string());
    assert_eq!(root_listing["truncated"], false);

    let src_listing: Value = client
        .get(url("/v1/workspace/files", &[("path", "src/")]))
        .bearer_auth("files-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(src_listing["path"], "src");
    assert_eq!(src_listing["entries"][0]["path"], "src/main.rs");
    let limited: Value = client
        .get(url("/v1/workspace/files", &[("limit", "1")]))
        .bearer_auth("files-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(limited["entries"].as_array().unwrap().len(), 1);
    assert_eq!(limited["truncated"], true);
    let mut rejected_listings = vec![
        ("../", StatusCode::BAD_REQUEST),
        ("/etc", StatusCode::BAD_REQUEST),
        ("src/../src", StatusCode::BAD_REQUEST),
        ("src\\main", StatusCode::BAD_REQUEST),
        (".git", StatusCode::FORBIDDEN),
        ("README.md", StatusCode::BAD_REQUEST),
        ("missing", StatusCode::NOT_FOUND),
    ];
    if cfg!(unix) {
        rejected_listings.push(("link-dir", StatusCode::FORBIDDEN));
    }
    for (path, expected) in rejected_listings {
        let status = client
            .get(url("/v1/workspace/files", &[("path", path)]))
            .bearer_auth("files-token")
            .send()
            .await?
            .status();
        assert_eq!(status, expected, "list path={path:?}");
    }
    let status = client
        .get(url("/v1/workspace/files", &[("limit", "0")]))
        .bearer_auth("files-token")
        .send()
        .await?
        .status();
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Reads: whole-file revision, byte windows, binary fallback, confinement.
    let read: Value = client
        .get(url("/v1/workspace/files/read", &[("path", "src/main.rs")]))
        .bearer_auth("files-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let expected_revision = {
        let digest = sha2::Sha256::digest(b"fn main() {}\n");
        digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };
    assert_eq!(read["path"], "src/main.rs");
    assert_eq!(read["encoding"], "utf-8");
    assert_eq!(read["content"], "fn main() {}\n");
    assert_eq!(read["size"], 13);
    assert_eq!(read["bytes"], 13);
    assert_eq!(read["truncated"], false);
    assert_eq!(read["revision"], expected_revision);
    let window: Value = client
        .get(url(
            "/v1/workspace/files/read",
            &[("path", "src/main.rs"), ("offset", "3"), ("limit", "4")],
        ))
        .bearer_auth("files-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(window["content"], "main");
    assert_eq!(window["offset"], 3);
    assert_eq!(window["bytes"], 4);
    assert_eq!(window["truncated"], true);
    assert_eq!(
        window["revision"], expected_revision,
        "revision covers the whole file"
    );
    let binary: Value = client
        .get(url("/v1/workspace/files/read", &[("path", "bin/blob")]))
        .bearer_auth("files-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(binary["encoding"], "base64");
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(binary["content"].as_str().unwrap())
            .unwrap(),
        vec![0u8, 1, 2, 255]
    );
    let mut rejected_reads = vec![
        ("", StatusCode::BAD_REQUEST),
        ("../outside/secret.txt", StatusCode::BAD_REQUEST),
        (".git/config", StatusCode::FORBIDDEN),
        ("src", StatusCode::BAD_REQUEST),
        ("missing.txt", StatusCode::NOT_FOUND),
        ("src/missing.rs", StatusCode::NOT_FOUND),
    ];
    if cfg!(unix) {
        rejected_reads.push(("link-file", StatusCode::FORBIDDEN));
        rejected_reads.push(("link-dir/secret.txt", StatusCode::FORBIDDEN));
    }
    for (path, expected) in rejected_reads {
        let status = client
            .get(url("/v1/workspace/files/read", &[("path", path)]))
            .bearer_auth("files-token")
            .send()
            .await?
            .status();
        assert_eq!(status, expected, "read path={path:?}");
    }
    let status = client
        .get(url(
            "/v1/workspace/files/read",
            &[("path", "README.md"), ("limit", "0")],
        ))
        .bearer_auth("files-token")
        .send()
        .await?
        .status();
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Writes: create, then optimistic concurrency on the whole-file revision.
    let put = |body: Value| {
        client
            .put(format!("{base}/v1/workspace/files"))
            .bearer_auth("files-token")
            .json(&body)
    };
    let status = client
        .put(format!("{base}/v1/workspace/files"))
        .json(&json!({"path": "notes/todo.md", "content": "x"}))
        .send()
        .await?
        .status();
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let created = put(json!({"path": "notes/todo.md", "content": "- first\n"}))
        .send()
        .await?;
    assert_eq!(created.status(), StatusCode::CREATED);
    let created: Value = created.json().await?;
    assert_eq!(created["created"], true);
    assert_eq!(created["path"], "notes/todo.md");
    assert_eq!(created["size"], 8);
    assert_eq!(
        fs::read_to_string(workspace.join("notes/todo.md"))?,
        "- first\n"
    );
    let first_revision = created["revision"].as_str().unwrap().to_string();
    let read_back: Value = client
        .get(url(
            "/v1/workspace/files/read",
            &[("path", "notes/todo.md")],
        ))
        .bearer_auth("files-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(read_back["revision"], first_revision);

    // Overwrites need the revision that was read; a stale one is refused.
    let status = put(json!({"path": "notes/todo.md", "content": "- second\n"}))
        .send()
        .await?
        .status();
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "overwrite without expected_revision"
    );
    let stale = put(json!({
        "path": "notes/todo.md",
        "content": "- second\n",
        "expected_revision": "0".repeat(64),
    }))
    .send()
    .await?;
    assert_eq!(stale.status(), StatusCode::CONFLICT);
    let stale: Value = stale.json().await?;
    assert!(
        stale["error"]["message"]
            .as_str()
            .unwrap()
            .contains(&first_revision),
        "conflict names the current revision"
    );
    assert_eq!(
        fs::read_to_string(workspace.join("notes/todo.md"))?,
        "- first\n"
    );
    let updated = put(json!({
        "path": "notes/todo.md",
        "content": "- second\n",
        "expected_revision": first_revision,
    }))
    .send()
    .await?;
    assert_eq!(updated.status(), StatusCode::OK);
    let updated: Value = updated.json().await?;
    assert_eq!(updated["created"], false);
    assert_ne!(updated["revision"], first_revision);
    assert_eq!(
        fs::read_to_string(workspace.join("notes/todo.md"))?,
        "- second\n"
    );
    let status = put(json!({
        "path": "notes/new.md",
        "content": "x",
        "expected_revision": first_revision,
    }))
    .send()
    .await?
    .status();
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "expected_revision on a new file"
    );
    assert!(!workspace.join("notes/new.md").exists());

    // Binary payloads, bad encodings, oversized bodies and confinement.
    let encoded = put(json!({
        "path": "bin/written.bin",
        "content": base64::engine::general_purpose::STANDARD.encode([7u8, 0, 9]),
        "encoding": "base64",
    }))
    .send()
    .await?;
    assert_eq!(encoded.status(), StatusCode::CREATED);
    assert_eq!(
        fs::read(workspace.join("bin/written.bin"))?,
        vec![7u8, 0, 9]
    );
    let status = put(json!({"path": "bin/x", "content": "x", "encoding": "hex"}))
        .send()
        .await?
        .status();
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let status = put(json!({"path": "bin/x", "content": "***", "encoding": "base64"}))
        .send()
        .await?
        .status();
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let status = put(json!({"path": "big.txt", "content": "a".repeat(4 * 1024 * 1024 + 1)}))
        .send()
        .await?
        .status();
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    let mut rejected_writes = vec![
        ("../outside/x.txt", StatusCode::BAD_REQUEST),
        ("/tmp/x.txt", StatusCode::BAD_REQUEST),
        (".git/hooks/pre-commit", StatusCode::FORBIDDEN),
        ("src", StatusCode::BAD_REQUEST),
    ];
    if cfg!(unix) {
        rejected_writes.push(("link-dir/x.txt", StatusCode::FORBIDDEN));
        rejected_writes.push(("link-file", StatusCode::FORBIDDEN));
    }
    for (path, expected) in rejected_writes {
        let status = put(json!({"path": path, "content": "x"}))
            .send()
            .await?
            .status();
        assert_eq!(status, expected, "write path={path:?}");
    }
    assert!(!outside.join("x.txt").exists());
    assert_eq!(
        fs::read_to_string(outside.join("secret.txt"))?,
        "never served"
    );
    assert!(!workspace.join(".git/hooks/pre-commit").exists());

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn session_artifacts_list_and_bounded_read() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let sessions_dir = tmp.path().join("sessions");
    let workspace = tmp.path().join("workspace");
    fs::create_dir_all(&workspace)?;
    let manager = crate::session_manager::SessionManager::new(sessions_dir.clone())?;
    let session_id = "sess_artifacts_route".to_string();
    let mut session = crate::session_manager::create_saved_session_with_id_and_mode(
        session_id.clone(),
        &[],
        "deepseek-v4-pro",
        &workspace,
        0,
        None,
        Some("plan"),
    );
    let artifact_dir = sessions_dir.join(&session_id).join("artifacts");
    fs::create_dir_all(&artifact_dir)?;
    let body = "line one\nline two\n".repeat(64);
    fs::write(artifact_dir.join("art_call_1.txt"), &body)?;
    let served = crate::artifacts::record_tool_output_artifact_with_size(
        &session_id,
        "call_1",
        "bash",
        PathBuf::from("artifacts/art_call_1.txt"),
        body.len() as u64,
        "line one",
    );
    let escaping = crate::artifacts::record_tool_output_artifact_with_size(
        &session_id,
        "call_2",
        "bash",
        PathBuf::from("../../escape.txt"),
        4,
        "nope",
    );
    fs::write(tmp.path().join("escape.txt"), "nope")?;
    let missing = crate::artifacts::record_tool_output_artifact_with_size(
        &session_id,
        "call_3",
        "bash",
        PathBuf::from("artifacts/gone.txt"),
        4,
        "gone",
    );
    session.artifacts = vec![served.clone(), escaping.clone(), missing.clone()];
    manager.save_session(&session)?;

    let (addr, _, handle) = spawn_test_server_with_root_token_mobile_workspace(
        tmp.path().join("runtime"),
        sessions_dir.clone(),
        Some("artifacts-token".to_string()),
        false,
        workspace,
    )
    .await?
    .context("artifacts test requires a loopback listener")?;
    let client = crate::tls::reqwest_client();
    let base = format!("http://{addr}");

    let status = client
        .get(format!("{base}/v1/sessions/{session_id}/artifacts"))
        .send()
        .await?
        .status();
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let listing: Value = client
        .get(format!("{base}/v1/sessions/{session_id}/artifacts"))
        .bearer_auth("artifacts-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(listing["session_id"], session_id);
    let artifacts = listing["artifacts"].as_array().unwrap();
    assert_eq!(artifacts.len(), 3);
    assert_eq!(artifacts[0]["id"], served.id);
    assert_eq!(artifacts[0]["kind"], "tool_output");
    assert_eq!(artifacts[0]["tool_name"], "bash");
    assert_eq!(artifacts[0]["tool_call_id"], "call_1");
    assert_eq!(artifacts[0]["byte_size"], body.len() as u64);
    assert_eq!(artifacts[0]["path"], "artifacts/art_call_1.txt");
    assert_eq!(artifacts[0]["preview"], "line one");
    assert!(artifacts[0].get("storage_path").is_none());

    let read: Value = client
        .get(format!(
            "{base}/v1/sessions/{session_id}/artifacts/{}",
            served.id
        ))
        .bearer_auth("artifacts-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(read["artifact"]["id"], served.id);
    assert_eq!(read["encoding"], "utf-8");
    assert_eq!(read["content"], body);
    assert_eq!(read["size"], body.len() as u64);
    assert_eq!(read["truncated"], false);
    let window: Value = client
        .get(format!(
            "{base}/v1/sessions/{session_id}/artifacts/{}?offset=5&limit=3",
            served.id
        ))
        .bearer_auth("artifacts-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(window["content"], "one");
    assert_eq!(window["truncated"], true);
    assert_eq!(window["revision"], read["revision"]);

    for (artifact_id, expected) in [
        (escaping.id.as_str(), StatusCode::FORBIDDEN),
        (missing.id.as_str(), StatusCode::NOT_FOUND),
        ("art_unknown", StatusCode::NOT_FOUND),
    ] {
        let status = client
            .get(format!(
                "{base}/v1/sessions/{session_id}/artifacts/{artifact_id}"
            ))
            .bearer_auth("artifacts-token")
            .send()
            .await?
            .status();
        assert_eq!(status, expected, "artifact={artifact_id}");
    }
    let status = client
        .get(format!(
            "{base}/v1/sessions/{session_id}/artifacts/{}?limit=0",
            served.id
        ))
        .bearer_auth("artifacts-token")
        .send()
        .await?
        .status();
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let status = client
        .get(format!("{base}/v1/sessions/sess_missing/artifacts"))
        .bearer_auth("artifacts-token")
        .send()
        .await?
        .status();
    assert_eq!(status, StatusCode::NOT_FOUND);

    handle.abort();
    Ok(())
}

/// Restores the process-wide test artifact root on drop.
struct TestArtifactRoot {
    previous: Option<PathBuf>,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl Drop for TestArtifactRoot {
    fn drop(&mut self) {
        crate::artifacts::set_test_artifact_sessions_root(self.previous.take());
    }
}

fn test_artifact_root(root: &Path) -> TestArtifactRoot {
    let guard = crate::artifacts::TEST_ARTIFACT_SESSIONS_GUARD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    TestArtifactRoot {
        previous: crate::artifacts::set_test_artifact_sessions_root(Some(root.to_path_buf())),
        _guard: guard,
    }
}

/// The documented file ref id: `file_` plus 32 hex of SHA-256(path).
fn turn_file_artifact_id(path: &str) -> String {
    format!(
        "file_{}",
        &crate::hashing::sha256_hex(path.as_bytes())[..32]
    )
}

fn file_ref_json(path: &str, change: &str, revision: Option<&str>, size: Option<u64>) -> Value {
    json!({
        "id": turn_file_artifact_id(path),
        "kind": "file",
        "path": path,
        "change": change,
        "size": size,
        "revision": revision,
        "source": "workspace_changed_during_turn",
        "recorded_at": Utc::now(),
    })
}

/// The turn artifact routes list a turn's refs only through its own thread,
/// and read each ref from wherever its recorded revision still lives: the
/// workspace, the post-turn snapshot, or the session artifact directory —
/// including a spill from an engine session that has no SavedSession index.
#[tokio::test]
async fn turn_artifact_routes_list_and_read_by_reference() -> Result<()> {
    let _env = lock_test_env();
    let tmp = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("CODEWHALE_HOME", tmp.path().join("home"));
    let artifact_root = tmp.path().join("artifact-sessions");
    let _artifacts = test_artifact_root(&artifact_root);
    let workspace = tmp.path().join("workspace");
    fs::create_dir_all(&workspace)?;
    fs::write(workspace.join("README.md"), "fixture\n")?;
    let rev = |bytes: &[u8]| crate::hashing::sha256_hex(bytes);

    // The turn: page.html written as v1 (then edited after the turn),
    // current.md written and untouched since.
    // Turn records name snapshots by tree id: a prune rewrites commit ids.
    let pre = crate::core::turn::pre_turn_snapshot(&workspace, 1, 0, None, None)
        .context("pre-turn snapshot")?
        .tree
        .as_str()
        .to_string();
    fs::write(workspace.join("page.html"), "<p>v1</p>\n")?;
    fs::write(workspace.join("current.md"), "current\n")?;
    let post = crate::core::turn::post_turn_snapshot(&workspace, 1, 0, None, None)
        .context("post-turn snapshot")?
        .tree
        .as_str()
        .to_string();
    fs::write(workspace.join("page.html"), "<p>v2</p>\n")?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(workspace.join("README.md"), workspace.join("link.md"))?;

    // A spill from an unbound runtime engine: bytes under the writer's
    // artifact root, no SavedSession JSON anywhere.
    let spill_dir = artifact_root.join("engine-random-session/artifacts");
    fs::create_dir_all(&spill_dir)?;
    fs::write(spill_dir.join("art_call_big.txt"), "big output\n")?;
    // Bytes that no longer match what the turn recorded.
    fs::write(spill_dir.join("art_call_forged.txt"), "rewritten\n")?;

    let (addr, runtime_threads, handle) = spawn_test_server_with_root_token_mobile_workspace(
        tmp.path().join("runtime"),
        tmp.path().join("sessions"),
        Some("turn-artifacts-token".to_string()),
        false,
        workspace.clone(),
    )
    .await?
    .context("turn artifact test requires a loopback listener")?;
    let thread = runtime_threads
        .create_thread(CreateThreadRequest {
            workspace: Some(workspace.clone()),
            ..Default::default()
        })
        .await?;
    let other = runtime_threads
        .create_thread(CreateThreadRequest {
            workspace: Some(workspace.clone()),
            ..Default::default()
        })
        .await?;

    let spill = |id: &str, revision: &str| {
        json!({
            "id": id, "kind": "tool_output", "path": format!("artifacts/{id}.txt"),
            "size": 11, "revision": revision, "session_id": "engine-random-session",
            "source": "tool_output_spill", "recorded_at": Utc::now(),
        })
    };
    let mut refs = vec![
        file_ref_json("page.html", "created", Some(&rev(b"<p>v1</p>\n")), Some(10)),
        file_ref_json("current.md", "created", Some(&rev(b"current\n")), Some(8)),
        file_ref_json("gone.md", "deleted", None, None),
        file_ref_json(
            "huge.bin",
            "created",
            Some(&rev(b"x")),
            Some(17 * 1024 * 1024),
        ),
        file_ref_json("lost.md", "updated", Some(&rev(b"never stored")), Some(12)),
        spill("art_call_big", &rev(b"big output\n")),
        spill("art_call_forged", &rev(b"other")),
        spill("art_call_pruned", &rev(b"x")),
    ];
    if cfg!(unix) {
        refs.push(file_ref_json(
            "link.md",
            "updated",
            Some(&rev(b"fixture\n")),
            Some(8),
        ));
    }
    // An intermediate revision one item recorded before a later write.
    let mut early_page =
        file_ref_json("page.html", "created", Some(&rev(b"<p>v0</p>\n")), Some(10));
    early_page["source"] = json!("tool_mutation");
    let item: crate::runtime_threads::TurnItemRecord = serde_json::from_value(json!({
        "id": "item_page", "turn_id": "turn_artifacts_route", "kind": "tool_call",
        "status": "completed", "summary": "write", "artifacts": [early_page],
    }))?;
    let store = runtime_threads.test_store();
    store.save_item(&item)?;
    let turn: TurnRecord = serde_json::from_value(json!({
        "id": "turn_artifacts_route", "thread_id": thread.id, "status": "completed",
        "input_summary": "build", "created_at": Utc::now(), "item_ids": ["item_page"],
        "artifacts": refs,
        "workspace": {
            "state": "settled", "pre_turn_snapshot_id": pre, "post_turn_snapshot_id": post,
        },
    }))?;
    store.save_turn(&turn)?;

    let client = crate::tls::reqwest_client();
    let base = format!(
        "http://{addr}/v1/threads/{}/turns/turn_artifacts_route/artifacts",
        thread.id
    );
    let get = |url: String| client.get(url).bearer_auth("turn-artifacts-token").send();
    assert_eq!(
        client.get(&base).send().await?.status(),
        StatusCode::UNAUTHORIZED
    );
    let listing: Value = get(base.clone()).await?.error_for_status()?.json().await?;
    assert_eq!(listing["turn_id"], "turn_artifacts_route");
    assert_eq!(listing["workspace"]["state"], "settled");
    assert_eq!(listing["artifacts"][0]["path"], "page.html");
    assert!(listing.get("item_artifacts").is_none());
    let foreign = format!(
        "http://{addr}/v1/threads/{}/turns/turn_artifacts_route/artifacts",
        other.id
    );
    assert_eq!(get(foreign).await?.status(), StatusCode::NOT_FOUND);
    let missing = format!(
        "http://{addr}/v1/threads/{}/turns/turn_nope/artifacts",
        thread.id
    );
    assert_eq!(get(missing).await?.status(), StatusCode::NOT_FOUND);

    let file_id = turn_file_artifact_id;
    let read = |id: String, query: &str| get(format!("{base}/{id}{query}"));

    // Still current in the workspace.
    let body: Value = read(file_id("current.md"), "")
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(body["source"], "workspace");
    assert_eq!(body["current"], true);
    assert_eq!(body["content"], "current\n");
    assert_eq!(body["revision"], rev(b"current\n"));

    // Edited after the turn: served from the post-turn snapshot.
    let body: Value = read(file_id("page.html"), "?offset=3&limit=2")
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(body["source"], "snapshot");
    assert_eq!(body["current"], false);
    assert_eq!(body["size"], 10);
    assert_eq!(body["content"], "v1");
    assert_eq!(body["truncated"], true);

    // An item's intermediate revision is selectable, and is honestly gone.
    let response = read(
        file_id("page.html"),
        &format!("?revision={}", rev(b"<p>v0</p>\n")),
    )
    .await?;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let message = response.text().await?;
    assert!(message.contains(&rev(b"<p>v2</p>\n")), "{message}");
    let response = read(file_id("page.html"), &format!("?revision={}", rev(b"nope"))).await?;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let mut statuses = vec![
        (file_id("lost.md"), StatusCode::CONFLICT),
        (file_id("gone.md"), StatusCode::GONE),
        (file_id("huge.bin"), StatusCode::PAYLOAD_TOO_LARGE),
        ("art_call_forged".to_string(), StatusCode::CONFLICT),
        ("art_call_pruned".to_string(), StatusCode::GONE),
        ("file_unknown".to_string(), StatusCode::NOT_FOUND),
    ];
    if cfg!(unix) {
        statuses.push((file_id("link.md"), StatusCode::FORBIDDEN));
    }
    for (id, expected) in statuses {
        assert_eq!(read(id.clone(), "").await?.status(), expected, "{id}");
    }

    // The spill is readable through the turn although no SavedSession
    // indexes its engine session.
    let body: Value = read("art_call_big".to_string(), "")
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(body["source"], "session_artifact");
    assert_eq!(body["content"], "big output\n");
    assert_eq!(body["artifact"]["kind"], "tool_output");
    assert!(body["current"].is_null());

    handle.abort();
    Ok(())
}

/// One tool call's changes, read from the workspace restore points the engine
/// recorded around it: a shell command's own writes belong to that command,
/// every path and revision comes from the span's own two trees rather than
/// from the work tree as it is now, and a call the engine never bounded — or
/// whose closing snapshot was lost — says so instead of handing back an empty
/// list that would read as "this call changed nothing".
#[tokio::test]
async fn call_change_route_reads_one_calls_workspace_span() -> Result<()> {
    if git_missing() {
        return Ok(());
    }
    let _env = lock_test_env();
    let tmp = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("CODEWHALE_HOME", tmp.path().join("home"));
    let workspace = tmp.path().join("workspace");
    fs::create_dir_all(&workspace)?;
    fs::write(workspace.join("kept.md"), "kept\n")?;
    let rev = |bytes: &[u8]| crate::hashing::sha256_hex(bytes);
    // The model endpoint's own id for the call, verbatim: the shape a gateway
    // really hands back, `|` and all. A record-id charset check would refuse it
    // and answer "no such call" for a call the turn plainly recorded, so this
    // literal is the regression this test exists for.
    const SHELL_CALL: &str =
        "call_01_f3d82rL5aT1NDbpsh4w63727|f8912d4c-2f79-46eb-91d4-9ed4998156f9";
    // A call whose receipts survived while the trees they name did not: the
    // side repo keeps only its newest snapshots, so an older turn's span is
    // regularly unreachable.
    const PRUNED_CALL: &str = "call_00_prunedForTest";
    // A call on a workspace whose snapshot store does not exist at all: the
    // receipts survived, the whole store did not.
    const STORELESS_CALL: &str = "call_00_storelessForTest";

    let (addr, runtime_threads, handle) = spawn_test_server_with_root_token_mobile_workspace(
        tmp.path().join("runtime"),
        tmp.path().join("sessions"),
        Some("call-changes-token".to_string()),
        false,
        workspace.clone(),
    )
    .await?
    .context("call change test requires a loopback listener")?;
    let thread = runtime_threads
        .create_thread(CreateThreadRequest {
            workspace: Some(workspace.clone()),
            ..Default::default()
        })
        .await?;

    // The call's own span: its `tool:` receipt before the command ran and its
    // `post-tool` partner after. `out.md` is the command's own write;
    // `script.py` is a path it modified. Afterwards the work tree moves on
    // again — the route must report the span, not what is on disk now.
    let repo = crate::snapshot::SnapshotRepo::open_or_init(&workspace)?;
    fs::write(workspace.join("script.py"), "print('v1')\n")?;
    let tool = repo.take_snapshot(&format!("tool:{SHELL_CALL}"), Some(&thread.id))?;
    fs::write(workspace.join("out.md"), "written by the command\n")?;
    fs::write(workspace.join("script.py"), "print('v2')\n")?;
    let post_tool = repo.take_snapshot(&format!("post-tool:{SHELL_CALL}"), Some(&thread.id))?;
    fs::write(workspace.join("out.md"), "edited long after the call\n")?;
    fs::write(workspace.join("after.md"), "after the span\n")?;

    // A second thread on its own workspace, which never had a snapshot store.
    let storeless_workspace = tmp.path().join("storeless-workspace");
    fs::create_dir_all(&storeless_workspace)?;
    let storeless_thread = runtime_threads
        .create_thread(CreateThreadRequest {
            workspace: Some(storeless_workspace),
            ..Default::default()
        })
        .await?;

    let store = runtime_threads.test_store();
    let shell_item: crate::runtime_threads::TurnItemRecord = serde_json::from_value(json!({
        "id": "item_shell", "turn_id": "turn_call_changes", "kind": "command_execution",
        "status": "completed", "summary": "exec_shell started",
        "metadata": { "tool_use_id": SHELL_CALL, "tool_name": "exec_shell" },
    }))?;
    store.save_item(&shell_item)?;
    // A call the engine judged read-only: the turn has its item, and no
    // restore point was ever taken for it.
    let read_item: crate::runtime_threads::TurnItemRecord = serde_json::from_value(json!({
        "id": "item_read", "turn_id": "turn_call_changes", "kind": "command_execution",
        "status": "completed", "summary": "exec_shell started",
        "metadata": { "tool_use_id": "call_read", "tool_name": "exec_shell" },
    }))?;
    store.save_item(&read_item)?;
    let pruned_item: crate::runtime_threads::TurnItemRecord = serde_json::from_value(json!({
        "id": "item_pruned", "turn_id": "turn_call_changes", "kind": "command_execution",
        "status": "completed", "summary": "bash started",
        "metadata": { "tool_use_id": PRUNED_CALL, "tool_name": "bash" },
    }))?;
    store.save_item(&pruned_item)?;
    // A call whose closing snapshot was lost: the opening receipt is recorded
    // and the span will never resolve.
    let half_item: crate::runtime_threads::TurnItemRecord = serde_json::from_value(json!({
        "id": "item_half", "turn_id": "turn_call_half", "kind": "command_execution",
        "status": "completed", "summary": "exec_shell started",
        "metadata": { "tool_use_id": "call_half", "tool_name": "exec_shell" },
    }))?;
    store.save_item(&half_item)?;
    let storeless_item: crate::runtime_threads::TurnItemRecord = serde_json::from_value(json!({
        "id": "item_storeless", "turn_id": "turn_storeless", "kind": "command_execution",
        "status": "completed", "summary": "bash started",
        "metadata": { "tool_use_id": STORELESS_CALL, "tool_name": "bash" },
    }))?;
    store.save_item(&storeless_item)?;

    let turn: TurnRecord = serde_json::from_value(json!({
        "id": "turn_call_changes", "thread_id": thread.id, "status": "completed",
        "input_summary": "run the script", "created_at": Utc::now(),
        "item_ids": ["item_shell", "item_read", "item_pruned"],
        "workspace_snapshots": [
            {
                "kind": "tool", "snapshot_id": tool.id.as_str(), "tree_id": tool.tree.as_str(),
                "session_id": thread.id, "tool_call_id": SHELL_CALL,
                "write_paths": null,
            },
            {
                "kind": "post_tool", "snapshot_id": post_tool.id.as_str(),
                "tree_id": post_tool.tree.as_str(), "session_id": thread.id,
                "tool_call_id": SHELL_CALL, "changed_paths": ["out.md", "script.py"],
            },
            {
                "kind": "tool", "snapshot_id": "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
                "tree_id": "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
                "session_id": thread.id, "tool_call_id": PRUNED_CALL,
            },
            {
                "kind": "post_tool", "snapshot_id": "feedfacefeedfacefeedfacefeedfacefeedface",
                "tree_id": "feedfacefeedfacefeedfacefeedfacefeedface",
                "session_id": thread.id, "tool_call_id": PRUNED_CALL,
                "changed_paths": ["gone.txt"],
            },
        ],
        "workspace": { "state": "settled" },
    }))?;
    store.save_turn(&turn)?;
    let half_turn: TurnRecord = serde_json::from_value(json!({
        "id": "turn_call_half", "thread_id": thread.id, "status": "completed",
        "input_summary": "run the script", "created_at": Utc::now(),
        "item_ids": ["item_half"],
        "workspace_snapshots": [{
            "kind": "tool", "snapshot_id": tool.id.as_str(), "tree_id": tool.tree.as_str(),
            "session_id": thread.id, "tool_call_id": "call_half",
        }],
        "workspace": { "state": "unavailable", "reason": "snapshot_failed" },
    }))?;
    store.save_turn(&half_turn)?;
    // The same pair of receipts, on a workspace whose store is gone rather
    // than merely pruned: one answer, not an internal error.
    let storeless_turn: TurnRecord = serde_json::from_value(json!({
        "id": "turn_storeless", "thread_id": storeless_thread.id, "status": "completed",
        "input_summary": "run the script", "created_at": Utc::now(),
        "item_ids": ["item_storeless"],
        "workspace_snapshots": [
            {
                "kind": "tool", "snapshot_id": "aa".repeat(20), "tree_id": "aa".repeat(20),
                "session_id": storeless_thread.id, "tool_call_id": STORELESS_CALL,
            },
            {
                "kind": "post_tool", "snapshot_id": "bb".repeat(20), "tree_id": "bb".repeat(20),
                "session_id": storeless_thread.id, "tool_call_id": STORELESS_CALL,
                "changed_paths": ["gone.txt"],
            },
        ],
        "workspace": { "state": "settled" },
    }))?;
    store.save_turn(&storeless_turn)?;

    let client = crate::tls::reqwest_client();
    let base = format!("http://{addr}");
    let url = |thread_id: &str, turn_id: &str, call: &str| {
        format!("{base}/v1/threads/{thread_id}/turns/{turn_id}/calls/{call}/changes")
    };
    let get = |thread_id: &str,
               turn_id: &str,
               call: &str|
     -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Value>> + Send + '_>,
    > {
        let client = client.clone();
        let url = url(thread_id, turn_id, call);
        Box::pin(async move {
            Ok(client
                .get(url)
                .bearer_auth("call-changes-token")
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?)
        })
    };

    // The item's persisted lifecycle owns the wait: an opening receipt with
    // no partner is normal while queued or running, not a permanent loss.
    let mut active_turn = half_turn.clone();
    active_turn.id = "turn_call_active".to_string();
    active_turn.status = RuntimeTurnStatus::InProgress;
    active_turn.item_ids = vec!["item_active".to_string()];
    active_turn.workspace = None;
    store.save_turn(&active_turn)?;
    let mut active_item = half_item.clone();
    active_item.id = "item_active".to_string();
    active_item.turn_id = active_turn.id.clone();
    for status in [
        TurnItemLifecycleStatus::Queued,
        TurnItemLifecycleStatus::InProgress,
    ] {
        active_item.status = status;
        store.save_item(&active_item)?;
        let pending = get(&thread.id, &active_turn.id, "call_half").await?;
        assert_eq!(pending["state"], "pending");
        assert_eq!(pending["reason"], Value::Null);
        assert_eq!(pending["files"].as_array().unwrap().len(), 0);
    }
    active_turn
        .workspace_snapshots
        .push(serde_json::from_value(json!({
            "kind": "post_tool", "snapshot_id": post_tool.id.as_str(),
            "tree_id": post_tool.tree.as_str(), "session_id": thread.id,
            "tool_call_id": "call_half", "changed_paths": ["out.md", "script.py"],
        }))?);
    active_turn.status = RuntimeTurnStatus::Completed;
    active_item.status = TurnItemLifecycleStatus::Completed;
    store.save_item(&active_item)?;
    store.save_turn(&active_turn)?;
    let settled = get(&thread.id, &active_turn.id, "call_half").await?;
    assert_eq!(settled["state"], "captured");
    assert_eq!(settled["reason"], Value::Null);
    assert_eq!(settled["files"].as_array().unwrap().len(), 2);

    let body = get(&thread.id, "turn_call_changes", SHELL_CALL).await?;
    assert_eq!(body["state"], "captured");
    assert_eq!(body["reason"], Value::Null);
    assert_eq!(body["tool_name"], "exec_shell");
    assert_eq!(body["truncated"], false);
    let files = body["files"].as_array().expect("files");
    let paths: Vec<&str> = files
        .iter()
        .map(|file| file["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        paths,
        ["out.md", "script.py"],
        "the span's own paths, in git's order: {files:?}"
    );

    let created = &files[0];
    assert_eq!(created["change"], "created");
    assert_eq!(created["added"], 1);
    assert_eq!(created["removed"], 0);
    // The span's end, not the work tree's newer bytes.
    assert_eq!(created["revision"], rev(b"written by the command\n"));
    assert_eq!(created["size"].as_u64(), Some(23));
    assert_eq!(
        created["restore_snapshot_id"],
        tool.tree.as_str(),
        "the revert point is the call's own `tool:` receipt"
    );
    assert!(
        created["diff"]
            .as_str()
            .unwrap()
            .contains("+written by the command"),
        "{}",
        created["diff"]
    );
    assert_eq!(created["diff_truncated"], false);

    let modified = &files[1];
    assert_eq!(modified["change"], "updated");
    assert_eq!(modified["added"], 1);
    assert_eq!(modified["removed"], 1);
    let patch = modified["diff"].as_str().unwrap();
    assert!(patch.contains("-print('v1')"), "{patch}");
    assert!(patch.contains("+print('v2')"), "{patch}");
    assert!(
        !body.to_string().contains("after.md"),
        "a write after the span is not the call's: {body}"
    );

    // A call the turn recorded but the engine never bounded: known, and not
    // reported as "changed nothing".
    let unbounded = get(&thread.id, "turn_call_changes", "call_read").await?;
    assert_eq!(unbounded["state"], "unavailable");
    assert_eq!(unbounded["reason"], "call_not_bounded");
    assert_eq!(unbounded["files"].as_array().unwrap().len(), 0);

    // A span whose trees have been pruned is a fact about the store, not a
    // failure of the read: the receipt's own path list is what a client keeps.
    let pruned = get(&thread.id, "turn_call_changes", PRUNED_CALL).await?;
    assert_eq!(pruned["state"], "unavailable");
    assert_eq!(pruned["reason"], "snapshots_pruned");
    assert_eq!(pruned["files"].as_array().unwrap().len(), 0);

    // A workspace with no snapshot store at all answers the same way: the
    // receipts cannot be resolved, and that is not a server failure.
    let storeless: Value = client
        .get(url(&storeless_thread.id, "turn_storeless", STORELESS_CALL))
        .bearer_auth("call-changes-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(storeless["state"], "unavailable");
    assert_eq!(storeless["reason"], "snapshots_pruned");

    // A span whose closing receipt is gone will never resolve, and says why.
    let half = get(&thread.id, "turn_call_half", "call_half").await?;
    assert_eq!(half["state"], "unavailable");
    assert_eq!(half["reason"], "post_snapshot_missing");

    // A corrupt item cannot turn a known call into an unavailable span.
    let item_path = tmp
        .path()
        // This harness roots the Runtime store at <root>/runtime/runtime.
        .join("runtime")
        .join("runtime")
        .join("runtime")
        .join("items")
        .join(format!("{}.json", half_item.id));
    let saved_item = fs::read(&item_path)?;
    fs::write(&item_path, b"invalid runtime item JSON\n")?;
    let response = client
        .get(url(&thread.id, "turn_call_half", "call_half"))
        .bearer_auth("call-changes-token")
        .send()
        .await;
    fs::write(&item_path, saved_item)?;
    let response = response?;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let error: Value = response.json().await?;
    assert!(
        error.to_string().contains("Failed to parse item"),
        "{error}"
    );

    // A call this turn never ran, an unknown turn, and a turn of another
    // thread are 404s rather than empty answers.
    for (thread_id, turn_id, call) in [
        (thread.id.as_str(), "turn_call_changes", "call_never"),
        (thread.id.as_str(), "turn_missing", "call_shell"),
        (thread.id.as_str(), "turn_call_half", "call_shell"),
        ("thread_missing", "turn_call_changes", SHELL_CALL),
    ] {
        let status = client
            .get(url(thread_id, turn_id, call))
            .bearer_auth("call-changes-token")
            .send()
            .await?
            .status();
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "{thread_id}/{turn_id}/{call}"
        );
    }
    let status = client
        .get(format!(
            "{}?limit=0",
            url(&thread.id, "turn_call_changes", SHELL_CALL)
        ))
        .bearer_auth("call-changes-token")
        .send()
        .await?
        .status();
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // A store is present, but git cannot read its metadata. Preserve the
    // operational failure rather than telling clients these trees were pruned.
    let head = repo.git_dir().join("HEAD");
    let saved_head = fs::read(&head)?;
    fs::write(&head, b"invalid snapshot repository metadata\n")?;
    let response = client
        .get(url(&thread.id, "turn_call_changes", SHELL_CALL))
        .bearer_auth("call-changes-token")
        .send()
        .await;
    fs::write(&head, saved_head)?;
    let response = response?;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let error: Value = response.json().await?;
    assert!(
        error.to_string().contains("snapshot tree lookup failed"),
        "{error}"
    );

    handle.abort();
    Ok(())
}

// ---------------------------------------------------------------------------
// /v1/jobs: client-owned shell jobs on the thread's shared ShellManager.
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[tokio::test]
async fn jobs_api_lists_creates_streams_stdin_and_kills() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let workspace = tmp.path().join("workspace");
    fs::create_dir_all(&workspace)?;
    let (addr, _runtime_threads, handle) = spawn_test_server_with_root_token_mobile_workspace(
        tmp.path().join("runtime"),
        tmp.path().join("sessions"),
        Some("jobs-token".to_string()),
        false,
        workspace.clone(),
    )
    .await?
    .context("jobs test requires a loopback listener")?;
    let client = crate::tls::reqwest_client();
    let base = format!("http://{addr}");

    // Auth is required on every jobs route.
    for route in ["/v1/jobs", "/v1/threads/thread-x/jobs"] {
        let status = client.get(format!("{base}{route}")).send().await?.status();
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{route}");
    }

    let thread: Value = client
        .post(format!("{base}/v1/threads"))
        .bearer_auth("jobs-token")
        .json(&json!({ "workspace": workspace, "allow_shell": true }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let thread_id = thread["id"].as_str().unwrap().to_string();

    // A shell-forbidden thread refuses job creation.
    let no_shell: Value = client
        .post(format!("{base}/v1/threads"))
        .bearer_auth("jobs-token")
        .json(&json!({ "workspace": workspace, "allow_shell": false }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let no_shell_id = no_shell["id"].as_str().unwrap().to_string();
    let status = client
        .post(format!("{base}/v1/threads/{no_shell_id}/jobs"))
        .bearer_auth("jobs-token")
        .json(&json!({ "command": "echo never" }))
        .send()
        .await?
        .status();
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Unknown thread and unknown job are honest 404s, and a thread that never
    // ran shell work lists empty rather than fabricating a manager.
    let status = client
        .get(format!("{base}/v1/threads/thread-missing/jobs"))
        .bearer_auth("jobs-token")
        .send()
        .await?
        .status();
    assert_eq!(status, StatusCode::NOT_FOUND);
    let empty: Value = client
        .get(format!("{base}/v1/threads/{no_shell_id}/jobs"))
        .bearer_auth("jobs-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(empty["jobs"].as_array().unwrap().len(), 0);
    let status = client
        .get(format!(
            "{base}/v1/threads/{thread_id}/jobs/job-missing/output"
        ))
        .bearer_auth("jobs-token")
        .send()
        .await?
        .status();
    // The thread has no manager yet, so this is a not-found either way.
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Create a client-owned background job.
    let created = client
        .post(format!("{base}/v1/threads/{thread_id}/jobs"))
        .bearer_auth("jobs-token")
        .json(&json!({ "command": "echo codewhale-jobs && echo second-line" }))
        .send()
        .await?
        .error_for_status()?;
    assert_eq!(created.status(), StatusCode::CREATED);
    let created: Value = created.json().await?;
    let job_id = created["job"]["id"].as_str().unwrap().to_string();
    assert_eq!(created["job"]["owner"], "client");
    assert_eq!(created["job"]["thread_id"], thread_id);
    assert_eq!(
        created["job"]["command"],
        "echo codewhale-jobs && echo second-line"
    );

    // Poll the byte stream until the job exits; the cursor contract replays
    // the full stream from 0 for a late reader.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut chunk: Value;
    loop {
        chunk = client
            .get(format!(
                "{base}/v1/threads/{thread_id}/jobs/{job_id}/output?format=text"
            ))
            .bearer_auth("jobs-token")
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if chunk["done"].as_bool().unwrap() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "job did not finish: {chunk}"
        );
        sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(chunk["status"], "Completed");
    assert_eq!(chunk["exit_code"], 0);
    assert!(chunk["data"].as_str().unwrap().contains("codewhale-jobs"));
    assert_eq!(chunk["dropped"], 0);

    // A cursor at the end stays put; a mid-stream cursor resumes exactly.
    let total = chunk["total"].as_u64().unwrap();
    let tail: Value = client
        .get(format!(
            "{base}/v1/threads/{thread_id}/jobs/{job_id}/output?format=text&cursor={total}"
        ))
        .bearer_auth("jobs-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(tail["data"], "");
    assert_eq!(tail["next_cursor"], total);
    assert_eq!(tail["done"], true);

    // stdin round-trip: a `cat` job echoes what the API writes.
    let cat: Value = client
        .post(format!("{base}/v1/threads/{thread_id}/jobs"))
        .bearer_auth("jobs-token")
        .json(&json!({ "command": "cat" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let cat_id = cat["job"]["id"].as_str().unwrap().to_string();
    assert_eq!(cat["job"]["stdin_available"], true);
    let status = client
        .post(format!("{base}/v1/threads/{thread_id}/jobs/{cat_id}/stdin"))
        .bearer_auth("jobs-token")
        .json(&json!({ "data": "typed-by-client\n", "close": true }))
        .send()
        .await?
        .status();
    assert_eq!(status, StatusCode::NO_CONTENT);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let echoed = loop {
        let chunk: Value = client
            .get(format!(
                "{base}/v1/threads/{thread_id}/jobs/{cat_id}/output?format=text"
            ))
            .bearer_auth("jobs-token")
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if chunk["done"].as_bool().unwrap() {
            break chunk;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "cat job did not finish: {chunk}"
        );
        sleep(std::time::Duration::from_millis(50)).await;
    };
    assert_eq!(echoed["data"], "typed-by-client\n");

    // The flat listing sees both jobs, tagged to this thread.
    let all: Value = client
        .get(format!("{base}/v1/jobs"))
        .bearer_auth("jobs-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let ids: Vec<&str> = all["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|job| job["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&job_id.as_str()) && ids.contains(&cat_id.as_str()));

    // Kill stops a running job and reports the terminal snapshot.
    let sleeper: Value = client
        .post(format!("{base}/v1/threads/{thread_id}/jobs"))
        .bearer_auth("jobs-token")
        .json(&json!({ "command": "sleep 60" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let sleeper_id = sleeper["job"]["id"].as_str().unwrap().to_string();
    let killed: Value = client
        .post(format!(
            "{base}/v1/threads/{thread_id}/jobs/{sleeper_id}/kill"
        ))
        .bearer_auth("jobs-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(killed["job"]["status"], "Killed");

    // A job id from another thread's scope is a scoped 404, not a leak.
    let other: Value = client
        .post(format!("{base}/v1/threads"))
        .bearer_auth("jobs-token")
        .json(&json!({ "workspace": workspace, "allow_shell": true }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let other_id = other["id"].as_str().unwrap().to_string();
    let status = client
        .get(format!("{base}/v1/threads/{other_id}/jobs/{job_id}"))
        .bearer_auth("jobs-token")
        .send()
        .await?
        .status();
    assert_eq!(status, StatusCode::NOT_FOUND);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn thread_context_reports_live_budget_from_the_engine() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let workspace = tmp.path().join("workspace");
    fs::create_dir_all(&workspace)?;
    let (addr, _runtime_threads, handle) = spawn_test_server_with_root_token_mobile_workspace(
        tmp.path().join("runtime"),
        tmp.path().join("sessions"),
        Some("context-token".to_string()),
        false,
        workspace.clone(),
    )
    .await?
    .context("context test requires a loopback listener")?;
    let client = crate::tls::reqwest_client();
    let base = format!("http://{addr}");

    let status = client
        .get(format!("{base}/v1/threads/thread-x/context"))
        .send()
        .await?
        .status();
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let status = client
        .get(format!("{base}/v1/threads/thread-missing/context"))
        .bearer_auth("context-token")
        .send()
        .await?
        .status();
    assert_eq!(status, StatusCode::NOT_FOUND);

    let thread: Value = client
        .post(format!("{base}/v1/threads"))
        .bearer_auth("context-token")
        .json(&json!({ "workspace": workspace }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let thread_id = thread["id"].as_str().unwrap().to_string();

    let context: Value = client
        .get(format!("{base}/v1/threads/{thread_id}/context"))
        .bearer_auth("context-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(context["thread_id"], thread_id);
    assert_eq!(context["live"], true);
    let window = context["window_tokens"].as_u64().unwrap();
    assert!(window > 0, "a resolved route must report its window");
    assert!(context["input_tokens"].as_u64().unwrap() > 0);
    assert!(context["available_input_tokens"].as_u64().unwrap() <= window);
    assert!(context["compaction_trigger_tokens"].as_u64().unwrap() <= window);
    let percent = context["usage_percent"].as_f64().unwrap();
    assert!((0.0..=100.0).contains(&percent));
    assert!(matches!(
        context["pressure"].as_str(),
        Some("low" | "moderate" | "high" | "critical")
    ));
    // Fresh thread: no provider-billed count exists yet — null, not zero.
    assert!(context["billed_input_tokens"].is_null());

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn provider_key_write_is_write_only_and_reports_readiness() -> Result<()> {
    // The credential store is only exposed to tests under an explicit isolated
    // home plus an explicit backend — both scoped to this test by the env lock.
    let _env_lock = crate::test_support::lock_test_env();
    let tmp = tempfile::tempdir()?;
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", tmp.path().join("cwhome"));
    let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    let _cli_key = crate::test_support::EnvVarGuard::remove("CODEWHALE_CLI_API_KEY");
    let _provider_keys: Vec<_> = ProviderKind::Deepseek
        .provider()
        .env_vars()
        .iter()
        .copied()
        .map(crate::test_support::EnvVarGuard::remove)
        .collect();
    fs::create_dir_all(tmp.path().join("cwhome"))?;
    let workspace = tmp.path().join("workspace");
    fs::create_dir_all(&workspace)?;
    let config_path = tmp.path().join("config.toml");
    fs::write(
        &config_path,
        "provider = \"deepseek\"\n[providers.deepseek]\nbase_url = \"http://127.0.0.1:1/v1\"\n",
    )?;
    let secret_path = tmp.path().join("cwhome/secrets/secrets.json");
    let live_config = Arc::new(parking_lot::RwLock::new(Config::default()));

    // No literal DeepSeek key in the live config: the older harness kept one
    // at the top level, where it silently outranked the store. Since #6394
    // that key is a `[providers.deepseek]` literal and the write refuses.
    let (addr, _runtime_threads, handle) =
        spawn_test_server_with_root_token_mobile_workspace_and_overrides(
            tmp.path().join("runtime"),
            tmp.path().join("sessions"),
            Some("keys-token".to_string()),
            false,
            workspace.clone(),
            TestServerOverrides {
                config_path: Some(config_path.clone()),
                config_handle: Some(live_config.clone()),
                ..TestServerOverrides::default()
            },
        )
        .await?
        .context("secrets test requires a loopback listener")?;
    let client = crate::tls::reqwest_client();
    let base = format!("http://{addr}");

    let status = client
        .put(format!("{base}/v1/providers/openai/key"))
        .json(&json!({ "key": "sk-test-write-only" }))
        .send()
        .await?
        .status();
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    for (id, want) in [
        ("not-a-provider", StatusCode::BAD_REQUEST),
        ("deepseek-cn", StatusCode::BAD_REQUEST),
    ] {
        let status = client
            .put(format!("{base}/v1/providers/{id}/key"))
            .bearer_auth("keys-token")
            .json(&json!({ "key": "sk-test" }))
            .send()
            .await?
            .status();
        assert_eq!(status, want, "{id}");
    }
    let status = client
        .put(format!("{base}/v1/providers/openai/key"))
        .bearer_auth("keys-token")
        .json(&json!({ "key": "   " }))
        .send()
        .await?
        .status();
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // An unsupported write is a client error before opening a secret backend
    // or changing either config copy. No opaque setter error is reclassified.
    let config_before = fs::read(&config_path)?;
    assert!(!secret_path.exists());
    let refused = client
        .put(format!("{base}/v1/providers/openai-codex/key"))
        .bearer_auth("keys-token")
        .json(&json!({ "key": "synthetic-unsupported-codex-key" }))
        .send()
        .await?;
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let refused: Value = refused.json().await?;
    assert_eq!(
        refused["error"]["message"],
        codewhale_config::credentials::OPENAI_CODEX_API_KEY_REFUSAL
    );
    assert!(
        !refused
            .to_string()
            .contains("synthetic-unsupported-codex-key")
    );
    assert_eq!(fs::read(&config_path)?, config_before);
    assert!(!secret_path.exists());
    {
        let live = live_config.read();
        assert_eq!(live.auth_mode, None);
        assert_eq!(
            live.provider_config_for(&live.test_identity_for_kind(ProviderKind::OpenaiCodex))
                .and_then(|entry| entry.auth_mode.as_deref()),
            None
        );
    }

    // A key written for a *non-active* provider is the harder case: the
    // readiness catalog only probes the secret store for it when the
    // auth-mode save marker is visible, so the route must mirror the
    // persisted marker into the live config for the readback to be honest.
    let receipt: Value = client
        .put(format!("{base}/v1/providers/openai/key"))
        .bearer_auth("keys-token")
        .json(&json!({ "key": "sk-test-write-only-key-material" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(receipt["provider"], "openai");
    assert_eq!(receipt["stored"], true);
    // The receipt is a redacted receipt: no key bytes, not even the length.
    let raw = receipt.to_string();
    assert!(!raw.contains("sk-test-write-only-key-material"));
    assert_eq!(receipt["credentialState"], "configured");

    // The readiness readback agrees through the providers catalog too.
    let providers: Value = client
        .get(format!("{base}/v1/providers"))
        .bearer_auth("keys-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let openai = providers["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "openai")
        .expect("openai is listed");
    assert_eq!(openai["credentialState"], "configured");

    // Before the active provider has a saved key, its local route must stay
    // keyless. An unconditional live root marker changes this to "missing".
    let deepseek = providers["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "deepseek")
        .expect("deepseek is listed");
    assert_eq!(deepseek["credentialState"], "local");
    {
        let live = live_config.read();
        assert_eq!(live.auth_mode, None);
        assert_eq!(
            live.provider_config_for(&live.test_identity_for_kind(ProviderKind::Deepseek))
                .and_then(|entry| entry.auth_mode.as_deref()),
            None
        );
        assert_eq!(
            live.provider_config_for(&live.test_identity_for_kind(ProviderKind::Openai))
                .and_then(|entry| entry.auth_mode.as_deref()),
            Some("api_key")
        );
    }
    let saved = codewhale_config::ConfigStore::load(Some(config_path.clone()))?;
    assert_eq!(saved.config.auth_mode, None);
    assert_eq!(
        saved.config.providers.openai.auth_mode.as_deref(),
        Some("api_key")
    );
    assert_eq!(saved.config.providers.openai_codex.auth_mode, None);

    // A key written for the *active* provider reports configured through the
    // live store even though the test route is a keyless local endpoint —
    // saving a key declares the api-key contract, exactly as `auth set` would.
    let receipt: Value = client
        .put(format!("{base}/v1/providers/deepseek/key"))
        .bearer_auth("keys-token")
        .json(&json!({ "key": "sk-test-active-route-key" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(receipt["provider"], "deepseek");
    assert_eq!(receipt["stored"], true);
    assert!(!receipt.to_string().contains("sk-test-active-route-key"));
    assert_eq!(receipt["credentialState"], "configured");

    {
        let live = live_config.read();
        assert_eq!(live.auth_mode.as_deref(), Some("api_key"));
        assert_eq!(
            live.provider_config_for(&live.test_identity_for_kind(ProviderKind::Deepseek))
                .and_then(|entry| entry.auth_mode.as_deref()),
            Some("api_key")
        );
    }
    let saved = codewhale_config::ConfigStore::load(Some(config_path.clone()))?;
    assert_eq!(saved.config.auth_mode.as_deref(), Some("api_key"));
    assert_eq!(
        saved.config.providers.deepseek.auth_mode.as_deref(),
        Some("api_key")
    );

    // A real backend read failure still reports a server error and leaves
    // the last committed metadata and damaged backend bytes untouched.
    let config_before = fs::read(&config_path)?;
    fs::write(&secret_path, b"not valid secret-store JSON")?;
    let failed = client
        .put(format!("{base}/v1/providers/openrouter/key"))
        .bearer_auth("keys-token")
        .json(&json!({ "key": "synthetic-failed-storage-key" }))
        .send()
        .await?;
    assert_eq!(failed.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let failed = failed.text().await?;
    assert!(failed.contains("credential write failed"));
    assert!(!failed.contains("synthetic-failed-storage-key"));
    assert_eq!(fs::read(&config_path)?, config_before);
    assert_eq!(fs::read(&secret_path)?, b"not valid secret-store JSON");
    {
        let live = live_config.read();
        assert_eq!(live.auth_mode.as_deref(), Some("api_key"));
        assert_eq!(
            live.provider_config_for(&live.test_identity_for_kind(ProviderKind::Openrouter))
                .and_then(|entry| entry.auth_mode.as_deref()),
            None
        );
    }

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn diagnostics_list_and_read_bounded_windows() -> Result<()> {
    // Log/crash directories resolve from the codewhale home — isolate it.
    let _env_lock = crate::test_support::lock_test_env();
    let tmp = tempfile::tempdir()?;
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", tmp.path().join("cwhome"));
    // An explicit profile owns both logs and crashes, even when ambient
    // legacy crash files exist outside it.
    let _user_home = crate::test_support::EnvVarGuard::set("HOME", tmp.path().join("userhome"));
    let logs = tmp.path().join("cwhome").join("logs");
    let crashes = tmp.path().join("cwhome").join("crashes");
    let ambient_crashes = tmp
        .path()
        .join("userhome")
        .join(".deepseek")
        .join("crashes");
    fs::create_dir_all(&ambient_crashes)?;
    fs::write(
        ambient_crashes.join("ambient-only.log"),
        "not in this profile",
    )?;
    fs::create_dir_all(&logs)?;
    fs::create_dir_all(&crashes)?;
    fs::write(logs.join("tui-20990101-1.log"), "line one\nline two\n")?;
    fs::write(tmp.path().join("cwhome/audit.log"), "{\"event\":\"x\"}\n")?;
    fs::write(crashes.join("20990101T000000Z-turn.log"), "Panic: boom\n")?;
    let workspace = tmp.path().join("workspace");
    fs::create_dir_all(&workspace)?;

    let (addr, _runtime_threads, handle) = spawn_test_server_with_root_token_mobile_workspace(
        tmp.path().join("runtime"),
        tmp.path().join("sessions"),
        Some("diag-token".to_string()),
        false,
        workspace,
    )
    .await?
    .context("diagnostics test requires a loopback listener")?;
    let client = crate::tls::reqwest_client();
    let base = format!("http://{addr}");

    let status = client.get(format!("{base}/v1/logs")).send().await?.status();
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let listed: Value = client
        .get(format!("{base}/v1/logs"))
        .bearer_auth("diag-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let names: Vec<&str> = listed["sources"][0]["files"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|f| f["name"].as_str())
        .collect();
    assert!(names.contains(&"tui-20990101-1.log"));
    assert!(names.contains(&"audit.log"));

    // Tail read returns the last bytes only.
    let tail: Value = client
        .get(format!("{base}/v1/logs/tui-20990101-1.log?tail=9"))
        .bearer_auth("diag-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(tail["content"], "line two\n");
    assert_eq!(tail["encoding"], "utf-8");
    // A tail window reaches EOF; the skipped prefix shows up as offset > 0.
    assert_eq!(tail["offset"], 9);
    assert_eq!(tail["truncated"], false);

    let crashes_list: Value = client
        .get(format!("{base}/v1/crashes"))
        .bearer_auth("diag-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(crashes_list["sources"].as_array().unwrap().len(), 1);
    assert_eq!(crashes_list["sources"][0]["dir"], json!(crashes));
    assert!(
        crashes_list["sources"][0]["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["name"] == "20990101T000000Z-turn.log")
    );
    let crash: Value = client
        .get(format!("{base}/v1/crashes/20990101T000000Z-turn.log"))
        .bearer_auth("diag-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(crash["content"], "Panic: boom\n");

    let status = client
        .get(format!("{base}/v1/crashes/ambient-only.log"))
        .bearer_auth("diag-token")
        .send()
        .await?
        .status();
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Traversal and missing files fail closed.
    let status = client
        .get(format!("{base}/v1/logs/..%2Fsecret"))
        .bearer_auth("diag-token")
        .send()
        .await?
        .status();
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let status = client
        .get(format!("{base}/v1/logs/missing.log"))
        .bearer_auth("diag-token")
        .send()
        .await?
        .status();
    assert_eq!(status, StatusCode::NOT_FOUND);
    let status = client
        .get(format!("{base}/v1/crashes/missing.log"))
        .bearer_auth("diag-token")
        .send()
        .await?
        .status();
    assert_eq!(status, StatusCode::NOT_FOUND);

    let process: Value = client
        .get(format!("{base}/v1/process"))
        .bearer_auth("diag-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(process["pid"], json!(std::process::id()));
    assert!(process["uptime_seconds"].is_number());
    assert_eq!(process["version"], env!("CARGO_PKG_VERSION"));

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn targets_report_self_and_probe_remote_endpoints() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let workspace = tmp.path().join("workspace");
    fs::create_dir_all(&workspace)?;

    let (addr, _runtime_threads, handle) = spawn_test_server_with_root_token_mobile_workspace(
        tmp.path().join("runtime"),
        tmp.path().join("sessions"),
        Some("targets-token".to_string()),
        false,
        workspace,
    )
    .await?
    .context("targets test requires a loopback listener")?;
    let client = crate::tls::reqwest_client();
    let base = format!("http://{addr}");

    let status = client
        .get(format!("{base}/v1/targets"))
        .send()
        .await?
        .status();
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let targets: Value = client
        .get(format!("{base}/v1/targets"))
        .bearer_auth("targets-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let local = &targets["targets"][0];
    assert_eq!(local["kind"], "local");
    assert_eq!(local["current"], true);
    assert_eq!(local["service"], "codewhale-runtime-api");
    assert_eq!(local["auth_required"], true);
    assert_eq!(local["endpoint"], json!(base));
    // Control-plane-owned surfaces answer honestly instead of 404ing.
    assert_eq!(targets["ssh"]["supported"], false);
    assert_eq!(targets["ssh"]["owner"], "codewhale-control-plane");
    assert_eq!(targets["cloud"]["supported"], false);
    assert_eq!(targets["remote"]["supported"], true);

    let remote: Value = client
        .get(format!("{base}/v1/remote"))
        .bearer_auth("targets-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(remote["loopback_only"], true);
    assert_eq!(remote["auth_required"], true);
    assert_eq!(remote["port"], json!(addr.port()));

    // The probe identifies this same server as a Codewhale runtime.
    let probed: Value = client
        .post(format!("{base}/v1/remote/connect"))
        .bearer_auth("targets-token")
        .json(&json!({"endpoint": base}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(probed["ok"], true);
    assert_eq!(probed["remote"]["service"], "codewhale-runtime-api");
    assert_eq!(probed["remote"]["auth_required"], true);
    assert_eq!(probed["attach"], "client");

    // A closed port is an honest negative verdict, not a server error.
    let refused: Value = client
        .post(format!("{base}/v1/remote/connect"))
        .bearer_auth("targets-token")
        .json(&json!({"endpoint": "http://127.0.0.1:1"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(refused["ok"], false);
    assert_eq!(refused["reason"], "unreachable");

    // Malformed, non-http, and credentialed endpoints fail closed at 400.
    for endpoint in [
        "not-a-url",
        "ftp://example.com",
        "http://user:pass@127.0.0.1:7878",
    ] {
        let status = client
            .post(format!("{base}/v1/remote/connect"))
            .bearer_auth("targets-token")
            .json(&json!({"endpoint": endpoint}))
            .send()
            .await?
            .status();
        assert_eq!(status, StatusCode::BAD_REQUEST, "endpoint {endpoint}");
    }
    // A token field is an unknown field — never accepted for forwarding
    // (axum's JSON extractor answers deserialization failures with 422).
    let status = client
        .post(format!("{base}/v1/remote/connect"))
        .bearer_auth("targets-token")
        .json(&json!({"endpoint": base, "token": "x"}))
        .send()
        .await?
        .status();
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Client-owned registry + control-plane-owned surfaces refuse writes.
    for (method, path) in [
        ("POST", "/v1/targets"),
        ("POST", "/v1/targets/switch"),
        ("POST", "/v1/ssh/connect"),
        ("POST", "/v1/cloud/attach"),
    ] {
        let status = client
            .request(method.parse()?, format!("{base}{path}"))
            .bearer_auth("targets-token")
            .json(&json!({}))
            .send()
            .await?
            .status();
        assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{method} {path}");
    }
    let ssh: Value = client
        .get(format!("{base}/v1/ssh"))
        .bearer_auth("targets-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(ssh["supported"], false);
    let cloud: Value = client
        .get(format!("{base}/v1/cloud"))
        .bearer_auth("targets-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(cloud["supported"], false);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn lsp_routes_serve_workspace_files_through_a_transport() -> Result<()> {
    use crate::lsp::diagnostics::{Diagnostic, Severity};
    use crate::lsp::registry::Language;
    use crate::lsp::tests::FakeTransport;

    let tmp = tempfile::tempdir()?;
    let workspace = tmp.path().join("workspace");
    fs::create_dir_all(&workspace)?;
    fs::write(workspace.join("main.rs"), "fn main() {}\n")?;
    fs::write(workspace.join("notes.unknownext"), "hi\n")?;

    // Seed the shared manager with a fake Rust transport — no real LSP spawn.
    let manager = Arc::new(crate::lsp::LspManager::new(
        crate::lsp::LspConfig::default(),
        workspace.canonicalize()?,
    ));
    manager
        .install_test_transport(
            Language::Rust,
            Arc::new(FakeTransport::new(vec![Diagnostic {
                line: 1,
                column: 4,
                severity: Severity::Error,
                message: "test diagnostic".to_string(),
            }])),
        )
        .await;

    let (addr, _runtime_threads, handle) =
        spawn_test_server_with_root_token_mobile_workspace_and_overrides(
            tmp.path().join("runtime"),
            tmp.path().join("sessions"),
            Some("lsp-token".to_string()),
            false,
            workspace,
            TestServerOverrides {
                lsp_manager: Some(manager),
                ..TestServerOverrides::default()
            },
        )
        .await?
        .context("lsp test requires a loopback listener")?;
    let client = crate::tls::reqwest_client();
    let base = format!("http://{addr}");

    let status = client.get(format!("{base}/v1/lsp")).send().await?.status();
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let capability: Value = client
        .get(format!("{base}/v1/lsp"))
        .bearer_auth("lsp-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(capability["enabled"], true);
    assert!(
        capability["languages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["language"] == "rust")
    );

    // Real diagnostics through the fake transport.
    let diagnostics: Value = client
        .get(format!("{base}/v1/diagnostics?path=main.rs"))
        .bearer_auth("lsp-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(diagnostics["ok"], true);
    assert_eq!(diagnostics["items"][0]["severity"], "error");
    assert_eq!(diagnostics["items"][0]["message"], "test diagnostic");

    // A language without a server is honest data, not an HTTP error.
    let none: Value = client
        .get(format!("{base}/v1/diagnostics?path=notes.unknownext"))
        .bearer_auth("lsp-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(none["ok"], false);
    assert_eq!(none["reason"], "no_server");

    // Position routes validate `line` before touching LSP.
    let status = client
        .get(format!("{base}/v1/definition?path=main.rs"))
        .bearer_auth("lsp-token")
        .send()
        .await?
        .status();
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // The fake transport declines request-style ops — surfaced as ok:false.
    let definition: Value = client
        .get(format!(
            "{base}/v1/definition?path=main.rs&line=1&character=5"
        ))
        .bearer_auth("lsp-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(definition["ok"], false);
    assert_eq!(definition["reason"], "lsp_error");

    // Traversal and missing files fail closed.
    for path in ["../escape.rs", ".git/config", "missing.rs"] {
        let status = client
            .get(format!("{base}/v1/diagnostics?path={path}"))
            .bearer_auth("lsp-token")
            .send()
            .await?
            .status();
        assert!(
            matches!(
                status,
                StatusCode::BAD_REQUEST | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
            ),
            "path {path} -> {status}"
        );
    }

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn voice_routes_report_capability_and_fail_closed_without_a_recorder() -> Result<()> {
    let _lock = lock_test_env();
    // Deterministic: the operator kill-switch keeps the test off the host mic.
    let _voice_off = EnvVarGuard::set("CODEWHALE_DISABLE_VOICE", "1");

    let tmp = tempfile::tempdir()?;
    let workspace = tmp.path().join("workspace");
    fs::create_dir_all(&workspace)?;

    let (addr, _runtime_threads, handle) =
        spawn_test_server_with_root_token_mobile_workspace_and_overrides(
            tmp.path().join("runtime"),
            tmp.path().join("sessions"),
            Some("voice-token".to_string()),
            false,
            workspace,
            TestServerOverrides::default(),
        )
        .await?
        .context("voice test requires a loopback listener")?;
    let client = crate::tls::reqwest_client();
    let base = format!("http://{addr}");

    let status = client
        .get(format!("{base}/v1/voice"))
        .send()
        .await?
        .status();
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    for path in ["/v1/voice/dictate", "/v1/voice/send", "/v1/voice/control"] {
        let status = client.post(format!("{base}{path}")).send().await?.status();
        assert_eq!(status, StatusCode::UNAUTHORIZED, "POST {path}");
    }

    let status: Value = client
        .get(format!("{base}/v1/voice"))
        .bearer_auth("voice-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(status["available"], false);
    assert!(status["recorder"].is_null());
    assert!(!status["asr"]["kind"].as_str().unwrap().is_empty());
    assert_eq!(status["modes"].as_array().unwrap().len(), 3);
    assert!(status["send_phrases"].as_array().unwrap().len() >= 3);
    assert_eq!(status["max_record_seconds"], 10);

    // Dictation with no recorder fails closed as data — never an HTTP 5xx.
    for path in ["/v1/voice/dictate", "/v1/voice/send"] {
        let outcome: Value = client
            .post(format!("{base}{path}"))
            .bearer_auth("voice-token")
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_eq!(outcome["ok"], false, "{path}");
        assert_eq!(outcome["reason"], "no_recorder", "{path}");
    }

    // Control mode takes an optional {"composer"} body; still no recorder.
    let outcome: Value = client
        .post(format!("{base}/v1/voice/control"))
        .bearer_auth("voice-token")
        .json(&json!({ "composer": "draft text" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(outcome["ok"], false);
    assert_eq!(outcome["reason"], "no_recorder");

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn git_routes_drive_a_real_workspace_repo() -> Result<()> {
    use crate::dependencies::{ExternalTool as _, Git};

    let tmp = tempfile::tempdir()?;
    let workspace = tmp.path().join("workspace");
    fs::create_dir_all(&workspace)?;
    let git = |args: &[&str]| {
        let output = Git::output(args, &workspace)?;
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok::<_, anyhow::Error>(output)
    };
    git(&["init", "-b", "main"])?;
    git(&["config", "user.email", "runtime-api@example.test"])?;
    git(&["config", "user.name", "Runtime API Test"])?;
    // Windows git defaults to core.autocrlf=true, so `git checkout` rewrites
    // LF to CRLF on restore and the discard assertion below reads back
    // "v1\r\n" instead of "v1\n". Pin the fixture repo's line endings so the
    // test measures the route rather than the host's git config.
    git(&["config", "core.autocrlf", "false"])?;
    fs::write(workspace.join("tracked.txt"), "v1\n")?;
    git(&["add", "tracked.txt"])?;
    git(&["commit", "-m", "initial"])?;

    let (addr, _runtime_threads, handle) = spawn_test_server_with_root_token_mobile_workspace(
        tmp.path().join("runtime"),
        tmp.path().join("sessions"),
        Some("git-token".to_string()),
        false,
        workspace.clone(),
    )
    .await?
    .context("git test requires a loopback listener")?;
    let client = crate::tls::reqwest_client();
    let base = format!("http://{addr}");

    let status = client.get(format!("{base}/v1/git")).send().await?.status();
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Untracked file shows up in the detail read.
    fs::write(workspace.join("new.rs"), "fn main() {}\n")?;
    let detail: Value = client
        .get(format!("{base}/v1/git"))
        .bearer_auth("git-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(detail["git_repo"], true);
    assert_eq!(detail["branch"], "main");
    assert!(
        detail["files"].as_array().unwrap().iter().any(|f| {
            f["path"] == "new.rs" && f["status"] == "untracked" && f["staged"] == false
        })
    );
    assert_eq!(detail["branches"], json!(["main"]));

    // Stage → status reflects the index; commit lands the change.
    let staged: Value = client
        .post(format!("{base}/v1/git/stage"))
        .bearer_auth("git-token")
        .json(&json!({ "paths": ["new.rs"] }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(staged["ok"], true);
    assert_eq!(staged["status"]["staged"], 1);
    let committed: Value = client
        .post(format!("{base}/v1/git/commit"))
        .bearer_auth("git-token")
        .json(&json!({ "message": "add new.rs" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(committed["ok"], true);
    assert_eq!(committed["status"]["staged"], 0);

    // Branch create+switch, then the graph reports both commits on it.
    let switched: Value = client
        .post(format!("{base}/v1/git/branch"))
        .bearer_auth("git-token")
        .json(&json!({ "name": "feature-x", "create": true }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(switched["ok"], true);
    assert_eq!(switched["status"]["branch"], "feature-x");
    let graph: Value = client
        .get(format!("{base}/v1/git/graph"))
        .bearer_auth("git-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let commits = graph["commits"].as_array().unwrap();
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0]["subject"], "add new.rs");
    assert!(commits[1]["id"].as_str().unwrap().len() == 40);

    // Discard restores a tracked file; discarding an untracked path fails
    // closed with git's own message rather than deleting the file.
    fs::write(workspace.join("tracked.txt"), "v2\n")?;
    let detail: Value = client
        .get(format!("{base}/v1/git"))
        .bearer_auth("git-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert!(detail["files"].as_array().unwrap().iter().any(|f| {
        f["path"] == "tracked.txt" && f["status"] == "modified" && f["staged"] == false
    }));

    // Repository-configured diff drivers and clean filters must not run for
    // these reads; the helper would replace the content with a sentinel.
    #[cfg(unix)]
    let helper_config = {
        use std::os::unix::fs::PermissionsExt as _;
        let helper = tmp.path().join("helper.sh");
        fs::write(&helper, "#!/bin/sh\necho HELPER-RAN\n")?;
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o755))?;
        let helper = helper.display().to_string();
        fs::create_dir_all(workspace.join(".git/info"))?;
        fs::write(
            workspace.join(".git/info/attributes"),
            "tracked.txt diff=conv filter=x\n",
        )?;
        let keys = ["diff.conv.textconv", "diff.external", "filter.x.clean"];
        for key in keys {
            git(&["config", key, &helper])?;
        }
        keys
    };

    // The diff reads serve the same worktree change: a unified patch for one
    // file, the whole tree with a numstat inventory, and the changes list.
    let file_diff: Value = client
        .get(format!("{base}/v1/diff?path=tracked.txt"))
        .bearer_auth("git-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(file_diff["ok"], true);
    assert_eq!(file_diff["untracked"], false);
    let patch = file_diff["diff"].as_str().unwrap();
    assert!(patch.contains("-v1") && patch.contains("+v2"), "{patch}");
    assert_eq!(file_diff["truncated"], false);

    // An untracked file is honest data: no diff, flagged for the client.
    fs::write(workspace.join("scratch.txt"), "loose\n")?;
    let untracked: Value = client
        .get(format!("{base}/v1/diff?path=scratch.txt"))
        .bearer_auth("git-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(untracked["ok"], true);
    assert_eq!(untracked["untracked"], true);
    assert_eq!(untracked["diff"], "");

    let changes: Value = client
        .get(format!("{base}/v1/changes"))
        .bearer_auth("git-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(changes["git_repo"], true);
    assert!(
        changes["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["path"] == "tracked.txt" && f["status"] == "modified")
    );

    let tree: Value = client
        .get(format!("{base}/v1/workspace/diff"))
        .bearer_auth("git-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(tree["ok"], true);
    assert!(
        tree["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["path"] == "tracked.txt" && f["added"] == 1 && f["deleted"] == 1)
    );
    assert!(tree["diff"].as_str().unwrap().contains("+v2"));
    assert_eq!(tree["truncated"], false);
    #[cfg(unix)]
    {
        for read in [&file_diff, &tree] {
            assert!(!read.to_string().contains("HELPER-RAN"), "{read}");
        }
        for key in helper_config {
            git(&["config", "--unset", key])?;
        }
        fs::remove_file(workspace.join(".git/info/attributes"))?;
    }

    // Diff path validation shares the file-route confinement.
    for (path, want) in [
        ("../escape.txt", StatusCode::BAD_REQUEST),
        (".git/config", StatusCode::FORBIDDEN),
    ] {
        let status = client
            .get(format!("{base}/v1/diff?path={path}"))
            .bearer_auth("git-token")
            .send()
            .await?
            .status();
        assert_eq!(status, want, "diff path {path}");
    }
    let discarded: Value = client
        .post(format!("{base}/v1/git/discard"))
        .bearer_auth("git-token")
        .json(&json!({ "paths": ["tracked.txt"] }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(discarded["ok"], true);
    assert_eq!(fs::read_to_string(workspace.join("tracked.txt"))?, "v1\n");
    let status = client
        .post(format!("{base}/v1/git/discard"))
        .bearer_auth("git-token")
        .json(&json!({ "paths": ["never-tracked.txt"] }))
        .send()
        .await?
        .status();
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Whole-tree discard stays refused; invalid refs and escapes are stopped
    // before git ever sees them (the .git refusal is forbidden, the rest are
    // bad requests).
    for (route, body, want) in [
        (
            "/v1/git/discard",
            json!({ "all": true }),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/v1/git/stage",
            json!({ "paths": ["../escape.txt"] }),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/v1/git/stage",
            json!({ "paths": [".git/config"] }),
            StatusCode::FORBIDDEN,
        ),
        (
            "/v1/git/branch",
            json!({ "name": "-f" }),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/v1/git/commit",
            json!({ "message": " " }),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let status = client
            .post(format!("{base}{route}"))
            .bearer_auth("git-token")
            .json(&body)
            .send()
            .await?
            .status();
        assert_eq!(status, want, "{route} {body}");
    }

    handle.abort();
    Ok(())
}

/// #6647: optional preconditions on git writes. A matching `expect` writes;
/// a stale HEAD, index or file answers 409 `git_state_changed` with the
/// current state and writes nothing; no `expect` keeps the old behaviour.
#[tokio::test]
async fn git_write_preconditions_reject_stale_state() -> Result<()> {
    use crate::dependencies::{ExternalTool as _, Git};

    let tmp = tempfile::tempdir()?;
    let workspace = tmp.path().join("workspace");
    fs::create_dir_all(&workspace)?;
    let git = |args: &[&str]| {
        let output = Git::output(args, &workspace)?;
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok::<_, anyhow::Error>(String::from_utf8_lossy(&output.stdout).into_owned())
    };
    git(&["init", "-b", "main"])?;
    git(&["config", "user.email", "runtime-api@example.test"])?;
    git(&["config", "user.name", "Runtime API Test"])?;
    git(&["config", "core.autocrlf", "false"])?;

    let (addr, _runtime_threads, handle) = spawn_test_server_with_root_token_mobile_workspace(
        tmp.path().join("runtime"),
        tmp.path().join("sessions"),
        Some("git-token".to_string()),
        false,
        workspace.clone(),
    )
    .await?
    .context("git test requires a loopback listener")?;
    let client = crate::tls::reqwest_client();
    let base = format!("http://{addr}");
    let read = || async {
        client
            .get(format!("{base}/v1/git"))
            .bearer_auth("git-token")
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await
            .map_err(anyhow::Error::from)
    };
    let post = |route: &'static str, body: Value| {
        let request = client
            .post(format!("{base}{route}"))
            .bearer_auth("git-token")
            .json(&body);
        async move {
            let response = request.send().await?;
            let status = response.status();
            // Axum's own extractor rejections (422) answer in plain text.
            let text = response.text().await?;
            let body = serde_json::from_str(&text).unwrap_or(Value::String(text));
            Ok::<_, anyhow::Error>((status, body))
        }
    };
    let rev_of = |detail: &Value, path: &str| -> String {
        detail["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["path"] == path)
            .and_then(|row| row["rev"].as_str())
            .unwrap_or_else(|| panic!("no rev for {path} in {detail}"))
            .to_string()
    };

    // (g) Unborn: head null matches a fresh repository, then goes stale.
    fs::write(workspace.join("tracked.txt"), "v1\n")?;
    let detail = read().await?;
    assert_eq!(detail["head_oid"], Value::Null);
    assert_eq!(detail["index_token"].as_str().unwrap().len(), 64);
    let (status, staged) = post(
        "/v1/git/stage",
        json!({ "paths": ["tracked.txt"], "expect": { "head": null, "files": { "tracked.txt": rev_of(&detail, "tracked.txt") } } }),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{staged}");
    let (status, body) = post(
        "/v1/git/commit",
        json!({ "message": "initial", "expect": { "head": null, "index": staged["current"]["index_token"] } }),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = post(
        "/v1/git/commit",
        json!({ "message": "again", "all": true, "expect": { "head": null } }),
    )
    .await?;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "git_state_changed");
    assert_eq!(body["stale"], json!(["head"]));

    // (a) The read exposes every token.
    fs::write(workspace.join("new.rs"), "fn main() {}\n")?;
    let detail = read().await?;
    let head = detail["head_oid"].as_str().unwrap().to_string();
    assert_eq!(head.len(), 40);
    assert!(head.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(detail["index_token"].as_str().unwrap().len(), 64);
    assert_eq!(detail["revision"].as_str().unwrap().len(), 64);
    for row in detail["files"].as_array().unwrap() {
        assert!(
            row["rev"].as_str().is_some_and(|rev| rev.len() == 66),
            "{row}"
        );
    }
    let changes: Value = client
        .get(format!("{base}/v1/changes"))
        .bearer_auth("git-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(changes["index_token"], detail["index_token"]);
    assert_eq!(changes["revision"], detail["revision"]);
    assert_eq!(changes["files"], detail["files"]);

    // (b) Matching preconditions: stage, then commit chained from `current`.
    let (status, staged) = post(
        "/v1/git/stage",
        json!({ "paths": ["new.rs"], "expect": { "head": head, "files": { "new.rs": rev_of(&detail, "new.rs") } } }),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{staged}");
    assert_eq!(staged["status"]["staged"], 1);
    assert_ne!(staged["current"]["index_token"], detail["index_token"]);
    let (status, committed) = post(
        "/v1/git/commit",
        json!({ "message": "add new.rs", "expect": { "head": head, "index": staged["current"]["index_token"] } }),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{committed}");
    assert_eq!(git(&["rev-parse", "HEAD~1"])?.trim(), head);
    let head = committed["current"]["head_oid"]
        .as_str()
        .unwrap()
        .to_string();

    // (c) A file rewritten after the read is not staged.
    fs::write(workspace.join("new2.rs"), "one\n")?;
    let detail = read().await?;
    fs::write(workspace.join("new2.rs"), "two\n")?;
    let index_before = git(&["ls-files", "--stage"])?;
    let (status, body) = post(
        "/v1/git/stage",
        json!({ "paths": ["new2.rs"], "expect": { "head": head, "files": { "new2.rs": rev_of(&detail, "new2.rs") } } }),
    )
    .await?;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "git_state_changed");
    assert_eq!(body["error"]["status"], 409);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("new2.rs changed"),
        "{body}"
    );
    assert_eq!(body["stale"], json!(["files"]));
    assert_eq!(body["stale_paths"], json!(["new2.rs"]));
    assert_ne!(
        rev_of(&body["current"], "new2.rs"),
        rev_of(&detail, "new2.rs")
    );
    assert_eq!(
        git(&["ls-files", "--stage"])?,
        index_before,
        "nothing staged"
    );

    // (d) Discard never destroys an edit made after the read.
    fs::write(workspace.join("tracked.txt"), "v2\n")?;
    let detail = read().await?;
    fs::write(workspace.join("tracked.txt"), "v2\nexternal edit\n")?;
    let (status, body) = post(
        "/v1/git/discard",
        json!({ "paths": ["tracked.txt"], "expect": { "head": head, "files": { "tracked.txt": rev_of(&detail, "tracked.txt") } } }),
    )
    .await?;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["stale_paths"], json!(["tracked.txt"]));
    assert_eq!(
        fs::read_to_string(workspace.join("tracked.txt"))?,
        "v2\nexternal edit\n"
    );
    // The refreshed token from `current` then discards exactly that state.
    let (status, body) = post(
        "/v1/git/discard",
        json!({ "paths": ["tracked.txt"], "expect": { "files": { "tracked.txt": rev_of(&body["current"], "tracked.txt") } } }),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(fs::read_to_string(workspace.join("tracked.txt"))?, "v1\n");

    // (e) Stale HEAD: an external commit lands between read and commit.
    let detail = read().await?;
    fs::write(workspace.join("other.txt"), "o\n")?;
    git(&["add", "other.txt"])?;
    git(&["commit", "-m", "external"])?;
    let external = git(&["rev-parse", "HEAD"])?.trim().to_string();
    fs::write(workspace.join("mine.txt"), "m\n")?;
    git(&["add", "mine.txt"])?;
    let (status, body) = post(
        "/v1/git/commit",
        json!({ "message": "mine", "expect": { "head": detail["head_oid"] } }),
    )
    .await?;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body["stale"].as_array().unwrap().contains(&json!("head")));
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("HEAD moved")
    );
    assert_eq!(git(&["rev-parse", "HEAD"])?.trim(), external);
    assert_eq!(body["current"]["head_oid"], external.as_str());

    // (f) Stale index: an external `git add` after the read.
    let detail = read().await?;
    let count_before = git(&["rev-list", "--count", "HEAD"])?;
    fs::write(workspace.join("sneak.txt"), "s\n")?;
    git(&["add", "sneak.txt"])?;
    let (status, body) = post(
        "/v1/git/commit",
        json!({ "message": "mine", "expect": { "head": detail["head_oid"], "index": detail["index_token"] } }),
    )
    .await?;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["stale"], json!(["index"]));
    assert_eq!(git(&["rev-list", "--count", "HEAD"])?, count_before);

    // Whole-tree revision guards stage-all against an unseen file.
    let detail = read().await?;
    fs::write(workspace.join("late.txt"), "late\n")?;
    let (status, body) = post(
        "/v1/git/stage",
        json!({ "all": true, "expect": { "revision": detail["revision"] } }),
    )
    .await?;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["stale"], json!(["revision"]));
    assert!(!git(&["ls-files"])?.contains("late.txt"));
    let (status, body) = post(
        "/v1/git/stage",
        json!({ "all": true, "expect": { "revision": body["current"]["revision"] } }),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(git(&["ls-files"])?.contains("late.txt"));

    // Literal pathspecs: `*` names a file called `*`, never a glob.
    fs::write(workspace.join("glob-a.txt"), "a\n")?;
    git(&["restore", "--staged", ":/"])?;
    let (status, body) = post("/v1/git/stage", json!({ "paths": ["glob-*"] })).await?;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(!git(&["diff", "--cached", "--name-only"])?.contains("glob-a.txt"));

    // (h) Absent preconditions, and an empty `expect`, keep working.
    let (status, body) = post("/v1/git/stage", json!({ "paths": ["glob-a.txt"] })).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = post(
        "/v1/git/unstage",
        json!({ "paths": ["glob-a.txt"], "expect": {} }),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");

    // (i) Malformed preconditions are refused before git runs.
    let detail = read().await?;
    for (route, body, want) in [
        (
            "/v1/git/commit",
            json!({ "message": "m", "expect": { "head": "abc" } }),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/v1/git/stage",
            json!({ "paths": ["glob-a.txt", "mine.txt"], "expect": { "files": { "glob-a.txt": rev_of(&detail, "glob-a.txt") } } }),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/v1/git/commit",
            json!({ "message": "m", "expect": { "files": { "glob-a.txt": rev_of(&detail, "glob-a.txt") } } }),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/v1/git/stage",
            json!({ "all": true, "expect": { "files": { "glob-a.txt": rev_of(&detail, "glob-a.txt") } } }),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/v1/git/stage",
            json!({ "paths": ["glob-a.txt"], "expect": { "unknown": 1 } }),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ] {
        let (status, response) = post(route, body.clone()).await?;
        assert_eq!(status, want, "{route} {body}: {response}");
        if status == StatusCode::BAD_REQUEST {
            assert_eq!(response["error"]["status"], 400);
            assert!(response["error"]["message"].is_string());
        }
    }
    assert!(!git(&["diff", "--cached", "--name-only"])?.contains("glob-a.txt"));

    handle.abort();
    Ok(())
}

/// The half of #6179 the credential route was missing: clear, the metadata a
/// client needs to decide whether to offer the control at all, and the refusal
/// for a credential Codewhale does not own.
#[tokio::test]
async fn provider_key_clear_round_trips_and_reports_writability() -> Result<()> {
    let _env_lock = crate::test_support::lock_test_env();
    let tmp = tempfile::tempdir()?;
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", tmp.path().join("cwhome"));
    let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    fs::create_dir_all(tmp.path().join("cwhome"))?;
    let workspace = tmp.path().join("workspace");
    fs::create_dir_all(&workspace)?;

    let (addr, _runtime_threads, handle) = spawn_test_server_with_root_token_mobile_workspace(
        tmp.path().join("runtime"),
        tmp.path().join("sessions"),
        Some("clear-token".to_string()),
        false,
        workspace.clone(),
    )
    .await?
    .context("secrets test requires a loopback listener")?;
    let client = crate::tls::reqwest_client();
    let base = format!("http://{addr}");

    // Clearing is a credential operation: it authenticates like the write.
    let status = client
        .delete(format!("{base}/v1/providers/openai/key"))
        .send()
        .await?
        .status();
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Same id validation as the write, so a client cannot discover providers
    // through one verb that the other rejects.
    for (id, want) in [
        ("not-a-provider", StatusCode::BAD_REQUEST),
        ("deepseek-cn", StatusCode::BAD_REQUEST),
    ] {
        let status = client
            .delete(format!("{base}/v1/providers/{id}/key"))
            .bearer_auth("clear-token")
            .send()
            .await?
            .status();
        assert_eq!(status, want, "{id}");
    }

    client
        .put(format!("{base}/v1/providers/openai/key"))
        .bearer_auth("clear-token")
        .json(&json!({ "key": "sk-key-that-will-be-cleared" }))
        .send()
        .await?
        .error_for_status()?;

    // Before the clear the catalog says the control is live and names the
    // source it would write — this is what lets a client enable the control.
    let providers: Value = client
        .get(format!("{base}/v1/providers"))
        .bearer_auth("clear-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let openai = providers["providers"]
        .as_array()
        .context("providers array")?
        .iter()
        .find(|entry| entry["id"] == "openai")
        .context("openai entry")?
        .clone();
    assert_eq!(openai["credentialState"], "configured");
    assert_eq!(openai["credentialSource"], "secret_store");
    assert_eq!(openai["credentialWritable"], true);
    // A writable route carries no refusal reason to render.
    assert!(openai["credentialWritableReason"].is_null());

    let receipt: Value = client
        .delete(format!("{base}/v1/providers/openai/key"))
        .bearer_auth("clear-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(receipt["provider"], "openai");
    assert_eq!(receipt["cleared"], true);
    // A clear receipt echoes no more than a write receipt does.
    assert!(!receipt.to_string().contains("sk-key-that-will-be-cleared"));
    assert_eq!(receipt["credentialState"], "missing");

    // And the catalog agrees on the next read, through the live config mirror
    // rather than only after a restart.
    let providers: Value = client
        .get(format!("{base}/v1/providers"))
        .bearer_auth("clear-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let openai = providers["providers"]
        .as_array()
        .context("providers array")?
        .iter()
        .find(|entry| entry["id"] == "openai")
        .context("openai entry")?
        .clone();
    assert_eq!(openai["credentialState"], "missing");
    // Still writable: the key is gone, the control is not.
    assert_eq!(openai["credentialWritable"], true);

    // Clearing an already-clear route is not an error — a client retrying a
    // revoke must not be told something went wrong.
    let receipt: Value = client
        .delete(format!("{base}/v1/providers/openai/key"))
        .bearer_auth("clear-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(receipt["cleared"], true);

    handle.abort();
    Ok(())
}

#[test]
fn agent_mail_cancel_conflict_maps_to_409() {
    let conflict = map_agent_mail_err(anyhow::anyhow!(
        "Agent Mail can be canceled only while queued (status: Delivered)"
    ));
    assert_eq!(conflict.status, StatusCode::CONFLICT);
}

/// #6176: Agent Mail is the durable "submit while running" queue (nothing
/// ever produces a `Queued` turn record — POST /turns rejects a busy
/// thread). Withdrawing a queued envelope is idempotent, delivery after
/// cancel starts no turn, and mail that already left `queued` is an
/// explicit conflict rather than a silent drop.
#[tokio::test]
async fn agent_mail_cancel_withdraws_queued_mail() -> Result<()> {
    let Some((addr, _runtime_threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let base = format!("http://{addr}");

    // Agent Mail addressability needs task-bound threads, and the sender
    // identity must be the source thread's task id.
    let new_thread = |task_id: &str| {
        let client = &client;
        let base = base.clone();
        let task_id = task_id.to_string();
        async move {
            let thread: Value = client
                .post(format!("{base}/v1/threads"))
                .json(&json!({ "task_id": task_id }))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            anyhow::Ok(thread["id"].as_str().expect("thread id").to_string())
        }
    };
    let source = new_thread("task_a").await?;
    let destination = new_thread("task_b").await?;

    let send = |message_id: &str| {
        let client = &client;
        let base = base.clone();
        let source = source.clone();
        let destination = destination.clone();
        let message_id = message_id.to_string();
        async move {
            let sent = client
                .post(format!("{base}/v1/agent-mail"))
                .json(&json!({
                    "message_id": message_id,
                    "source_thread_id": source,
                    "destination_thread_id": destination,
                    "sender": {"identity": "task_a", "display_label": "Test Sender"},
                    "summary": "handoff: review the queued work",
                    "delivery_mode": "queue_only",
                    "trigger_turn": false,
                }))
                .send()
                .await?;
            anyhow::Ok(sent)
        }
    };

    let sent = send("mail_queue_1").await?;
    assert_eq!(sent.status(), StatusCode::CREATED);
    let body: Value = sent.json().await?;
    assert_eq!(body["envelope"]["status"], "queued");

    let inbox: Value = client
        .get(format!("{base}/v1/threads/{destination}/agent-mail"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert!(inbox.as_array().is_some_and(|mail| {
        mail.iter()
            .any(|item| item["message_id"] == "mail_queue_1" && item["status"] == "queued")
    }));

    let canceled: Value = client
        .post(format!(
            "{base}/v1/threads/{destination}/agent-mail/mail_queue_1/cancel"
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(canceled["status"], "canceled");

    // Re-cancel is an idempotent no-op returning the stored envelope.
    let recanceled: Value = client
        .post(format!(
            "{base}/v1/threads/{destination}/agent-mail/mail_queue_1/cancel"
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(recanceled, canceled);

    // Delivery after cancel starts no turn.
    let delivered: Value = client
        .post(format!(
            "{base}/v1/threads/{destination}/agent-mail/mail_queue_1/deliver"
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(delivered["envelope"]["status"], "canceled");
    assert!(delivered.get("turn").is_none());

    // Mail that already left `queued` is an explicit conflict.
    let sent = send("mail_queue_2").await?;
    assert_eq!(sent.status(), StatusCode::CREATED);
    let delivered: Value = client
        .post(format!(
            "{base}/v1/threads/{destination}/agent-mail/mail_queue_2/deliver"
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(delivered["envelope"]["status"], "delivered");
    let conflict = client
        .post(format!(
            "{base}/v1/threads/{destination}/agent-mail/mail_queue_2/cancel"
        ))
        .send()
        .await?;
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    assert!(
        conflict
            .text()
            .await?
            .contains("can be canceled only while queued")
    );

    // A message addressed elsewhere is a foreign-destination rejection.
    let foreign = client
        .post(format!(
            "{base}/v1/threads/{source}/agent-mail/mail_queue_1/cancel"
        ))
        .send()
        .await?;
    assert_eq!(foreign.status(), StatusCode::FORBIDDEN);

    // An unknown message id is a 404, never a silent success.
    let missing = client
        .post(format!(
            "{base}/v1/threads/{destination}/agent-mail/mail_nope/cancel"
        ))
        .send()
        .await?;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    handle.abort();
    Ok(())
}

/// Running-work accounting (#6180): a watch-only client enumerates owned
/// in-flight work in one call, with turn identity for targeting, and the
/// listing clears when the work settles.
#[tokio::test]
async fn threads_running_lists_active_turns_and_clears_on_settle() -> Result<()> {
    let Some((addr, manager, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let base = format!("http://{addr}");

    let idle: serde_json::Value = client
        .get(format!("{base}/v1/threads/running"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(idle, serde_json::json!([]));

    let created: serde_json::Value = client
        .post(format!("{base}/v1/threads"))
        .json(&serde_json::json!({ "model": "test-model" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let thread_id = created["id"].as_str().context("missing thread id")?;

    client
        .post(format!("{base}/v1/threads/{thread_id}/turns"))
        .json(&serde_json::json!({ "prompt": "do the thing" }))
        .send()
        .await?
        .error_for_status()?;

    // Pin the turn in-flight deterministically: a test server may settle a
    // started turn on its own, which would make the listing below racy.
    let mut started = manager
        .test_store()
        .list_turns_for_thread(thread_id)?
        .pop()
        .context("missing started turn")?;
    started.status = crate::runtime_threads::RuntimeTurnStatus::InProgress;
    manager.test_store().save_turn(&started)?;

    let running: serde_json::Value = client
        .get(format!("{base}/v1/threads/running"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(running.as_array().map(Vec::len), Some(1));
    assert_eq!(running[0]["thread_id"], thread_id);
    let active = running[0]["active_turns"]
        .as_array()
        .context("missing active_turns")?;
    assert_eq!(active.len(), 1);
    assert_eq!(active[0]["status"], "in_progress");
    let turn_id = active[0]["turn_id"].as_str().context("missing turn id")?;

    // Stop the live runner before the store-side settle. Mid-turn monitor
    // projection used to load an in-flight snapshot and save_turn it back over
    // a concurrent Completed write; save_turn now serializes every turn write
    // (reentrant turn_mutation), late item attaches skip terminal turns, and
    // interrupting drops the engine so no further projection races the settle.
    // Prefer the runner's own terminalization when it lands quickly; otherwise
    // pin Completed the same way the InProgress pin above is authoritative.
    let _ = manager.interrupt_turn(thread_id, turn_id).await;
    let settle_deadline = std::time::Instant::now() + Duration::from_millis(500);
    loop {
        let turn = manager.test_store().load_turn(turn_id)?;
        if !turn.status.is_active_work() {
            break;
        }
        if std::time::Instant::now() >= settle_deadline {
            let mut turn = turn;
            turn.status = crate::runtime_threads::RuntimeTurnStatus::Completed;
            manager.test_store().save_turn(&turn)?;
            break;
        }
        sleep(Duration::from_millis(20)).await;
    }

    // The listing above is read from the durable store, and the engine still
    // owns this record: it can persist its own status after the write above,
    // which puts the turn back in flight and made a single read flaky on a
    // loaded macOS runner. That is not a defect — the engine is entitled to
    // finish its turn. What must hold is that a settled turn stops being
    // listed, so poll for that instead of assuming the first read is final.
    let deadline = std::time::Instant::now() + ci_scaled(Duration::from_secs(5));
    loop {
        let settled: serde_json::Value = client
            .get(format!("{base}/v1/threads/running"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if settled == serde_json::json!([]) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "a settled turn must leave the running list: {settled}"
        );
        sleep(Duration::from_millis(50)).await;
    }

    handle.abort();
    Ok(())
}

/// Notice serving (#6180): GET lists raised notices with thread/turn
/// identity, DELETE acks one, unknown notices and threads 404.
#[tokio::test]
async fn thread_notices_serve_list_and_ack() -> Result<()> {
    let Some((addr, manager, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();
    let base = format!("http://{addr}");

    let created: serde_json::Value = client
        .post(format!("{base}/v1/threads"))
        .json(&serde_json::json!({ "model": "test-model" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let thread_id = created["id"].as_str().context("missing thread id")?;

    let empty: serde_json::Value = client
        .get(format!("{base}/v1/threads/{thread_id}/notices"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(empty, serde_json::json!([]));

    let notice_id = manager.raise_notice(
        thread_id,
        "model-notify",
        "turn_1",
        "tool_1",
        "come back".to_string(),
    );
    let listed: serde_json::Value = client
        .get(format!("{base}/v1/threads/{thread_id}/notices"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(listed.as_array().map(Vec::len), Some(1));
    assert_eq!(listed[0]["id"], notice_id);
    assert_eq!(listed[0]["kind"], "model-notify");
    assert_eq!(listed[0]["turn_id"], "turn_1");
    assert_eq!(listed[0]["subject"], "tool_1");

    let ack = client
        .delete(format!("{base}/v1/threads/{thread_id}/notices/{notice_id}"))
        .send()
        .await?;
    assert_eq!(ack.status(), reqwest::StatusCode::NO_CONTENT);

    let cleared: serde_json::Value = client
        .get(format!("{base}/v1/threads/{thread_id}/notices"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(cleared, serde_json::json!([]));

    let again = client
        .delete(format!("{base}/v1/threads/{thread_id}/notices/{notice_id}"))
        .send()
        .await?;
    assert_eq!(again.status(), reqwest::StatusCode::NOT_FOUND);

    let unknown = client
        .get(format!("{base}/v1/threads/nope/notices"))
        .send()
        .await?;
    assert_eq!(unknown.status(), reqwest::StatusCode::NOT_FOUND);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn provider_models_refresh_uses_exact_configured_route_without_switching() -> Result<()> {
    let _env = crate::test_support::lock_test_env();
    let root = tempfile::tempdir()?;
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path());
    let _cli = crate::test_support::EnvVarGuard::remove(codewhale_config::CLI_API_KEY_ENV);
    crate::provider_catalog_live::reset_cache_for_test();
    let upstream = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/v1/models"))
        .and(wiremock::matchers::header(
            "Authorization",
            "Bearer refresh-fixture-key",
        ))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .set_body_json(json!({"data":[{"id":"discovered-fixture-model"}]})),
        )
        .expect(1)
        .mount(&upstream)
        .await;
    let config_path = root.path().join("config.toml");
    fs::write(
        &config_path,
        format!(
            r#"provider = "first"
[providers.first]
kind = "openai-compatible"
base_url = "http://127.0.0.1:9/v1"
model = "saved-model"
[providers.second]
kind = "openai-compatible"
base_url = "{}/v1"
api_key = "refresh-fixture-key"
"#,
            upstream.uri()
        ),
    )?;
    let (addr, _, handle) = spawn_test_server_with_config_path(config_path)
        .await?
        .expect("loopback server required");
    let client = crate::tls::reqwest_client_builder()
        .timeout(Duration::from_secs(5))
        .build()?;
    for suffix in [
        "custom/models/refresh?model_provider_id=",
        "custom/models/refresh?model_provider_id=missing",
        "openai/models/refresh?model_provider_id=second",
        "deepseek-cn-unknown/models/refresh",
        "custom/models/refresh?base_url=https://untrusted.invalid",
    ] {
        assert_eq!(
            client
                .post(format!("http://{addr}/v1/providers/{suffix}"))
                .send()
                .await?
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    let response = client
        .post(format!(
            "http://{addr}/v1/providers/custom/models/refresh?model_provider_id=second"
        ))
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let receipt: Value = response.json().await?;
    assert_eq!(receipt["outcome"], "updated");
    assert_eq!(receipt["model_count"], 1);
    assert!(!receipt.to_string().contains("refresh-fixture-key"));
    let catalog: Value = client
        .get(format!(
            "http://{addr}/v1/providers/custom/models?model_provider_id=second"
        ))
        .send()
        .await?
        .json()
        .await?;
    assert!(
        catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["id"] == "discovered-fixture-model")
    );
    assert_eq!(get_config(&client, &addr).await["provider"], "first");
    handle.abort();
    Ok(())
}

#[tokio::test]
async fn mcp_management_selected_connection_and_retry_preserve_siblings() -> Result<()> {
    use axum::response::IntoResponse as _;
    use std::sync::atomic::{AtomicUsize, Ordering};
    async fn fixture(
        State(count): State<Arc<AtomicUsize>>,
        Json(request): Json<Value>,
    ) -> axum::response::Response {
        let Some(id) = request.get("id") else {
            return StatusCode::NO_CONTENT.into_response();
        };
        let result = match request["method"].as_str().unwrap_or_default() {
            "initialize" => {
                count.fetch_add(1, Ordering::SeqCst);
                json!({"protocolVersion":request["params"]["protocolVersion"],"capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}})
            }
            "tools/list" => {
                json!({"tools":[{"name":"fixture_tool","inputSchema":{"type":"object"}}]})
            }
            _ => json!({}),
        };
        Json(json!({"jsonrpc":"2.0","id":id,"result":result})).into_response()
    }
    let one = Arc::new(AtomicUsize::new(0));
    let two = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let fixture_addr = listener.local_addr()?;
    let app = axum::Router::new()
        .route("/one", axum::routing::post(fixture).with_state(one.clone()))
        .route("/two", axum::routing::post(fixture).with_state(two.clone()));
    let fixture_task = tokio::spawn(async move { axum::serve(listener, app).await });
    let root = tempfile::tempdir()?;
    fs::write(
        root.path().join("mcp.json"),
        json!({"servers":{
            "one":{"url":format!("http://{fixture_addr}/one"),"transport":"streamable_http"},
            "two":{"url":format!("http://{fixture_addr}/two"),"transport":"streamable_http"},
            "broken":{"command":root.path().join("missing-executable"),"connect_timeout":1}
        }})
        .to_string(),
    )?;
    let (addr, _, handle) =
        spawn_test_server_with_root(root.path().to_owned(), root.path().join("sessions"))
            .await?
            .context("MCP management test requires loopback")?;
    let client = crate::tls::reqwest_client();
    let base = format!("http://{addr}/v1/apps/mcp");
    let failed: Value = client
        .get(format!("{base}/tools?server=broken&connect=true"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(failed["connections"][0]["server"], "broken");
    assert_eq!(failed["connections"][0]["connected"], false);
    assert!(failed["connections"][0]["error"].is_string());
    assert_eq!(one.load(Ordering::SeqCst), 0);
    assert_eq!(two.load(Ordering::SeqCst), 0);
    let selected: Value = client
        .get(format!("{base}/tools?server=one&connect=true"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(selected["connections"][0]["connected"], true, "{selected}");
    assert_eq!(one.load(Ordering::SeqCst), 1);
    assert_eq!(
        two.load(Ordering::SeqCst),
        0,
        "testing one server must not connect its sibling"
    );
    let second: Value = client
        .get(format!("{base}/tools?server=two&connect=true"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(second["connections"][0]["connected"], true, "{second}");
    let retry: Value = client
        .post(format!("{base}/servers/one/reconnect"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(retry["ok"], true, "{retry}");
    assert_eq!(one.load(Ordering::SeqCst), 2);
    assert_eq!(two.load(Ordering::SeqCst), 1);
    let listing: Value = client
        .get(format!("{base}/servers"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    for name in ["one", "two"] {
        let row = listing["servers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["name"] == name)
            .unwrap();
        assert_eq!(row["connected"], true, "{listing}");
        assert_eq!(row["origin"], "global");
        assert_eq!(row["writable"], true);
    }
    let mut changed: Value =
        serde_json::from_str(&fs::read_to_string(root.path().join("mcp.json"))?)?;
    changed["servers"]["one"]["enabled"] = json!(false);
    fs::write(root.path().join("mcp.json"), changed.to_string())?;
    let disabled: Value = client
        .post(format!("{base}/servers/one/reconnect"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(disabled["ok"], false);
    assert!(
        disabled["connection"]["error"]
            .as_str()
            .unwrap()
            .contains("disabled")
    );
    assert_eq!(
        one.load(Ordering::SeqCst),
        2,
        "disabled retry must not launch the old cached config"
    );
    changed["servers"]["one"]["enabled"] = json!(true);
    changed["servers"]["one"]["url"] = json!(format!("http://{fixture_addr}/two"));
    fs::write(root.path().join("mcp.json"), changed.to_string())?;
    let stale: Value = client
        .post(format!("{base}/servers/one/reconnect"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(stale["ok"], false);
    assert!(
        stale["connection"]["error"]
            .as_str()
            .unwrap()
            .contains("configuration changed")
    );
    assert_eq!(one.load(Ordering::SeqCst), 2);
    assert_eq!(two.load(Ordering::SeqCst), 1);
    let stale_connect: Value = client
        .get(format!("{base}/tools?server=one&connect=true"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert!(
        stale_connect["connections"][0]["error"]
            .as_str()
            .unwrap()
            .contains("configuration changed")
    );
    assert_eq!(stale_connect["tools"], json!([]));
    fs::write(root.path().join("mcp.json"), "{malformed")?;
    let malformed = client
        .get(format!("{base}/tools?server=one&connect=true"))
        .send()
        .await?;
    assert_eq!(malformed.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        one.load(Ordering::SeqCst),
        2,
        "malformed sources must not connect cached endpoints"
    );
    assert_eq!(two.load(Ordering::SeqCst), 1);
    handle.abort();
    fixture_task.abort();
    Ok(())
}

#[tokio::test]
async fn mcp_management_project_ownership_blocks_global_shadow_mutations() -> Result<()> {
    let _env = crate::test_support::lock_test_env();
    let root = tempfile::tempdir()?;
    let workspace = root.path().join("workspace");
    fs::create_dir_all(workspace.join(".codewhale"))?;
    let trust_path = root.path().join("trust.toml");
    let trusted = workspace.canonicalize()?;
    let mut projects = toml::map::Map::new();
    projects.insert(
        trusted.display().to_string(),
        toml::Value::try_from(json!({"trust_level":"trusted"}))?,
    );
    fs::write(
        &trust_path,
        toml::to_string(&toml::Value::Table(toml::map::Map::from_iter([(
            "projects".to_owned(),
            toml::Value::Table(projects),
        )])))?,
    )?;
    let _path = crate::test_support::EnvVarGuard::set("CODEWHALE_CONFIG_PATH", &trust_path);
    let _legacy = crate::test_support::EnvVarGuard::remove("DEEPSEEK_CONFIG_PATH");
    let global = json!({"servers":{"shared":{"command":"global-command"},"global":{"command":"global-command"}}}).to_string();
    let project = json!({"servers":{"shared":{"command":root.path().join("missing-project-executable")},"project":{"command":root.path().join("missing-project-executable")}}}).to_string();
    fs::write(root.path().join("mcp.json"), &global)?;
    fs::write(workspace.join(".codewhale/mcp.json"), &project)?;
    let (addr, _, handle) = spawn_test_server_with_root_token_mobile_workspace(
        root.path().to_owned(),
        root.path().join("sessions"),
        None,
        false,
        workspace.clone(),
    )
    .await?
    .context("MCP ownership test requires loopback")?;
    let client = crate::tls::reqwest_client();
    let base = format!("http://{addr}/v1/apps/mcp/servers");
    let listing: Value = client
        .get(&base)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    for name in ["shared", "project"] {
        let row = listing["servers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["name"] == name)
            .unwrap();
        assert_eq!(row["origin"], "project", "{listing}");
        assert_eq!(row["writable"], false);
        let detail: Value = client
            .get(format!("{base}/{name}"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_eq!(detail["origin"], "project");
        assert_eq!(detail["writable"], false);
        assert_eq!(
            client
                .patch(format!("{base}/{name}"))
                .header(header::IF_MATCH, mcp_test_revision(&client, &base).await?)
                .json(&json!({"enabled":false}))
                .send()
                .await?
                .status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            client
                .delete(format!("{base}/{name}"))
                .header(header::IF_MATCH, mcp_test_revision(&client, &base).await?)
                .send()
                .await?
                .status(),
            StatusCode::CONFLICT
        );
        for action in ["enable", "disable"] {
            assert_eq!(
                client
                    .post(format!("{base}/{name}/{action}"))
                    .header(header::IF_MATCH, mcp_test_revision(&client, &base).await?)
                    .send()
                    .await?
                    .status(),
                StatusCode::CONFLICT
            );
        }
        assert_eq!(
            client
                .post(&base)
                .header(header::IF_MATCH, mcp_test_revision(&client, &base).await?)
                .json(&json!({"name":name,"command":"override"}))
                .send()
                .await?
                .status(),
            StatusCode::CONFLICT
        );
    }
    assert_eq!(fs::read_to_string(root.path().join("mcp.json"))?, global);
    assert_eq!(
        fs::read_to_string(workspace.join(".codewhale/mcp.json"))?,
        project
    );
    // Initialize the pool under project authority, without a real executable.
    let trusted_connect: Value = client
        .get(format!(
            "http://{addr}/v1/apps/mcp/tools?server=project&connect=true"
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert!(trusted_connect["connections"][0]["error"].is_string());
    // Removing workspace trust removes project ownership rather than
    // labelling an ignored repository file as writable effective state.
    fs::write(&trust_path, "")?;
    let detail: Value = client
        .get(format!("{base}/shared"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(detail["origin"], "global");
    assert_eq!(detail["writable"], true);
    let revoked_connect: Value = client
        .get(format!(
            "http://{addr}/v1/apps/mcp/tools?server=project&connect=true"
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert!(
        revoked_connect["connections"][0]["error"]
            .as_str()
            .unwrap()
            .contains("configuration changed")
    );
    assert_eq!(revoked_connect["tools"], json!([]));
    handle.abort();
    Ok(())
}

#[tokio::test]
async fn mcp_server_management_blocks_credential_retargeting() -> Result<()> {
    let root = tempfile::tempdir()?;
    let mut servers = serde_json::Map::new();
    for (name, field, value) in [
        ("env", "env", json!({"API_KEY":"fixture-private-value"})),
        (
            "headers",
            "headers",
            json!({"Authorization":"fixture-private-value"}),
        ),
        (
            "env_headers",
            "env_headers",
            json!({"Authorization":"FIXTURE_TOKEN"}),
        ),
        ("bearer", "bearer_token_env_var", json!("FIXTURE_TOKEN")),
        (
            "oauth",
            "oauth",
            json!({"client_id":"fixture-private-value"}),
        ),
        ("scopes", "scopes", json!(["tools"])),
        (
            "resource",
            "oauth_resource",
            json!("https://original.invalid/resource"),
        ),
    ] {
        let mut server = json!({"command":"original-command","args":["original"],"url":"https://original.invalid/mcp","transport":"sse"});
        server[field] = value;
        servers.insert(name.to_owned(), server);
    }
    servers.insert("plain".to_owned(), json!({"command":"original-command"}));
    let path = root.path().join("mcp.json");
    fs::write(&path, json!({"servers":servers}).to_string())?;
    let (addr, _, handle) =
        spawn_test_server_with_root(root.path().to_owned(), root.path().join("sessions"))
            .await?
            .context("MCP credential guard test requires loopback")?;
    let client = crate::tls::reqwest_client();
    let base = format!("http://{addr}/v1/apps/mcp/servers");
    for name in [
        "env",
        "headers",
        "env_headers",
        "bearer",
        "oauth",
        "scopes",
        "resource",
    ] {
        let detail: Value = client
            .get(format!("{base}/{name}"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_eq!(detail["credential_configured"], true, "{name}");
        assert!(!detail.to_string().contains("fixture-private-value"));
        for private_field in [
            "env",
            "headers",
            "env_headers",
            "bearer_token_env_var",
            "oauth",
        ] {
            assert!(
                detail.get(private_field).is_none(),
                "{name}: {private_field}"
            );
        }
        client
            .patch(format!("{base}/{name}"))
            .header(header::IF_MATCH, mcp_test_revision(&client, &base).await?)
            .json(&json!({"connect_timeout":17}))
            .send()
            .await?
            .error_for_status()?;
        let before = fs::read(&path)?;
        for patch in [
            json!({"command":"replacement-command"}),
            json!({"args":["replacement"]}),
            json!({"url":"https://other.invalid/mcp"}),
            json!({"transport":null}),
        ] {
            let response = client
                .patch(format!("{base}/{name}"))
                .header(header::IF_MATCH, mcp_test_revision(&client, &base).await?)
                .json(&patch)
                .send()
                .await?;
            assert_eq!(response.status(), StatusCode::CONFLICT, "{name}: {patch}");
            assert_eq!(
                fs::read(&path)?,
                before,
                "rejected retarget must not change config"
            );
        }
    }
    let plain: Value = client
        .patch(format!("{base}/plain"))
        .header(header::IF_MATCH, mcp_test_revision(&client, &base).await?)
        .json(&json!({"command":"replacement-command"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(plain["credential_configured"], false);
    let cleared: Value = client
        .patch(format!("{base}/env"))
        .header(header::IF_MATCH, mcp_test_revision(&client, &base).await?)
        .json(&json!({"env":{},"command":"replacement-command"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(cleared["credential_configured"], false);
    assert_eq!(cleared["command"], "replacement-command");
    handle.abort();
    Ok(())
}

/// Like `error_for_status`, but keeps the server's `ApiError` body so a hosted
/// failure names the refusing handler instead of only its status.
async fn mcp_test_success(response: reqwest::Response) -> Result<reqwest::Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let url = response.url().clone();
    let body = response.text().await.unwrap_or_default();
    bail!("{status} for {url}: {body}")
}

async fn mcp_test_revision(client: &reqwest::Client, base: &str) -> Result<String> {
    let listing: Value = mcp_test_success(client.get(base).send().await?)
        .await?
        .json()
        .await?;
    Ok(format!(
        "\"{}\"",
        listing["revision"]
            .as_str()
            .context("MCP revision missing")?
    ))
}

#[tokio::test]
async fn mcp_management_revision_precondition_prevents_stale_mutation() -> Result<()> {
    let root = tempfile::tempdir()?;
    let (addr, _, handle) =
        spawn_test_server_with_root(root.path().to_owned(), root.path().join("sessions"))
            .await?
            .context("loopback required")?;
    let client = crate::tls::reqwest_client();
    let base = format!("http://{addr}/v1/apps/mcp/servers");
    let absent = mcp_test_revision(&client, &base).await?;
    assert_eq!(absent, "\"mcp-v1-absent\"");
    assert_eq!(
        client
            .post(&base)
            .json(&json!({"name":"one","command":"one"}))
            .send()
            .await?
            .status(),
        StatusCode::PRECONDITION_REQUIRED
    );
    let created: Value = client
        .post(&base)
        .header(header::IF_MATCH, &absent)
        .json(&json!({"name":"one","command":"one"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert!(created["revision"].as_str().unwrap().starts_with("mcp-v1-"));
    let path = root.path().join("mcp.json");
    let before = fs::read(&path)?;
    for request in [
        client
            .post(&base)
            .json(&json!({"name":"two","command":"two"})),
        client
            .patch(format!("{base}/one"))
            .json(&json!({"command":"changed"})),
        client.delete(format!("{base}/one")),
        client.post(format!("{base}/one/enable")),
        client.post(format!("{base}/one/disable")),
    ] {
        assert_eq!(
            request
                .header(header::IF_MATCH, &absent)
                .send()
                .await?
                .status(),
            StatusCode::PRECONDITION_FAILED
        );
        assert_eq!(fs::read(&path)?, before);
    }
    assert_eq!(
        client
            .delete(format!("{base}/one"))
            .header(header::IF_MATCH, "*")
            .send()
            .await?
            .status(),
        StatusCode::BAD_REQUEST
    );
    let current = mcp_test_revision(&client, &base).await?;
    let disabled: Value = client
        .post(format!("{base}/one/disable"))
        .header(header::IF_MATCH, &current)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_ne!(disabled["revision"], created["revision"]);
    handle.abort();
    Ok(())
}

#[cfg(all(unix, not(target_env = "ohos")))]
#[tokio::test]
async fn jobs_api_pty_resize_raw_input_and_nonblocking_poll() -> Result<()> {
    use base64::Engine as _;
    let tmp = tempfile::tempdir()?;
    let workspace = tmp.path().join("workspace");
    fs::create_dir_all(&workspace)?;
    let (addr, _, handle) = spawn_test_server_with_root_token_mobile_workspace(
        tmp.path().join("runtime"),
        tmp.path().join("sessions"),
        Some("pty-token".into()),
        false,
        workspace.clone(),
    )
    .await?
    .context("PTY test requires loopback listener")?;
    let client = crate::tls::reqwest_client();
    let base = format!("http://{addr}");
    let thread: Value = client
        .post(format!("{base}/v1/threads"))
        .bearer_auth("pty-token")
        .json(&json!({"workspace":workspace,"allow_shell":true}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let thread_id = thread["id"].as_str().unwrap();
    let jobs = format!("{base}/v1/threads/{thread_id}/jobs");
    let created: Value = client.post(&jobs).bearer_auth("pty-token")
        .json(&json!({"command":"stty -echo; printf ready; while IFS= read -r line; do stty size; done", "tty":true}))
        .send().await?.error_for_status()?.json().await?;
    assert_eq!(created["job"]["tty"], true);
    assert_eq!(
        created["job"]["terminal_size"],
        json!({"rows":24,"cols":80})
    );
    let job_id = created["job"]["job_id"].as_str().unwrap();
    let job = format!("{jobs}/{job_id}");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let output: Value = client
            .get(format!("{job}/output?format=text"))
            .bearer_auth("pty-token")
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if output["data"].as_str().unwrap().contains("ready") {
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        client
            .post(format!("{job}/resize"))
            .json(&json!({"rows":30,"cols":90}))
            .send()
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );
    for size in [json!({"rows":0,"cols":90}), json!({"rows":30,"cols":1001})] {
        assert_eq!(
            client
                .post(format!("{job}/resize"))
                .bearer_auth("pty-token")
                .json(&size)
                .send()
                .await?
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    let resized: Value = client
        .post(format!("{job}/resize"))
        .bearer_auth("pty-token")
        .json(&json!({"rows":42,"cols":123}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        resized["job"]["terminal_size"],
        json!({"rows":42,"cols":123})
    );
    client
        .post(format!("{job}/stdin"))
        .bearer_auth("pty-token")
        .json(&json!({"data":"size\n"}))
        .send()
        .await?
        .error_for_status()?;
    loop {
        let output: Value = client
            .get(format!("{job}/output?format=text"))
            .bearer_auth("pty-token")
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if output["data"].as_str().unwrap().contains("42 123") {
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        sleep(Duration::from_millis(20)).await;
    }
    let other: Value = client
        .post(format!("{base}/v1/threads"))
        .bearer_auth("pty-token")
        .json(&json!({"workspace":workspace,"allow_shell":true}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        client
            .post(format!(
                "{base}/v1/threads/{}/jobs/{job_id}/resize",
                other["id"].as_str().unwrap()
            ))
            .bearer_auth("pty-token")
            .json(&json!({"rows":30,"cols":90}))
            .send()
            .await?
            .status(),
        StatusCode::NOT_FOUND
    );
    client
        .post(format!("{job}/kill"))
        .bearer_auth("pty-token")
        .send()
        .await?
        .error_for_status()?;
    assert_eq!(
        client
            .post(format!("{job}/resize"))
            .bearer_auth("pty-token")
            .json(&json!({"rows":30,"cols":90}))
            .send()
            .await?
            .status(),
        StatusCode::CONFLICT
    );

    let cat: Value = client
        .post(&jobs)
        .bearer_auth("pty-token")
        .json(&json!({"command":"cat"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(cat["job"]["tty"], false);
    assert!(cat["job"]["terminal_size"].is_null());
    let cat_job = format!("{jobs}/{}", cat["job"]["job_id"].as_str().unwrap());
    assert_eq!(
        client
            .post(format!("{cat_job}/resize"))
            .bearer_auth("pty-token")
            .json(&json!({"rows":30,"cols":90}))
            .send()
            .await?
            .status(),
        StatusCode::BAD_REQUEST
    );
    // A 5-second output long-poll must not hold the owner lock against stdin.
    let poll_client = client.clone();
    let poll_url = format!("{cat_job}/output?wait_ms=5000");
    let polling = tokio::spawn(async move {
        poll_client
            .get(poll_url)
            .bearer_auth("pty-token")
            .send()
            .await
    });
    sleep(Duration::from_millis(100)).await;
    let bytes = b"\0\xff\x1b[A\x03\n";
    tokio::time::timeout(Duration::from_secs(2), client.post(format!("{cat_job}/stdin"))
        .bearer_auth("pty-token").json(&json!({"data":base64::engine::general_purpose::STANDARD.encode(bytes), "encoding":"base64", "close":true})).send()).await??.error_for_status()?;
    let _ = polling.await??.error_for_status()?;
    let output: Value = client
        .get(format!("{cat_job}/output?wait_ms=1000"))
        .bearer_auth("pty-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        base64::engine::general_purpose::STANDARD.decode(output["data"].as_str().unwrap())?,
        bytes
    );
    let replay: Value = client
        .get(format!("{cat_job}/output"))
        .bearer_auth("pty-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(replay["data"], output["data"]);
    let end = output["next_cursor"].as_u64().unwrap();
    let tail: Value = client
        .get(format!("{cat_job}/output?cursor={end}"))
        .bearer_auth("pty-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(tail["data"], "");
    handle.abort();
    Ok(())
}

#[test]
fn the_mobile_page_follows_agents_by_name() {
    // #6565 / #6474: the mobile page never subscribed to agent events.
    let html = super::MOBILE_HTML;
    for event in [
        "\"agent.spawned\"",
        "\"agent.progress\"",
        "\"agent.completed\"",
    ] {
        assert!(html.contains(event), "subscribes to {event}");
    }
    assert!(html.contains("id=\"agents\""), "has an agents strip");
    assert!(
        html.contains("payload.agent_name"),
        "names agents by their name"
    );
    assert!(
        html.contains("if (name === \"agent.completed\")"),
        "eventText renders completion"
    );
    assert!(
        html.contains("state.agents.clear()"),
        "a new thread starts a new strip"
    );
    assert!(
        !html.contains("\"Sub-agent \" + payload.agent_id"),
        "never shows the raw id"
    );
}

/// #6621: a Runtime thread owns the workspace restore points recorded on its
/// own turns, for every kind of thread, through the HTTP API and with real
/// engines built by `ensure_engine_loaded` (only the model client is
/// scripted), so the production `EngineConfig` / `SyncSession` identity path
/// is the one under test.
mod thread_snapshot_ownership {
    use super::*;
    use crate::llm_client::mock::{MockLlmClient, canned};

    struct Fixture {
        root: PathBuf,
        workspace: PathBuf,
        addr: SocketAddr,
        threads: SharedRuntimeThreadManager,
        server: tokio::task::JoinHandle<()>,
        mock: Arc<MockLlmClient>,
        client: reqwest::Client,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.server.abort();
        }
    }

    /// `None` when git or a loopback listener is unavailable.
    async fn fixture(root: &FsPath, overrides: TestServerOverrides) -> Result<Option<Fixture>> {
        if crate::dependencies::Git::command().is_none() {
            return Ok(None);
        }
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace)?;
        // The directory the Runtime's fork and resume paths read saved
        // sessions from, as production wires it.
        let sessions_dir = crate::session_manager::default_sessions_dir()?;
        let Some((addr, threads, server)) =
            spawn_test_server_with_root_token_mobile_workspace_and_overrides(
                root.to_path_buf(),
                sessions_dir,
                None,
                false,
                workspace.clone(),
                overrides,
            )
            .await?
        else {
            return Ok(None);
        };
        let mock = Arc::new(MockLlmClient::new(Vec::new()));
        threads.set_test_model_client(mock.clone());
        Ok(Some(Fixture {
            root: root.to_path_buf(),
            workspace,
            addr,
            threads,
            server,
            mock,
            client: crate::tls::reqwest_client(),
        }))
    }

    impl Fixture {
        fn url(&self, path: &str) -> String {
            format!("http://{}{path}", self.addr)
        }

        async fn create_thread(&self) -> Result<String> {
            let created: Value = self
                .client
                .post(self.url("/v1/threads"))
                .json(&json!({
                    "model": "deepseek-v4-pro",
                    "mode": "agent",
                    "workspace": self.workspace,
                    "trust_mode": true,
                    "auto_approve": true,
                }))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            Ok(created["id"].as_str().context("thread id")?.to_string())
        }

        /// Run one real turn whose model writes each `(path, content)` with
        /// `write_file`, then answers. Returns the settled turn record.
        async fn write_turn(&self, thread_id: &str, writes: &[(&str, &str)]) -> Result<Value> {
            for (path, content) in writes {
                let call_id = format!("call_{}", &Uuid::new_v4().simple().to_string()[..12]);
                self.mock.push_turn(canned::tool_call_turn(
                    &call_id,
                    "write_file",
                    &json!({ "path": path, "content": content }).to_string(),
                ));
            }
            self.mock.push_turn(canned::simple_text_turn("done"));
            let started: Value = self
                .client
                .post(self.url(&format!("/v1/threads/{thread_id}/turns")))
                .json(&json!({ "prompt": format!("write {writes:?}") }))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            let turn_id = started["turn"]["id"]
                .as_str()
                .context("turn id")?
                .to_string();
            let status = wait_for_terminal_turn_status(
                &self.client,
                self.addr,
                thread_id,
                &turn_id,
                Duration::from_secs(60),
            )
            .await?;
            let turn = self.turn(thread_id, &turn_id).await?;
            assert_eq!(status, "completed", "{turn:#}");
            Ok(turn)
        }

        async fn detail(&self, thread_id: &str) -> Result<Value> {
            Ok(self
                .client
                .get(self.url(&format!("/v1/threads/{thread_id}")))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?)
        }

        async fn turn(&self, thread_id: &str, turn_id: &str) -> Result<Value> {
            self.detail(thread_id).await?["turns"]
                .as_array()
                .and_then(|turns| turns.iter().find(|turn| turn["id"] == turn_id))
                .cloned()
                .context("turn not in thread detail")
        }

        async fn patch_undo(&self, thread_id: &str, depth: usize) -> Result<reqwest::Response> {
            Ok(self
                .client
                .post(self.url(&format!("/v1/threads/{thread_id}/patch-undo")))
                .json(&json!({ "depth": depth }))
                .send()
                .await?)
        }

        /// Patch-undo that must restore files; returns the fork's id.
        async fn undo_restoring(&self, thread_id: &str, depth: usize) -> Result<String> {
            let resp = self.patch_undo(thread_id, depth).await?;
            let status = resp.status();
            let body: Value = resp.json().await?;
            assert_eq!(status, StatusCode::CREATED, "{body:#}");
            assert_eq!(body["patch_result"]["files_restored"], true, "{body:#}");
            Ok(body["thread"]["id"]
                .as_str()
                .context("fork id")?
                .to_string())
        }

        async fn thread_count(&self) -> Result<usize> {
            let threads: Value = self
                .client
                .get(self.url("/v1/threads?include_archived=true"))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            Ok(threads.as_array().map_or(0, Vec::len))
        }

        fn read(&self, path: &str) -> Option<String> {
            fs::read_to_string(self.workspace.join(path)).ok()
        }
    }

    fn receipts(turn: &Value) -> Vec<Value> {
        turn["workspace_snapshots"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    fn receipt_kinds(turn: &Value) -> Vec<String> {
        receipts(turn)
            .iter()
            .map(|receipt| receipt["kind"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    fn test_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("codewhale-6621-{name}-{}", Uuid::new_v4()))
    }

    /// The issue's reproduction: a fresh HTTP thread writes a new file and
    /// overwrites an existing one (two writes, one turn), and patch-undo
    /// restores both — the new file deleted, the old one back to its exact
    /// bytes — and reports it. The turn record carries the restore points,
    /// tagged with the thread's own id.
    #[tokio::test]
    async fn fresh_thread_patch_undo_restores_its_own_turn() -> Result<()> {
        let _env = lock_test_env();
        let root = test_root("fresh");
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
        let Some(fx) = fixture(&root, TestServerOverrides::default()).await? else {
            return Ok(());
        };
        fs::write(fx.workspace.join("a.txt"), "original bytes\n")?;
        let thread_id = fx.create_thread().await?;
        let turn = fx
            .write_turn(
                &thread_id,
                &[("new.txt", "created"), ("a.txt", "overwritten")],
            )
            .await?;
        assert_eq!(fx.read("new.txt").as_deref(), Some("created"));
        assert_eq!(fx.read("a.txt").as_deref(), Some("overwritten"));
        assert_eq!(
            receipt_kinds(&turn),
            [
                "pre_turn",
                "tool",
                "post_tool",
                "tool",
                "post_tool",
                "post_turn"
            ],
            "{turn:#}"
        );
        // Each receipt after the first records what changed in the span it
        // closes: the write happened inside its own tool span.
        let changed: Vec<Value> = receipts(&turn)
            .iter()
            .map(|receipt| receipt["changed_paths"].clone())
            .collect();
        assert_eq!(
            changed,
            [
                Value::Null,
                json!([]),
                json!(["new.txt"]),
                json!([]),
                json!(["a.txt"]),
                json!([]),
            ],
            "{turn:#}"
        );
        assert!(
            receipts(&turn)
                .iter()
                .all(|receipt| receipt["session_id"] == thread_id.as_str()),
            "every restore point is tagged with the thread's own id: {turn:#}"
        );
        // The saved-session binding is untouched by any of this.
        assert!(fx.detail(&thread_id).await?["thread"]["session_id"].is_null());

        let fork = fx.undo_restoring(&thread_id, 0).await?;
        assert_eq!(fx.read("new.txt"), None, "the created file is removed");
        assert_eq!(
            fx.read("a.txt").as_deref(),
            Some("original bytes\n"),
            "both writes of the turn are undone, not only the last"
        );
        assert_ne!(fork, thread_id);
        Ok(())
    }

    /// `depth = 1` drops two turns and restores the workspace to before the
    /// older one. A chained undo on the returned fork restores the turn that
    /// fork inherited.
    #[tokio::test]
    async fn multi_turn_and_chained_undo_restore_whole_turns() -> Result<()> {
        let _env = lock_test_env();
        let root = test_root("depth");
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
        let Some(fx) = fixture(&root, TestServerOverrides::default()).await? else {
            return Ok(());
        };
        fs::write(fx.workspace.join("a.txt"), "v0")?;
        let thread_id = fx.create_thread().await?;
        fx.write_turn(&thread_id, &[("a.txt", "v1")]).await?;
        fx.write_turn(&thread_id, &[("a.txt", "v2"), ("b.txt", "b")])
            .await?;

        fx.undo_restoring(&thread_id, 1).await?;
        assert_eq!(fx.read("a.txt").as_deref(), Some("v0"));
        assert_eq!(fx.read("b.txt"), None);

        // Redo both by hand, then undo one turn at a time.
        let thread_id = fx.create_thread().await?;
        fx.write_turn(&thread_id, &[("a.txt", "w1")]).await?;
        fx.write_turn(&thread_id, &[("a.txt", "w2")]).await?;
        let fork = fx.undo_restoring(&thread_id, 0).await?;
        assert_eq!(fx.read("a.txt").as_deref(), Some("w1"));
        fx.undo_restoring(&fork, 0).await?;
        assert_eq!(
            fx.read("a.txt").as_deref(),
            Some("v0"),
            "the fork owns the turn it inherited"
        );
        Ok(())
    }

    /// `POST /fork` and `fork-at-turn` clone the turn records, so each fork
    /// restores the turns it inherited.
    #[tokio::test]
    async fn forks_restore_inherited_turns() -> Result<()> {
        let _env = lock_test_env();
        let root = test_root("fork");
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
        let Some(fx) = fixture(&root, TestServerOverrides::default()).await? else {
            return Ok(());
        };
        let thread_id = fx.create_thread().await?;
        let first = fx.write_turn(&thread_id, &[("one.txt", "1")]).await?;
        fx.write_turn(&thread_id, &[("two.txt", "2")]).await?;

        let forked: Value = fx
            .client
            .post(fx.url(&format!("/v1/threads/{thread_id}/fork")))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let fork_id = forked["id"].as_str().context("fork id")?.to_string();
        fx.undo_restoring(&fork_id, 0).await?;
        assert_eq!(fx.read("two.txt"), None);
        assert_eq!(fx.read("one.txt").as_deref(), Some("1"));

        // Put two.txt back so the original thread's state is consistent,
        // then branch after the first turn and undo that retained turn.
        fs::write(fx.workspace.join("two.txt"), "2")?;
        let branched: Value = fx
            .client
            .post(fx.url(&format!("/v1/threads/{thread_id}/fork-at-turn")))
            .json(&json!({ "turn_id": first["id"] }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let branch_id = branched["thread"]["id"]
            .as_str()
            .context("branch id")?
            .to_string();
        // two.txt came from a turn the branch dropped, and changed nothing
        // one.txt holds, so undoing the retained first turn removes one.txt
        // and leaves two.txt alone.
        fx.undo_restoring(&branch_id, 0).await?;
        assert_eq!(fx.read("one.txt"), None);
        assert_eq!(fx.read("two.txt").as_deref(), Some("2"));

        // A turn this Runtime ran without any tool provably changed nothing:
        // the undo forks and says so.
        fx.write_turn(&branch_id, &[]).await?;
        let resp = fx.patch_undo(&branch_id, 0).await?;
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body: Value = resp.json().await?;
        assert_eq!(body["patch_result"]["files_restored"], false, "{body:#}");
        assert_eq!(fx.read("two.txt").as_deref(), Some("2"));
        Ok(())
    }

    /// Ownership survives engine eviction and a full Runtime restart: the
    /// engine is rebuilt under the same thread id, a later turn is tagged with
    /// it too, and the earlier turn still restores.
    #[tokio::test]
    async fn ownership_survives_eviction_and_restart() -> Result<()> {
        let _env = lock_test_env();
        let root = test_root("restart");
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
        let Some(fx) = fixture(
            &root,
            TestServerOverrides {
                max_active_threads: Some(1),
                ..TestServerOverrides::default()
            },
        )
        .await?
        else {
            return Ok(());
        };
        let a = fx.create_thread().await?;
        fx.write_turn(&a, &[("a.txt", "from a")]).await?;
        // Loading B's engine evicts A's.
        let b = fx.create_thread().await?;
        fx.write_turn(&b, &[("b.txt", "from b")]).await?;
        let second = fx.write_turn(&a, &[("a2.txt", "a again")]).await?;
        assert!(
            receipts(&second)
                .iter()
                .all(|receipt| receipt["session_id"] == a.as_str()),
            "a rebuilt engine keeps the thread's identity: {second:#}"
        );
        fx.undo_restoring(&a, 0).await?;
        assert_eq!(fx.read("a2.txt"), None);

        // Restart: stop this Runtime and open another on the same root.
        let workspace = fx.workspace.clone();
        drop(fx);
        let deadline = tokio::time::Instant::now() + ci_scaled(Duration::from_secs(20));
        let fx = loop {
            match fixture(&root, TestServerOverrides::default()).await {
                Ok(Some(fx)) => break fx,
                Ok(None) => return Ok(()),
                Err(err) if tokio::time::Instant::now() < deadline => {
                    tracing::debug!("runtime not reopened yet: {err:#}");
                    sleep(Duration::from_millis(100)).await;
                }
                Err(err) => return Err(err),
            }
        };
        assert_eq!(fx.workspace, workspace);
        let after_restart = fx.write_turn(&b, &[("b2.txt", "b again")]).await?;
        assert!(
            receipts(&after_restart)
                .iter()
                .all(|receipt| receipt["session_id"] == b.as_str()),
            "{after_restart:#}"
        );
        fx.undo_restoring(&b, 0).await?;
        assert_eq!(fx.read("b2.txt"), None);
        fx.undo_restoring(&b, 1).await?;
        assert_eq!(fx.read("b.txt"), None, "a pre-restart turn restores");
        assert_eq!(fx.read("a.txt").as_deref(), Some("from a"));
        Ok(())
    }

    /// Saving the thread under any session document — the conversation's own
    /// id, an explicit other id, or a fresh `POST /v1/sessions` uuid — never
    /// changes which snapshots the thread owns. A thread resumed from a saved
    /// session has no recorded restore points for its imported turns, and its
    /// patch-undo fails honestly without forking or touching files.
    #[tokio::test]
    async fn save_and_resume_keep_ownership_honest() -> Result<()> {
        let _env = lock_test_env();
        let root = test_root("sessions");
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
        let Some(fx) = fixture(&root, TestServerOverrides::default()).await? else {
            return Ok(());
        };
        let thread_id = fx.create_thread().await?;
        fx.write_turn(&thread_id, &[("saved.txt", "one")]).await?;

        let created: Value = fx
            .client
            .post(fx.url("/v1/sessions"))
            .json(&json!({ "thread_id": thread_id }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let minted = created["session_id"]
            .as_str()
            .or_else(|| created["id"].as_str())
            .context("minted session id")?
            .to_string();
        let own: Value = fx
            .client
            .put(fx.url("/v1/sessions"))
            .json(&json!({ "thread_id": thread_id }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        // A nameless save updates the document the thread is bound to.
        assert_eq!(own["session_id"], minted.as_str());
        fx.client
            .put(fx.url("/v1/sessions"))
            .json(&json!({ "thread_id": thread_id, "session_id": "explicit-doc-6621" }))
            .send()
            .await?
            .error_for_status()?;
        assert_eq!(
            fx.detail(&thread_id).await?["thread"]["session_id"],
            "explicit-doc-6621"
        );

        // A turn after the rebinding is still tagged with the thread id.
        let later = fx.write_turn(&thread_id, &[("later.txt", "two")]).await?;
        assert!(
            receipts(&later)
                .iter()
                .all(|receipt| receipt["session_id"] == thread_id.as_str()),
            "{later:#}"
        );
        fx.undo_restoring(&thread_id, 1).await?;
        assert_eq!(fx.read("saved.txt"), None);
        assert_eq!(fx.read("later.txt"), None);

        // Resume the first (no longer held) document into a new thread.
        fs::write(fx.workspace.join("saved.txt"), "restored by hand")?;
        let resumed: Value = fx
            .client
            .post(fx.url(&format!("/v1/sessions/{minted}/resume-thread")))
            .json(&json!({}))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let resumed_id = resumed["thread_id"].as_str().context("resumed thread")?;
        assert_ne!(resumed_id, thread_id);
        let before = fx.thread_count().await?;
        let resp = fx.patch_undo(resumed_id, 0).await?;
        let status = resp.status();
        let body: Value = resp.json().await?;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "{body:#}\n{:#}",
            fx.detail(resumed_id).await?
        );
        assert_eq!(
            body["error"]["code"], "restore_point_unavailable",
            "{body:#}"
        );
        assert_eq!(fx.read("saved.txt").as_deref(), Some("restored by hand"));
        assert_eq!(
            fx.thread_count().await?,
            before,
            "a refused undo forks nothing"
        );
        Ok(())
    }

    /// Two threads in one workspace: neither can revert the other's snapshot
    /// (nor a TUI session's), and one thread's undo leaves the other's later
    /// writes in place.
    #[tokio::test]
    async fn threads_cannot_restore_each_others_snapshots() -> Result<()> {
        let _env = lock_test_env();
        let root = test_root("cross");
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
        let Some(fx) = fixture(&root, TestServerOverrides::default()).await? else {
            return Ok(());
        };
        let a = fx.create_thread().await?;
        let b = fx.create_thread().await?;
        let b_turn = fx.write_turn(&b, &[("b.txt", "from b")]).await?;
        let a_turn = fx.write_turn(&a, &[("a.txt", "from a")]).await?;
        let a_tool = receipts(&a_turn)
            .into_iter()
            .find(|receipt| receipt["kind"] == "tool")
            .context("a tool receipt")?;

        let repo = crate::snapshot::SnapshotRepo::open_or_init(&fx.workspace)?;
        let tui = repo.take_snapshot("tool:tui-call", Some("tui-session-6621"))?;
        let listed_a = repo
            .list(usize::MAX)?
            .into_iter()
            .find(|snapshot| snapshot.tree.as_str() == a_tool["tree_id"])
            .context("a's tool snapshot is listed")?;
        for foreign in [
            listed_a.id.as_str().to_string(),
            a_tool["snapshot_id"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            a_tool["tree_id"].as_str().unwrap_or_default().to_string(),
            tui.id.as_str().to_string(),
        ] {
            let resp = fx
                .client
                .post(fx.url(&format!("/v1/threads/{b}/file-revert")))
                .json(&json!({
                    "path": "a.txt",
                    "snapshot_id": foreign,
                    "expected_hash": sha256_of(&fx.workspace.join("a.txt")),
                }))
                .send()
                .await?;
            assert_eq!(resp.status(), StatusCode::CONFLICT, "{foreign}");
            assert_eq!(fx.read("a.txt").as_deref(), Some("from a"));
        }

        // B's undo rolls back b.txt only; A's later write survives.
        fx.undo_restoring(&b, 0).await?;
        assert_eq!(fx.read("b.txt"), None);
        assert_eq!(fx.read("a.txt").as_deref(), Some("from a"));
        let _ = b_turn;
        Ok(())
    }

    /// File-revert with the tool restore point from the thread's own turn
    /// record, on a fresh thread with no saved-session binding.
    #[tokio::test]
    async fn fresh_thread_file_revert_uses_its_turn_record() -> Result<()> {
        let _env = lock_test_env();
        let root = test_root("revert");
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
        let Some(fx) = fixture(&root, TestServerOverrides::default()).await? else {
            return Ok(());
        };
        fs::write(fx.workspace.join("kept.txt"), "before")?;
        let thread_id = fx.create_thread().await?;
        let turn = fx
            .write_turn(&thread_id, &[("kept.txt", "after"), ("made.txt", "new")])
            .await?;
        let tools: Vec<Value> = receipts(&turn)
            .into_iter()
            .filter(|receipt| receipt["kind"] == "tool")
            .collect();
        assert_eq!(tools.len(), 2, "{turn:#}");
        let url = fx.url(&format!("/v1/threads/{thread_id}/file-revert"));

        let stale = fx
            .client
            .post(&url)
            .json(&json!({
                "path": "kept.txt",
                "snapshot_id": tools[0]["snapshot_id"],
                "expected_hash": "absent",
            }))
            .send()
            .await?;
        assert_eq!(stale.status(), StatusCode::CONFLICT);
        assert_eq!(fx.read("kept.txt").as_deref(), Some("after"));

        let reverted: Value = fx
            .client
            .post(&url)
            .json(&json!({
                "path": "kept.txt",
                "snapshot_id": tools[0]["snapshot_id"],
                "expected_hash": sha256_of(&fx.workspace.join("kept.txt")),
            }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_eq!(reverted["action"], "modified");
        assert_eq!(fx.read("kept.txt").as_deref(), Some("before"));

        let removed: Value = fx
            .client
            .post(&url)
            .json(&json!({
                "path": "made.txt",
                "snapshot_id": tools[1]["tree_id"],
                "expected_hash": sha256_of(&fx.workspace.join("made.txt")),
            }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_eq!(removed["action"], "removed");
        assert_eq!(fx.read("made.txt"), None);
        Ok(())
    }

    /// An undo that cannot restore files fails honestly: snapshots disabled,
    /// a legacy seeded turn with tool calls and no receipts, a restore point
    /// pruned from the store, and an untrusted thread. Each is a 409 with a
    /// code, no fork, and unchanged files.
    #[tokio::test]
    async fn undo_that_cannot_restore_files_is_refused() -> Result<()> {
        let _env = lock_test_env();
        let root = test_root("honest");
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
        let config = Config {
            snapshots: Some(crate::config::SnapshotsConfig {
                enabled: false,
                ..crate::config::SnapshotsConfig::default()
            }),
            ..Config::default()
        }
        .with_legacy_root(
            Some("runtime-api-test-key".to_string()),
            Some("http://127.0.0.1:1/v1".to_string()),
        );
        let Some(fx) = fixture(
            &root,
            TestServerOverrides {
                config: Some(config),
                ..TestServerOverrides::default()
            },
        )
        .await?
        else {
            return Ok(());
        };
        let refused = |fx: &Fixture, thread: String, code: &'static str| {
            let fx_url = fx.url(&format!("/v1/threads/{thread}/patch-undo"));
            let client = fx.client.clone();
            async move {
                let resp = client.post(fx_url).json(&json!({})).send().await?;
                let status = resp.status();
                let body: Value = resp.json().await?;
                assert_eq!(status, StatusCode::CONFLICT, "{body:#}");
                assert_eq!(body["error"]["code"], code, "{body:#}");
                anyhow::Ok(())
            }
        };

        // Snapshots off: the turn wrote a file and recorded no restore point.
        let off = fx.create_thread().await?;
        let turn = fx.write_turn(&off, &[("off.txt", "written")]).await?;
        assert!(receipts(&turn).is_empty(), "{turn:#}");
        let before = fx.thread_count().await?;
        refused(&fx, off.clone(), "restore_point_unavailable").await?;
        assert_eq!(fx.read("off.txt").as_deref(), Some("written"));
        assert_eq!(fx.thread_count().await?, before);

        // A legacy turn that ran a tool before receipts existed.
        let legacy = fx.create_thread().await?;
        fx.threads
            .seed_thread_from_messages(
                &legacy,
                &[
                    Message {
                        role: Role::User,
                        content: vec![ContentBlock::Text {
                            text: "write legacy.txt".to_string(),
                            cache_control: None,
                        }],
                    },
                    Message {
                        role: Role::Assistant,
                        content: vec![ContentBlock::ToolUse {
                            execution_id: None,
                            id: "call_legacy".to_string(),
                            name: "write_file".to_string(),
                            input: json!({ "path": "legacy.txt", "content": "x" }),
                            caller: None,
                            thought_signature: None,
                        }],
                    },
                ],
            )
            .await?;
        refused(&fx, legacy, "restore_point_unavailable").await?;
        assert_eq!(fx.thread_count().await?, before + 1);
        drop(fx);

        // Snapshots on again: a pruned restore point and an untrusted thread.
        let root = test_root("honest-on");
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
        let Some(fx) = fixture(&root, TestServerOverrides::default()).await? else {
            return Ok(());
        };
        let pruned = fx.create_thread().await?;
        fx.write_turn(&pruned, &[("pruned.txt", "p")]).await?;
        crate::snapshot::SnapshotRepo::open_or_init(&fx.workspace)?
            .prune_older_than(Duration::ZERO)?;
        refused(&fx, pruned, "restore_point_pruned").await?;
        assert_eq!(fx.read("pruned.txt").as_deref(), Some("p"));

        let untrusted = fx.create_thread().await?;
        fx.write_turn(&untrusted, &[("u.txt", "u")]).await?;
        fx.client
            .patch(fx.url(&format!("/v1/threads/{untrusted}")))
            .json(&json!({ "trust_mode": false, "auto_approve": false }))
            .send()
            .await?
            .error_for_status()?;
        let before = fx.thread_count().await?;
        refused(&fx, untrusted, "restore_requires_trust").await?;
        assert_eq!(fx.read("u.txt").as_deref(), Some("u"));
        assert_eq!(fx.thread_count().await?, before);
        let _ = &fx.root;
        Ok(())
    }

    /// Run one turn from pre-pushed model steps and return it settled.
    async fn scripted_turn(fx: &Fixture, thread_id: &str, prompt: &str) -> Result<Value> {
        let started: Value = fx
            .client
            .post(fx.url(&format!("/v1/threads/{thread_id}/turns")))
            .json(&json!({ "prompt": prompt }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let turn_id = started["turn"]["id"]
            .as_str()
            .context("turn id")?
            .to_string();
        let status = wait_for_terminal_turn_status(
            &fx.client,
            fx.addr,
            thread_id,
            &turn_id,
            Duration::from_secs(60),
        )
        .await?;
        let turn = fx.turn(thread_id, &turn_id).await?;
        assert_eq!(status, "completed", "{turn:#}");
        Ok(turn)
    }

    /// Another writer changes a different file while thread A's turn runs
    /// (here: while the model is thinking, between tool calls). A's
    /// patch-undo must not revert that file as if the turn had written it:
    /// it refuses with `workspace_changed_since_turn`, forks nothing and
    /// changes nothing. Without the other writer the same kind of turn
    /// restores.
    #[tokio::test]
    async fn a_concurrent_writers_change_is_never_undone_as_the_turns() -> Result<()> {
        let _env = lock_test_env();
        let root = test_root("concurrent");
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
        let Some(fx) = fixture(&root, TestServerOverrides::default()).await? else {
            return Ok(());
        };
        fs::write(fx.workspace.join("sibling.txt"), "sibling before")?;
        let thread_id = fx.create_thread().await?;
        fx.mock.push_turn(canned::tool_call_turn(
            "call_own_write",
            "write_file",
            &json!({ "path": "own.txt", "content": "from the turn" }).to_string(),
        ));
        let sibling = fx.workspace.join("sibling.txt");
        fx.mock.push_factory(move |_| {
            fs::write(&sibling, "another thread's edit").expect("concurrent write");
            canned::simple_text_turn("done")
        });
        let turn = scripted_turn(&fx, &thread_id, "write own.txt").await?;
        assert_eq!(fx.read("own.txt").as_deref(), Some("from the turn"));

        let before = fx.thread_count().await?;
        let resp = fx.patch_undo(&thread_id, 0).await?;
        let status = resp.status();
        let body: Value = resp.json().await?;
        assert_eq!(status, StatusCode::CONFLICT, "{body:#}\n{turn:#}");
        assert_eq!(
            body["error"]["code"], "workspace_changed_since_turn",
            "{body:#}"
        );
        let message = body["error"]["message"].as_str().unwrap_or_default();
        assert!(message.contains("sibling.txt"), "{message}");
        assert_eq!(
            fx.read("sibling.txt").as_deref(),
            Some("another thread's edit")
        );
        assert_eq!(fx.read("own.txt").as_deref(), Some("from the turn"));
        assert_eq!(
            fx.thread_count().await?,
            before,
            "a refused undo forks nothing"
        );

        // A shell command's writes are the turn's own: the engine bounds the
        // call with its own snapshots, so the undo restores them.
        let shell_thread: Value = fx
            .client
            .post(fx.url("/v1/threads"))
            .json(&json!({
                "model": "deepseek-v4-pro",
                "mode": "agent",
                "workspace": fx.workspace,
                "trust_mode": true,
                "auto_approve": true,
                "allow_shell": true,
            }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let shell_thread = shell_thread["id"]
            .as_str()
            .context("thread id")?
            .to_string();
        fx.mock.push_turn(canned::tool_call_turn(
            "call_shell_write",
            "bash",
            &json!({ "command": "printf shell > shell.txt" }).to_string(),
        ));
        fx.mock.push_turn(canned::simple_text_turn("done"));
        let shell_turn = scripted_turn(&fx, &shell_thread, "run a command").await?;
        assert_eq!(
            fx.read("shell.txt").as_deref(),
            Some("shell"),
            "{shell_turn:#}\n{:#}",
            fx.detail(&shell_thread).await?["items"]
        );
        assert!(
            receipt_kinds(&shell_turn).contains(&"post_tool".to_string()),
            "{shell_turn:#}"
        );
        fx.undo_restoring(&shell_thread, 0).await?;
        assert_eq!(fx.read("shell.txt"), None);
        assert_eq!(
            fx.read("sibling.txt").as_deref(),
            Some("another thread's edit")
        );
        Ok(())
    }

    /// A turn whose file tool writes a gitignored file changed something no
    /// snapshot holds. Its patch-undo is a `409 path_not_snapshotted`, never
    /// a `201` claiming there was nothing to restore; no fork, file intact.
    #[tokio::test]
    async fn undo_of_a_write_to_an_ignored_path_is_refused() -> Result<()> {
        let _env = lock_test_env();
        let root = test_root("ignored");
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
        let Some(fx) = fixture(&root, TestServerOverrides::default()).await? else {
            return Ok(());
        };
        fs::write(fx.workspace.join(".gitignore"), ".env.local\n")?;
        fs::write(fx.workspace.join(".env.local"), "TOKEN=old\n")?;
        let thread_id = fx.create_thread().await?;
        fx.write_turn(&thread_id, &[(".env.local", "TOKEN=new\n")])
            .await?;
        let before = fx.thread_count().await?;
        let resp = fx.patch_undo(&thread_id, 0).await?;
        let status = resp.status();
        let body: Value = resp.json().await?;
        assert_eq!(status, StatusCode::CONFLICT, "{body:#}");
        assert_eq!(body["error"]["code"], "path_not_snapshotted", "{body:#}");
        assert!(
            body["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains(".env.local")),
            "{body:#}"
        );
        assert_eq!(fx.read(".env.local").as_deref(), Some("TOKEN=new\n"));
        assert_eq!(fx.thread_count().await?, before);
        Ok(())
    }

    /// A turn with more file-modifying tool calls than the snapshot count
    /// cap still undoes: the count prune keeps the turn's own pre-turn and
    /// post-turn restore points (cap lowered to 3 for this workspace so the
    /// turn does not need fifty writes).
    #[tokio::test]
    async fn a_turn_with_more_writes_than_the_snapshot_cap_still_undoes() -> Result<()> {
        let _env = lock_test_env();
        let root = test_root("burst");
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
        let Some(fx) = fixture(&root, TestServerOverrides::default()).await? else {
            return Ok(());
        };
        crate::core::turn::test_max_snapshots::set(&fx.workspace, 3);
        fs::write(fx.workspace.join("f0.txt"), "original")?;
        let thread_id = fx.create_thread().await?;
        let writes: Vec<(String, String)> = (0..5)
            .map(|i| (format!("f{i}.txt"), format!("write {i}")))
            .collect();
        let writes: Vec<(&str, &str)> = writes
            .iter()
            .map(|(path, content)| (path.as_str(), content.as_str()))
            .collect();
        fx.write_turn(&thread_id, &writes).await?;
        let listed =
            crate::snapshot::SnapshotRepo::open_or_init(&fx.workspace)?.list(usize::MAX)?;
        assert!(
            listed.len() < 12,
            "the cap pruned the tool snapshots: {}",
            listed.len()
        );

        fx.undo_restoring(&thread_id, 0).await?;
        assert_eq!(fx.read("f0.txt").as_deref(), Some("original"));
        for i in 1..5 {
            assert_eq!(fx.read(&format!("f{i}.txt")), None);
        }
        Ok(())
    }

    /// A thread resumed from saved document X is bound to X; saving it
    /// without naming a document updates X rather than writing a second
    /// document under another name.
    #[tokio::test]
    async fn nameless_save_of_a_resumed_thread_updates_its_document() -> Result<()> {
        let _env = lock_test_env();
        let root = test_root("resave");
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
        let Some(fx) = fixture(&root, TestServerOverrides::default()).await? else {
            return Ok(());
        };
        let thread_id = fx.create_thread().await?;
        fx.write_turn(&thread_id, &[]).await?;
        let created: Value = fx
            .client
            .post(fx.url("/v1/sessions"))
            .json(&json!({ "thread_id": thread_id }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let document = created["session_id"]
            .as_str()
            .or_else(|| created["id"].as_str())
            .context("saved session id")?
            .to_string();
        let manager = crate::session_manager::SessionManager::new(
            crate::session_manager::default_sessions_dir()?,
        )?;
        let messages_before = manager.load_session(&document)?.messages.len();

        let resumed: Value = fx
            .client
            .post(fx.url(&format!("/v1/sessions/{document}/resume-thread")))
            .json(&json!({}))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let resumed_id = resumed["thread_id"]
            .as_str()
            .context("resumed thread")?
            .to_string();
        assert_eq!(
            fx.detail(&resumed_id).await?["thread"]["session_id"],
            document.as_str()
        );
        fx.write_turn(&resumed_id, &[]).await?;
        let documents_before = manager.list_sessions()?.len();

        let saved: Value = fx
            .client
            .put(fx.url("/v1/sessions"))
            .json(&json!({ "thread_id": resumed_id }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_eq!(saved["session_id"], document.as_str(), "{saved:#}");
        assert_eq!(
            manager.list_sessions()?.len(),
            documents_before,
            "no second document"
        );
        assert!(
            manager.load_session(&document)?.messages.len() > messages_before,
            "the resumed turn was saved into the bound document"
        );
        assert_eq!(
            fx.detail(&resumed_id).await?["thread"]["session_id"],
            document.as_str()
        );
        Ok(())
    }
}

#[tokio::test]
async fn switch_provider_away_and_back_keeps_the_root_default_with_its_route() -> Result<()> {
    // Device-test regression: `provider = "openai"` with the model only in the
    // root `default_text_model`. Switching to a pass-through route used to leave
    // the alias at the root, so that route inherited `gpui-fixture`, the
    // provider list advertised OpenAI's catalog default (`gpt-5.6`), and the
    // desktop model chip landed there.
    let root = std::env::temp_dir().join(format!("codewhale-switch-root-alias-{}", Uuid::new_v4()));
    fs::create_dir_all(&root)?;
    let config_file = root.join("custom-config.toml");
    fs::write(
        &config_file,
        r#"provider = "openai"
default_text_model = "gpui-fixture"

[providers.openai]
api_key = "sk-test"
base_url = "http://127.0.0.1:9/v1"

[providers.openrouter]
api_key = "sk-or-test"
"#,
    )?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let (status, away) =
        post_switch_provider(&client, &addr, "openrouter", &serde_json::json!({})).await;
    assert_eq!(status, StatusCode::OK, "body: {away}");
    assert_ne!(
        away["model"].as_str(),
        Some("gpui-fixture"),
        "openrouter must not inherit openai's root default: {away}"
    );
    let persisted = fs::read_to_string(&config_file)?;
    assert!(
        !persisted.contains("default_text_model"),
        "the alias must move off the root. Actual config:\n{persisted}"
    );

    let providers: serde_json::Value = client
        .get(format!("http://{addr}/v1/providers"))
        .send()
        .await?
        .json()
        .await?;
    let openai = providers["providers"]
        .as_array()
        .and_then(|entries| entries.iter().find(|entry| entry["id"] == "openai"))
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        openai["default_model"].as_str(),
        Some("gpui-fixture"),
        "the inactive openai entry must keep its configured model, not the catalog default: {openai}"
    );

    let (status, back) =
        post_switch_provider(&client, &addr, "openai", &serde_json::json!({})).await;
    assert_eq!(status, StatusCode::OK, "body: {back}");
    assert_eq!(back["model"].as_str(), Some("gpui-fixture"), "body: {back}");
    assert_eq!(get_config(&client, &addr).await["model"], "gpui-fixture");

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn switch_provider_does_not_resurrect_a_shadowed_legacy_root_model() -> Result<()> {
    // Review regression (#6693): the legacy root `model` was shadowed by
    // `default_text_model`. Moving only the alias off the root let the
    // pass-through openrouter route resolve `deepseek-v4-flash`.
    let root = std::env::temp_dir().join(format!("codewhale-switch-legacy-{}", Uuid::new_v4()));
    fs::create_dir_all(&root)?;
    let config_file = root.join("custom-config.toml");
    fs::write(
        &config_file,
        r#"model = "deepseek-v4-flash"
default_text_model = "gpui-fixture"
provider = "openai"

[providers.openai]
api_key = "sk-test"
base_url = "http://127.0.0.1:9/v1"

[providers.openrouter]
api_key = "sk-or-test"
"#,
    )?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file.clone()).await?
    else {
        return Ok(());
    };
    let client = crate::tls::reqwest_client();

    let (status, away) =
        post_switch_provider(&client, &addr, "openrouter", &serde_json::json!({})).await;
    assert_eq!(status, StatusCode::OK, "body: {away}");
    // OpenRouter normalizes the stale value to `deepseek/deepseek-v4-flash`.
    assert!(
        !away["model"]
            .as_str()
            .unwrap_or_default()
            .contains("deepseek-v4-flash"),
        "body: {away}"
    );
    assert_ne!(away["model"].as_str(), Some("gpui-fixture"), "body: {away}");

    let (status, back) =
        post_switch_provider(&client, &addr, "openai", &serde_json::json!({})).await;
    assert_eq!(status, StatusCode::OK, "body: {back}");
    assert_eq!(back["model"].as_str(), Some("gpui-fixture"), "body: {back}");

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn session_export_keeps_execution_identity_and_rejects_invalid_item_correlation() -> Result<()>
{
    let _env = lock_test_env();
    let dir = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("CODEWHALE_HOME", dir.path());
    let root = dir.path().join("server");
    let sessions = dir.path().join("sessions");
    let (addr, runtime, handle) = spawn_test_server_with_root(root.clone(), sessions.clone())
        .await?
        .context("execution identity export requires the local API fixture")?;
    let thread = runtime.create_thread(Default::default()).await?;
    let original: Vec<Message> = serde_json::from_value(json!([
        {"role":"user","content":[{"type":"text","text":"export exact executions"}]},
        {"role":"assistant","content":[{"type":"tool_use","id":"same-wire","execution_id":"host-first","name":"read_file","input":{"path":"one"},"caller":{"type":"subagent","tool_id":"parent"},"thought_signature":"signature"}]},
        {"role":"user","content":[{"type":"tool_result","tool_use_id":"same-wire","execution_id":"host-first","content":"one","content_blocks":[{"type":"text","text":"rich"}]}]},
        {"role":"assistant","content":[{"type":"tool_use","id":"same-wire","execution_id":"host-second","name":"read_file","input":{"path":"two"}}]},
        {"role":"user","content":[{"type":"tool_result","tool_use_id":"same-wire","execution_id":"host-second","content":"two"}]}
    ]))?;
    runtime
        .seed_thread_from_messages(&thread.id, &original)
        .await?;
    let detail = runtime.get_thread_detail(&thread.id).await?;
    assert_eq!(messages_from_thread_detail(&detail)?, original);

    // A completed live item contains both sides. It must not become only a
    // result when the same thread is saved through the API's projection.
    let mut coalesced = detail.clone();
    let result_index = coalesced
        .items
        .iter()
        .position(|item| {
            item.metadata
                .as_ref()
                .is_some_and(|m| m["tool_result_for"] == "host-first")
        })
        .context("result item")?;
    let result_item = coalesced.items.remove(result_index);
    let call = coalesced
        .items
        .iter_mut()
        .find(|item| {
            item.metadata
                .as_ref()
                .is_some_and(|m| m["tool_use_id"] == "host-first")
        })
        .context("call item")?;
    let metadata = call.metadata.as_mut().unwrap();
    metadata["tool_input"] = json!(call.detail.clone().unwrap());
    metadata["tool_result_for"] = json!("host-first");
    metadata["content_blocks"] = result_item.metadata.as_ref().unwrap()["content_blocks"].clone();
    call.detail = result_item.detail;
    for turn in &mut coalesced.turns {
        turn.item_ids.retain(|id| id != &result_item.id);
    }
    assert_eq!(messages_from_thread_detail(&coalesced)?, original);

    let client = crate::tls::reqwest_client();
    let exported = client
        .post(format!("http://{addr}/v1/sessions"))
        .json(&json!({"thread_id":thread.id}))
        .send()
        .await?;
    assert_eq!(exported.status(), StatusCode::CREATED);
    let exported: Value = exported.json().await?;
    let id = exported["session_id"].as_str().context("session id")?;
    let manager = crate::session_manager::SessionManager::new(sessions.clone())?;
    let saved = manager.load_session(id)?;
    assert_eq!(saved.messages, original);
    assert_eq!(
        saved.journal.as_ref().context("journal")?.to_messages(),
        original
    );
    let display: Value = client
        .get(format!("http://{addr}/v1/sessions/{id}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert!(
        display["messages"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|m| m["content"].as_array().unwrap())
            .all(|block| block.get("execution_id").is_none()),
        "detail stays display-only"
    );

    // Corrupt an identity in the existing durable record. The export must
    // report the error, without overwriting its prior valid saved document.
    let mut broken = detail
        .items
        .iter()
        .find(|item| {
            item.metadata
                .as_ref()
                .is_some_and(|m| m["tool_use_id"] == "host-first")
        })
        .context("first call")?
        .clone();
    broken.metadata.as_mut().unwrap()["execution_id"] = json!("");
    fs::write(
        root.join("runtime/runtime/items")
            .join(format!("{}.json", broken.id)),
        serde_json::to_vec(&broken)?,
    )?;
    let saved_path = sessions.join(format!("{id}.json"));
    let before = fs::read(&saved_path)?;
    let refused = client
        .post(format!("http://{addr}/v1/sessions"))
        .json(&json!({"thread_id":thread.id}))
        .send()
        .await?;
    assert_eq!(refused.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        refused
            .text()
            .await?
            .contains("Invalid stored tool execution identity")
    );
    assert_eq!(fs::read(saved_path)?, before);
    handle.abort();
    Ok(())
}
