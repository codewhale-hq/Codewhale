
    #[test]
    fn exec_only_installs_an_explicit_headless_turn_cap() {
        let cli = parse_cli(&["codewhale", "exec", "--auto", "benchmark this"]);
        let Some(Commands::Exec(args)) = cli.command else {
            panic!("expected exec command");
        };

        assert_eq!(args.max_turns, None);
        let defaulted = exec_max_steps(args.max_turns);
        assert_eq!(
            defaulted,
            crate::core::engine::turn_budget::DEFAULT_MAX_MODEL_STEPS
        );
        assert_eq!(
            crate::core::turn::TurnContext::new(defaulted).step_limit(),
            None
        );
        assert_eq!(exec_max_steps(Some(7)), 7);
        let mut capped = crate::core::turn::TurnContext::new(exec_max_steps(Some(7)));
        for _ in 0..7 {
            capped.next_step();
        }
        assert!(capped.at_max_steps());
        assert_eq!(capped.stop_diagnostics.effective_max_steps, Some(7));
        assert_eq!(
            exec_max_steps(Some(u32::MAX)),
            crate::core::engine::turn_budget::MAX_MAX_MODEL_STEPS,
            "even the largest override stays finite"
        );
    }

    #[test]
    fn exec_accepts_continue_for_latest_workspace_session() {
        let cli = parse_cli(&["codewhale", "exec", "--continue", "follow up"]);
        let Some(Commands::Exec(args)) = cli.command else {
            panic!("expected exec command");
        };

        assert!(args.continue_session);
    }

    #[test]
    fn sessions_footer_points_to_resume_subcommand() {
        let cli = parse_cli(&["codewhale", "resume", "abc123"]);
        let Some(Commands::Resume { session_id, last }) = cli.command else {
            panic!("expected resume command");
        };

        assert_eq!(session_id.as_deref(), Some("abc123"));
        assert!(!last);
        assert_eq!(sessions_resume_command(), "codewhale resume");
        assert!(!sessions_resume_command().contains("--resume"));
    }

    #[test]
    fn plugin_registry_initialization_precedes_dotenv_for_all_launch_paths() {
        use std::cell::Cell;

        #[derive(Clone, Copy)]
        enum Expected {
            Plain,
            Resume,
            Fork,
            Exec,
            Serve,
        }

        let cases: &[(&[&str], Expected)] = &[
            (&["codewhale"], Expected::Plain),
            (&["codewhale", "resume", "--last"], Expected::Resume),
            (&["codewhale", "fork", "--last"], Expected::Fork),
            (&["codewhale", "exec", "probe"], Expected::Exec),
            (&["codewhale", "serve", "--mcp"], Expected::Serve),
        ];

        for (args, expected) in cases {
            let phase = Cell::new(0);
            let (_cli, command) = prepare_cli_startup(
                parse_cli(args),
                || {
                    assert_eq!(phase.get(), 0, "plugin init order for {args:?}");
                    phase.set(1);
                },
                || {
                    assert_eq!(phase.get(), 1, "dotenv load order for {args:?}");
                    phase.set(2);
                },
            );

            assert_eq!(phase.get(), 2, "startup phases for {args:?}");
            let correct_variant = matches!(
                (expected, command.as_ref()),
                (Expected::Plain, None)
                    | (Expected::Resume, Some(Commands::Resume { .. }))
                    | (Expected::Fork, Some(Commands::Fork { .. }))
                    | (Expected::Exec, Some(Commands::Exec(_)))
                    | (Expected::Serve, Some(Commands::Serve(_)))
            );
            assert!(correct_variant, "unexpected command for {args:?}");
        }
    }

    #[test]
    fn scorecard_rejects_a_threshold_that_cannot_gate() {
        for bad in ["NaN", "inf", "-inf"] {
            assert!(
                Cli::try_parse_from([
                    "codewhale",
                    "scorecard",
                    "--input",
                    "t.json",
                    "--threshold",
                    bad
                ])
                .is_err(),
                "--threshold {bad} must be refused"
            );
        }
        assert!(
            Cli::try_parse_from([
                "codewhale",
                "scorecard",
                "--input",
                "t.json",
                "--threshold",
                "2.5"
            ])
            .is_ok()
        );
    }

    #[test]
    fn workspace_dotenv_is_found_from_the_launch_workspace_not_the_process_directory() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let nested = workspace.path().join("crates/app");
        std::fs::create_dir_all(&nested).expect("mkdir nested");
        std::fs::create_dir_all(workspace.path().join(".git")).expect("mkdir .git");
        std::fs::write(workspace.path().join(".env"), "OPENAI_API_KEY=workspace\n")
            .expect("write .env");

        // `--workspace <dir>` from a process directory in another tree.
        assert_eq!(
            find_workspace_dotenv(workspace.path()).expect("search"),
            Some(workspace.path().join(".env"))
        );
        // Nested launch directory walks up to the repository root.
        assert_eq!(
            find_workspace_dotenv(&nested).expect("search"),
            Some(workspace.path().join(".env"))
        );
    }

    #[test]
    fn workspace_dotenv_loads_only_provider_credentials_and_preserves_shell_values() {
        let _lock = crate::test_support::lock_test_env();
        let _deepseek = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY");
        let _nvidia = crate::test_support::EnvVarGuard::set("NVIDIA_API_KEY", "shell-key");
        let _home = crate::test_support::EnvVarGuard::remove("CODEWHALE_HOME");
        let _config = crate::test_support::EnvVarGuard::remove("CODEWHALE_CONFIG_PATH");
        let _shell = crate::test_support::EnvVarGuard::remove("DEEPSEEK_ALLOW_SHELL");
        let tmp = tempfile::TempDir::new().expect("temp workspace");
        let dotenv = tmp.path().join(".env");
        std::fs::write(
            &dotenv,
            "DEEPSEEK_API_KEY=workspace-key\n\
             NVIDIA_API_KEY=repo-must-not-override-shell\n\
             CODEWHALE_HOME=./attacker-home\n\
             CODEWHALE_CONFIG_PATH=./attacker.toml\n\
             DEEPSEEK_ALLOW_SHELL=true\n",
        )
        .expect("write dotenv");

        let report = load_workspace_dotenv_credentials_from_path(&dotenv).expect("safe load");

        assert_eq!(
            std::env::var("DEEPSEEK_API_KEY").as_deref(),
            Ok("workspace-key")
        );
        assert_eq!(std::env::var("NVIDIA_API_KEY").as_deref(), Ok("shell-key"));
        assert!(std::env::var_os("CODEWHALE_HOME").is_none());
        assert!(std::env::var_os("CODEWHALE_CONFIG_PATH").is_none());
        assert!(std::env::var_os("DEEPSEEK_ALLOW_SHELL").is_none());
        assert_eq!(
            report.loaded,
            BTreeSet::from(["DEEPSEEK_API_KEY".to_string()])
        );
        assert_eq!(
            report.ignored,
            BTreeSet::from([
                "CODEWHALE_CONFIG_PATH".to_string(),
                "CODEWHALE_HOME".to_string(),
                "DEEPSEEK_ALLOW_SHELL".to_string(),
            ])
        );
    }

    #[test]
    fn workspace_dotenv_rejects_ambient_variable_substitution() {
        let _lock = crate::test_support::lock_test_env();
        let _deepseek = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY");
        let _ambient = crate::test_support::EnvVarGuard::set(
            "CODEWHALE_JS_SECRET_LEAK_TEST",
            "ambient-secret-must-not-expand",
        );
        let tmp = tempfile::TempDir::new().expect("temp workspace");
        let dotenv = tmp.path().join(".env");
        std::fs::write(
            &dotenv,
            "DEEPSEEK_API_KEY=${CODEWHALE_JS_SECRET_LEAK_TEST}\n",
        )
        .expect("write dotenv");

        let error = load_workspace_dotenv_credentials_from_path(&dotenv)
            .expect_err("expansion must fail closed")
            .to_string();

        assert!(error.contains("variable expansion"));
        assert!(!error.contains("ambient-secret-must-not-expand"));
        assert!(std::env::var_os("DEEPSEEK_API_KEY").is_none());
    }

    #[test]
    fn workspace_dotenv_rejects_multiline_ambient_variable_substitution() {
        let _lock = crate::test_support::lock_test_env();
        let _deepseek = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY");
        let _ambient = crate::test_support::EnvVarGuard::set(
            "CODEWHALE_JS_SECRET_LEAK_TEST",
            "ambient-secret-must-not-expand",
        );
        let tmp = tempfile::TempDir::new().expect("temp workspace");
        let dotenv = tmp.path().join(".env");
        std::fs::write(
            &dotenv,
            "DEEPSEEK_API_KEY=\"prefix\n$CODEWHALE_JS_SECRET_LEAK_TEST=bar\nsuffix\"\n",
        )
        .expect("write dotenv");

        let error = load_workspace_dotenv_credentials_from_path(&dotenv)
            .expect_err("multiline expansion must fail closed")
            .to_string();

        assert!(error.contains("variable expansion"));
        assert!(!error.contains("ambient-secret-must-not-expand"));
        assert!(std::env::var_os("DEEPSEEK_API_KEY").is_none());
    }

    #[test]
    fn workspace_dotenv_comment_quote_cannot_hide_later_expansion() {
        let _lock = crate::test_support::lock_test_env();
        let _deepseek = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY");
        let _ambient = crate::test_support::EnvVarGuard::set(
            "CODEWHALE_JS_SECRET_LEAK_TEST",
            "ambient-secret-must-not-expand",
        );
        let tmp = tempfile::TempDir::new().expect("temp workspace");
        let dotenv = tmp.path().join(".env");
        std::fs::write(
            &dotenv,
            "# unmatched quote in ignored comment: '\n\
             DEEPSEEK_API_KEY=$CODEWHALE_JS_SECRET_LEAK_TEST\n",
        )
        .expect("write dotenv");

        let error = load_workspace_dotenv_credentials_from_path(&dotenv)
            .expect_err("comment quote must not hide expansion")
            .to_string();

        assert!(error.contains("variable expansion"));
        assert!(!error.contains("ambient-secret-must-not-expand"));
        assert!(std::env::var_os("DEEPSEEK_API_KEY").is_none());
    }

    #[test]
    fn workspace_dotenv_allows_single_quoted_literal_dollar() {
        let _lock = crate::test_support::lock_test_env();
        let _deepseek = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY");
        let tmp = tempfile::TempDir::new().expect("temp workspace");
        let dotenv = tmp.path().join(".env");
        std::fs::write(&dotenv, "DEEPSEEK_API_KEY='$literal-value'\n").expect("write dotenv");

        load_workspace_dotenv_credentials_from_path(&dotenv).expect("literal dollar load");

        assert_eq!(
            std::env::var("DEEPSEEK_API_KEY").as_deref(),
            Ok("$literal-value")
        );
    }

    #[test]
    fn workspace_dotenv_parse_failure_applies_no_earlier_credentials() {
        let _lock = crate::test_support::lock_test_env();
        let _deepseek = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY");
        let tmp = tempfile::TempDir::new().expect("temp workspace");
        let dotenv = tmp.path().join(".env");
        std::fs::write(
            &dotenv,
            "DEEPSEEK_API_KEY=must-not-survive\nBROKEN=\"unterminated\n",
        )
        .expect("write dotenv");

        let error = load_workspace_dotenv_credentials_from_path(&dotenv)
            .expect_err("parse failure must be transactional")
            .to_string();

        assert!(error.contains("could not be parsed safely"), "{error}");
        assert!(!error.contains("must-not-survive"));
        assert!(std::env::var_os("DEEPSEEK_API_KEY").is_none());
    }

    #[test]
    fn workspace_dotenv_credential_allowlist_excludes_control_plane_names() {
        for provider in codewhale_config::provider::providers_sorted_for_display() {
            for key in provider.env_vars() {
                assert!(
                    is_workspace_dotenv_credential_key(key),
                    "provider credential {key} must remain supported"
                );
            }
        }
        for key in [
            "CODEWHALE_HOME",
            "CODEWHALE_CONFIG_PATH",
            "DEEPSEEK_CONFIG_PATH",
            "DEEPSEEK_PROFILE",
            "DEEPSEEK_MANAGED_CONFIG_PATH",
            "DEEPSEEK_REQUIREMENTS_PATH",
            "DEEPSEEK_PROVIDER",
            "DEEPSEEK_BASE_URL",
            "DEEPSEEK_MODEL",
            "DEEPSEEK_APPROVAL_POLICY",
            "DEEPSEEK_SANDBOX_MODE",
            "DEEPSEEK_ALLOW_SHELL",
            "DEEPSEEK_YOLO",
            "DEEPSEEK_MCP_CONFIG",
            "CODEWHALE_RUNTIME_TOKEN",
            "PATH",
            "NODE_OPTIONS",
            "PYTHONPATH",
            "LD_PRELOAD",
            "DYLD_INSERT_LIBRARIES",
        ] {
            assert!(
                !is_workspace_dotenv_credential_key(key),
                "control-plane variable {key} must not load from a workspace"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn workspace_dotenv_does_not_follow_symbolic_links() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::TempDir::new().expect("temp workspace");
        let external = tmp.path().join("external-credentials");
        let dotenv = tmp.path().join(".env");
        std::fs::write(&external, "DEEPSEEK_API_KEY=external-secret\n")
            .expect("write external fixture");
        symlink(&external, &dotenv).expect("create dotenv symlink");

        let error = load_workspace_dotenv_credentials_from_path(&dotenv)
            .expect_err("symlink must fail closed")
            .to_string();

        assert!(error.contains("securely open"), "{error}");
        assert!(!error.contains("external-secret"));
    }

    #[cfg(unix)]
    #[test]
    fn workspace_dotenv_rejects_hard_links_to_external_files() {
        let tmp = tempfile::TempDir::new().expect("temp workspace");
        let external = tmp.path().join("external-credentials");
        let dotenv = tmp.path().join(".env");
        std::fs::write(&external, "DEEPSEEK_API_KEY=external-secret\n")
            .expect("write external fixture");
        std::fs::hard_link(&external, &dotenv).expect("create dotenv hard link");

        let error = load_workspace_dotenv_credentials_from_path(&dotenv)
            .expect_err("hard link must fail closed")
            .to_string();

        assert!(error.contains("multiple filesystem links"), "{error}");
        assert!(!error.contains("external-secret"));
    }

    #[cfg(unix)]
    #[test]
    fn workspace_dotenv_rejects_fifo_without_blocking_startup() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        use std::sync::mpsc;
        use std::time::Duration;

        let tmp = tempfile::TempDir::new().expect("temp workspace");
        let dotenv = tmp.path().join(".env");
        let c_path = CString::new(dotenv.as_os_str().as_bytes()).expect("fifo path");
        // SAFETY: `c_path` is a live, NUL-terminated path and the requested
        // mode grants access only to the current user.
        let result = unsafe { libc::mkfifo(c_path.as_ptr(), libc::S_IRUSR | libc::S_IWUSR) };
        assert_eq!(result, 0, "mkfifo failed: {}", io::Error::last_os_error());

        let (tx, rx) = mpsc::channel();
        let worker_path = dotenv.clone();
        let worker = std::thread::spawn(move || {
            let result = load_workspace_dotenv_credentials_from_path(&worker_path)
                .map(|_| "unexpected success".to_string())
                .unwrap_or_else(|error| error.to_string());
            tx.send(result).expect("send loader result");
        });

        let error = match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(error) => error,
            Err(timeout) => {
                // Release a regressed blocking reader so the test can fail
                // promptly instead of leaving a stuck process behind.
                let _writer = std::fs::OpenOptions::new()
                    .write(true)
                    .open(&dotenv)
                    .expect("open fifo writer to release blocked reader");
                let _ = rx.recv_timeout(Duration::from_secs(1));
                worker.join().expect("join released loader");
                panic!("workspace .env FIFO blocked startup: {timeout}");
            }
        };
        worker.join().expect("join loader");

        assert!(error.contains("not a regular file"), "{error}");
    }

    #[test]
    fn exec_json_conflicts_with_stream_json_output() {
        let err = Cli::try_parse_from([
            "codewhale",
            "exec",
            "--json",
            "--output-format",
            "stream-json",
            "hello",
        ])
        .expect_err("json summary and stream-json must not mix");

        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn exec_stream_turn_usage_event_serializes_reported_fields() {
        let event = ExecStreamEvent::TurnUsage {
            turn: 2,
            input_tokens: 1200,
            output_tokens: 180,
            reasoning_tokens: Some(90),
            prompt_cache_hit_tokens: Some(900),
            prompt_cache_miss_tokens: Some(300),
            prompt_cache_write_tokens: Some(0),
            reasoning_replay_tokens: Some(40),
            duration_ms: 1834,
        };

        let value = exec_stream_value(&event).expect("serializes");
        let json = serde_json::to_string(&value).expect("serializes");
        assert!(!json.contains('\n'));
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["type"], "turn_usage");
        assert_eq!(parsed["schema"], "codewhale.exec-stream");
        assert_eq!(parsed["schema_version"], 1);
        assert_eq!(parsed["turn"], 2);
        assert_eq!(parsed["input_tokens"], 1200);
        assert_eq!(parsed["output_tokens"], 180);
        assert_eq!(parsed["reasoning_tokens"], 90);
        assert_eq!(parsed["prompt_cache_hit_tokens"], 900);
        assert_eq!(parsed["prompt_cache_miss_tokens"], 300);
        assert_eq!(parsed["prompt_cache_write_tokens"], 0);
        assert_eq!(parsed["reasoning_replay_tokens"], 40);
        assert_eq!(parsed["duration_ms"], 1834);
    }

    #[test]
    fn exec_stream_turn_usage_event_omits_unreported_fields() {
        // Honest absence: optional token fields the provider did not report
        // are dropped from the object entirely — never emitted as null and
        // never backfilled with fabricated zeros.
        let event = ExecStreamEvent::TurnUsage {
            turn: 1,
            input_tokens: 11,
            output_tokens: 3,
            reasoning_tokens: None,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
            prompt_cache_write_tokens: None,
            reasoning_replay_tokens: None,
            duration_ms: 250,
        };

        let value = exec_stream_value(&event).expect("serializes");
        let parsed = value;
        assert_eq!(parsed["type"], "turn_usage");
        assert_eq!(parsed["input_tokens"], 11);
        assert_eq!(parsed["output_tokens"], 3);
        assert_eq!(parsed["duration_ms"], 250);
        let object = parsed.as_object().expect("event object");
        for absent in [
            "reasoning_tokens",
            "prompt_cache_hit_tokens",
            "prompt_cache_miss_tokens",
            "prompt_cache_write_tokens",
            "reasoning_replay_tokens",
        ] {
            assert!(!object.contains_key(absent), "{absent} leaked: {parsed}");
        }
    }

    #[test]
    fn exec_stream_pre_existing_event_type_tags_are_unchanged() {
        // Contract guard for existing stream consumers (bench harness, fleet
        // ledger): the pre-turn_usage event vocabulary keeps its exact tags.
        let cases: Vec<(ExecStreamEvent, &str)> = vec![
            (
                ExecStreamEvent::Content {
                    content: "hi".to_string(),
                },
                "content",
            ),
            (
                ExecStreamEvent::ToolUse {
                    name: "read_file".to_string(),
                    id: "call_1".to_string(),
                    input: serde_json::json!({}),
                    started_at: "2026-08-03T00:00:00Z".to_string(),
                },
                "tool_use",
            ),
            (
                ExecStreamEvent::ToolResult {
                    id: "call_1".to_string(),
                    name: "read_file".to_string(),
                    output: "ok".to_string(),
                    status: "success".to_string(),
                    started_at: "2026-08-03T00:00:00Z".to_string(),
                    completed_at: "2026-08-03T00:00:01Z".to_string(),
                    duration_ms: 1,
                    side_effect_status: "unknown".to_string(),
                    error_category: None,
                    truncated: None,
                    artifact: None,
                    result_metadata: None,
                },
                "tool_result",
            ),
            (
                ExecStreamEvent::SandboxDenied {
                    tool_id: "call_1".to_string(),
                    tool_name: "exec_shell".to_string(),
                    reason: "denied".to_string(),
                    outcome: "approval_required".to_string(),
                },
                "sandbox_denied",
            ),
            (
                ExecStreamEvent::WorkflowEvent {
                    run_id: "workflow_1".to_string(),
                    event: serde_json::json!({"type": "task_completed"}),
                },
                "workflow_event",
            ),
            (
                ExecStreamEvent::SessionCapture {
                    content: "x".to_string(),
                    saved_session_id: "session-x".to_string(),
                },
                "session_capture",
            ),
            (
                ExecStreamEvent::Error {
                    error: "boom".to_string(),
                },
                "error",
            ),
            (ExecStreamEvent::Done, "done"),
        ];

        for (event, expected_type) in cases {
            let value = exec_stream_value(&event).expect("serializes");
            assert_eq!(value["type"], expected_type, "event tag drifted");
            assert_eq!(value["schema"], "codewhale.exec-stream");
            assert_eq!(value["schema_version"], 1);
        }
    }

    #[test]
    fn exec_stream_events_are_json_lines() {
        let event = ExecStreamEvent::ToolResult {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            output: "line 1\nline 2".to_string(),
            status: "success".to_string(),
            started_at: "2026-07-13T00:00:00Z".to_string(),
            completed_at: "2026-07-13T00:00:01Z".to_string(),
            duration_ms: 1000,
            side_effect_status: "not_started".to_string(),
            error_category: None,
            truncated: Some(false),
            artifact: None,
            result_metadata: None,
        };

        let value = exec_stream_value(&event).expect("serializes");
        let json = serde_json::to_string(&value).expect("serializes");
        assert!(!json.contains('\n'));
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["type"], "tool_result");
        assert_eq!(parsed["schema"], "codewhale.exec-stream");
        assert_eq!(parsed["schema_version"], 1);
        assert_eq!(parsed["duration_ms"], 1000);
        assert_eq!(parsed["side_effect_status"], "not_started");
    }

    #[test]
    fn workflow_receipt_stream_event_is_one_json_line() {
        let event = ExecStreamEvent::WorkflowEvent {
            run_id: "workflow_1234".to_string(),
            event: serde_json::json!({
                "type": "handoff_promoted",
                "artifact_id": "workflow_1234:agent_1:review-gate:review_report",
                "gate_id": "review-gate",
                "kind": "review_report",
                "from_role": "reviewer",
                "to_role": "verifier",
                "producer_task_id": "agent_1"
            }),
        };

        let value = exec_stream_value(&event).expect("serializes");
        let json = serde_json::to_string(&value).expect("serializes");
        assert!(!json.contains('\n'));
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["type"], "workflow_event");
        assert_eq!(parsed["schema"], "codewhale.exec-stream");
        assert_eq!(parsed["schema_version"], 1);
        assert_eq!(parsed["run_id"], "workflow_1234");
        assert_eq!(parsed["event"]["type"], "handoff_promoted");
        assert_eq!(
            parsed["event"]["artifact_id"],
            "workflow_1234:agent_1:review-gate:review_report"
        );
        assert_eq!(parsed["event"]["gate_id"], "review-gate");
        assert_eq!(parsed["event"]["kind"], "review_report");
        assert_eq!(parsed["event"]["from_role"], "reviewer");
        assert_eq!(parsed["event"]["to_role"], "verifier");
        assert_eq!(parsed["event"]["producer_task_id"], "agent_1");
        assert!(parsed["event"].get("payload").is_none(), "{parsed}");

        let consumed = ExecStreamEvent::WorkflowEvent {
            run_id: "workflow_1234".to_string(),
            event: serde_json::json!({
                "type": "handoff_consumed",
                "artifact_id": "workflow_1234:agent_1:review-gate:review_report",
                "kind": "review_report",
                "from_role": "reviewer",
                "to_role": "verifier",
                "consumer_task_id": "agent_2"
            }),
        };
        let consumed = exec_stream_value(&consumed).expect("serializes consumed receipt");
        assert_eq!(consumed["type"], "workflow_event");
        assert_eq!(consumed["schema"], "codewhale.exec-stream");
        assert_eq!(consumed["schema_version"], 1);
        assert_eq!(consumed["event"]["type"], "handoff_consumed");
        assert_eq!(
            consumed["event"]["artifact_id"],
            "workflow_1234:agent_1:review-gate:review_report"
        );
        assert_eq!(consumed["event"]["consumer_task_id"], "agent_2");
        assert!(consumed["event"].get("payload").is_none(), "{consumed}");
    }

    #[test]
    fn exec_stream_metadata_redacts_resume_breadcrumbs() {
        let raw_session_id = "abc123fullsecret";
        let event = ExecStreamEvent::Metadata {
            meta: Box::new(ExecStreamMeta {
                receipt_kind: "terminal",
                provider: "deepseek".to_string(),
                provider_id: None,
                model: "deepseek-v4-flash".to_string(),
                route_source: "explicit_or_configured".to_string(),
                input_tokens: Some(123),
                output_tokens: Some(45),
                prompt_cache_hit_tokens: Some(10),
                prompt_cache_miss_tokens: None,
                prompt_cache_write_tokens: None,
                reasoning_tokens: Some(3),
                codewhale_max_output_tokens: Some(384_000),
                codewhale_max_output_tokens_source: Some("documented"),
                duration_ms: 2500,
                retry_count: None,
                approval_posture: "ask".to_string(),
                sandbox_posture: "configured_default".to_string(),
                binary_sha256: Some("sha256:binary".to_string()),
                config_sha256: None,
                prompt_sha256: "sha256:prompt".to_string(),
                tool_catalog_sha256: Some("sha256:tools".to_string()),
                input_analysis: ExecStreamInputAnalysis::default(),
                visible_final_answer_chars: 17,
                visible_final_answer_excerpt: "the visible reply".to_string(),
                session_id: exec_stream_session_ref(raw_session_id),
                resume_command: exec_stream_resume_hint(raw_session_id),
                workspace: "/tmp/work".to_string(),
                message_count: 4,
                status: Some("completed".to_string()),
                termination_reason: Some("resolved".to_string()),
                error_category: None,
                error: None,
            }),
        };

        let json = serde_json::to_string(&event).expect("serializes");
        assert!(!json.contains('\n'));
        assert!(!json.contains(raw_session_id));
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["type"], "metadata");
        assert_ne!(parsed["meta"]["session_id"], raw_session_id);
        assert!(
            parsed["meta"]["session_id"]
                .as_str()
                .unwrap()
                .starts_with("<redacted:")
        );
        assert_eq!(
            parsed["meta"]["resume_command"],
            "codewhale exec --resume <session_capture.saved_session_id>"
        );
        assert_eq!(parsed["meta"]["workspace"], "/tmp/work");
        assert_eq!(parsed["meta"]["message_count"], 4);
        assert_eq!(parsed["meta"]["visible_final_answer_chars"], 17);
        assert_eq!(
            parsed["meta"]["visible_final_answer_excerpt"],
            "the visible reply"
        );

        // Contract (#5946): the raw saved-session id is carried by exactly one
        // field, `session_capture.saved_session_id`. The `metadata` receipt
        // above stays fingerprint-only, and the capture's own `content` keeps
        // the same fingerprint so both surfaces can be correlated in a log.
        let capture = ExecStreamEvent::SessionCapture {
            content: exec_stream_session_ref(raw_session_id),
            saved_session_id: raw_session_id.to_string(),
        };
        let capture_json = serde_json::to_string(&capture).expect("serializes");
        let parsed_capture: serde_json::Value =
            serde_json::from_str(&capture_json).expect("valid json");
        assert_eq!(parsed_capture["type"], "session_capture");
        assert_eq!(parsed_capture["content"], parsed["meta"]["session_id"]);
        assert_ne!(parsed_capture["content"], raw_session_id);
        assert_eq!(parsed_capture["saved_session_id"], raw_session_id);
        assert!(parsed_capture.get("session_id").is_none(), "{capture_json}");
    }

    #[test]
    fn exec_stream_final_answer_excerpt_is_bounded_and_redacted() {
        assert_eq!(
            exec_stream_final_answer_excerpt("  short reply \n"),
            "short reply"
        );
        let long = "x".repeat(crate::fleet::executor::EXEC_STREAM_FINAL_ANSWER_EXCERPT_CHARS + 5);
        let excerpt = exec_stream_final_answer_excerpt(&long);
        assert_eq!(
            excerpt.chars().count(),
            crate::fleet::executor::EXEC_STREAM_FINAL_ANSWER_EXCERPT_CHARS + 3
        );
        assert!(excerpt.ends_with("..."));
        let leaked = exec_stream_final_answer_excerpt("token: sk-ant-must-not-leak-1234567890");
        assert!(!leaked.contains("sk-ant-must-not-leak"), "{leaked}");
    }

    #[test]
    fn exec_stream_final_answer_text_is_the_last_assistant_reply() {
        // Multi-step turn: pre-tool commentary, a tool result, then a
        // distinct final answer. The receipt must carry only the final
        // reply, not the cumulative stream output.
        let messages = vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "write the report".to_string(),
                    cache_control: None,
                }],
            },
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "let me check the workspace first".to_string(),
                    cache_control: None,
                }],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    execution_id: None,
                    tool_use_id: "call-1".to_string(),
                    content: "listed files".to_string(),
                    is_error: Some(false),
                    content_blocks: None,
                }],
            },
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::thinking("final reasoning"),
                    ContentBlock::Text {
                        text: "the final report".to_string(),
                        cache_control: None,
                    },
                ],
            },
        ];
        assert_eq!(
            exec_stream_final_answer_text(&messages, true),
            Some("the final report".to_string())
        );
    }

    #[test]
    fn exec_stream_final_answer_text_never_reuses_a_resumed_turn_reply() {
        let mut messages = vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "old prompt".into(),
                    cache_control: None,
                }],
            },
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "old reply must not be delivered again".into(),
                    cache_control: None,
                }],
            },
        ];
        // The synchronization event can precede acceptance of the new prompt.
        assert_eq!(exec_stream_final_answer_text(&messages, false), None);
        messages.push(Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "new prompt that fails before an answer".into(),
                cache_control: None,
            }],
        });
        assert_eq!(exec_stream_final_answer_text(&messages, true), None);
        messages.push(Message {
            role: Role::InterruptedAssistant,
            content: vec![ContentBlock::Text {
                text: "current partial reply".into(),
                cache_control: None,
            }],
        });
        assert_eq!(
            exec_stream_final_answer_text(&messages, true).as_deref(),
            Some("current partial reply")
        );
        // Tool results are user-role records inside this same turn.
        messages.push(Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                execution_id: None,
                tool_use_id: "call-current".into(),
                content: "result".into(),
                is_error: None,
                content_blocks: None,
            }],
        });
        assert_eq!(
            exec_stream_final_answer_text(&messages, true).as_deref(),
            Some("current partial reply")
        );
    }

    #[test]
    fn exec_stream_final_answer_text_requires_assistant_text() {
        assert_eq!(exec_stream_final_answer_text(&[], true), None);
        let user_only = vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "prompt".to_string(),
                cache_control: None,
            }],
        }];
        assert_eq!(exec_stream_final_answer_text(&user_only, true), None);
        let textless_assistant = vec![Message {
            role: Role::Assistant,
            content: vec![ContentBlock::thinking("reasoning only")],
        }];
        assert_eq!(
            exec_stream_final_answer_text(&textless_assistant, true),
            None
        );
    }

    #[test]
    fn exec_stream_input_analysis_reports_prompt_composition() {
        let system = SystemPrompt::Text("system rules".to_string());
        let messages = vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "run tests".to_string(),
                    cache_control: None,
                }],
            },
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::thinking("checking context"),
                    ContentBlock::Text {
                        text: "working".to_string(),
                        cache_control: None,
                    },
                    ContentBlock::ToolUse {
                        execution_id: None,
                        id: "call-1".to_string(),
                        name: "exec_shell".to_string(),
                        input: serde_json::json!({"command": "cargo test"}),
                        caller: None,
                        thought_signature: None,
                    },
                ],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    execution_id: None,
                    tool_use_id: "call-1".to_string(),
                    content: "stdout line\nstderr line".to_string(),
                    is_error: Some(false),
                    content_blocks: Some(vec![serde_json::json!({
                        "type": "text",
                        "text": "structured output"
                    })]),
                }],
            },
        ];

        let analysis = exec_stream_input_analysis(&messages, Some(&system));

        assert_eq!(analysis.user_message_count, 2);
        assert_eq!(analysis.assistant_message_count, 1);
        assert_eq!(analysis.tool_message_count, 0);
        assert_eq!(analysis.tool_use_count, 1);
        assert_eq!(analysis.tool_result_count, 1);
        assert_eq!(analysis.thinking_chars, "checking context".chars().count());
        assert!(analysis.text_chars >= "run testsworking".chars().count());
        assert!(analysis.tool_use_input_chars > 0);
        assert!(analysis.tool_result_chars >= "stdout line\nstderr line".chars().count());
        assert!(analysis.estimated_system_tokens > 0);
        assert!(analysis.estimated_message_content_tokens > 0);
        assert!(
            analysis.estimated_request_tokens
                >= analysis.estimated_system_tokens
                    + analysis.estimated_message_content_tokens
                    + analysis.estimated_framing_tokens
        );
    }

    #[test]
    fn review_receipt_check_public_json_omits_private_details() {
        let validation = crate::tools::review::ReviewReceiptValidation {
            passed: false,
            reason: "secret reason with /tmp/private/receipt.json".to_string(),
            diff_fingerprint: "sha256:current".to_string(),
            receipt_fingerprint: Some("sha256:current".to_string()),
            receipt_path: Some(PathBuf::from("/tmp/private/receipt.json")),
            unresolved_risk: Some(crate::tools::review::ReviewReceiptRisk {
                unresolved: true,
                level: "error".to_string(),
                summary: "secret summary".to_string(),
            }),
        };

        let public = review_receipt_validation_public_json(&validation);
        let encoded = serde_json::to_string(&public).expect("public json");

        assert_eq!(public["passed"], false);
        assert_eq!(public["status"], "unresolved_risk");
        assert_eq!(public["risk_level"], "error");
        assert!(!encoded.contains("secret"));
        assert!(!encoded.contains("/tmp/private"));
    }

    #[test]
    fn exec_text_session_breadcrumbs_use_compact_ids() {
        let session_id = "1234567890abcdef";

        assert_eq!(exec_saved_session_line(session_id), "session: 12345678");
        assert_eq!(
            exec_resumed_session_line(session_id),
            "resumed session: 12345678"
        );
        assert!(!exec_saved_session_line(session_id).contains(session_id));
        assert!(!exec_resumed_session_line(session_id).contains(session_id));
    }

    #[test]
    fn alternate_screen_defaults_on_in_auto_mode() {
        let cli = parse_cli(&["codewhale"]);
        let config = Config::default();

        assert_eq!(startup_screen_mode(&cli, &config), ScreenMode::Fullscreen);
    }

    #[test]
    fn screen_mode_round_trips_through_its_own_vocabulary() {
        for mode in [ScreenMode::Fullscreen, ScreenMode::Inline] {
            assert_eq!(ScreenMode::parse(mode.as_str()), Some(mode));
        }
        // The `tui.alternate_screen` words the config file already documents.
        assert_eq!(ScreenMode::parse("auto"), Some(ScreenMode::Fullscreen));
        assert_eq!(ScreenMode::parse("always"), Some(ScreenMode::Fullscreen));
        assert_eq!(ScreenMode::parse("never"), Some(ScreenMode::Inline));
        assert_eq!(ScreenMode::parse("  INLINE "), Some(ScreenMode::Inline));
        assert_eq!(ScreenMode::parse("sideways"), None);
        assert!(ScreenMode::Fullscreen.uses_alt_screen());
        assert!(!ScreenMode::Inline.uses_alt_screen());
    }

    #[test]
    fn removed_no_alt_screen_flag_is_rejected() {
        // Negative test: the retired compatibility flag must not be silently
        // accepted and must not reach the alternate-screen decision at all.
        let error = Cli::try_parse_from(["codewhale", "--no-alt-screen"])
            .expect_err("--no-alt-screen must no longer parse");
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::UnknownArgument,
            "retired flag should fail as an unknown argument, not be absorbed"
        );
    }

    #[test]
    fn config_never_selects_the_inline_screen() {
        let cli = parse_cli(&["codewhale"]);
        let config = Config {
            tui: Some(crate::config::TuiConfig {
                alternate_screen: Some("never".to_string()),
                mouse_capture: None,
                selection_copy_markdown: None,
                stream_chunk_timeout_secs: None,
                max_model_steps: None,
                turn_wall_clock_secs: None,
                stream_max_content_mb: None,
                stream_max_duration_secs: None,
                stream_max_resumes: None,
                stream_max_transparent_retries: None,
                stream_max_errors: None,
                stream_open_timeout_secs: None,
                connect_timeout_secs: None,
                force_http1: None,
                status_items: None,
                posture_bar: None,
                metrics_line: None,
                osc8_links: None,
                composer_arrows_scroll: None,
                notification_condition: None,
            }),
            ..Config::default()
        };

        assert_eq!(startup_screen_mode(&cli, &config), ScreenMode::Inline);
    }

    #[test]
    #[cfg(not(windows))]
    fn mouse_capture_defaults_on_when_alternate_screen_is_active() {
        let cli = parse_cli(&["codewhale"]);
        let config = Config::default();

        assert!(should_use_mouse_capture_with(
            &cli, &config, true, None, None, None
        ));
    }

    #[test]
    #[cfg(windows)]
    fn mouse_capture_defaults_off_on_legacy_windows_console() {
        // Legacy conhost (no `WT_SESSION` and no `ConEmuPID`) keeps the
        // v0.8.x default-off behavior: mouse-mode reporting on legacy console
        // can leak SGR escapes into the composer.
        let cli = parse_cli(&["codewhale"]);
        let config = Config::default();

        assert!(!should_use_mouse_capture_with(
            &cli, &config, true, None, None, None
        ));
    }

    // #1169: Windows Terminal sets `WT_SESSION` and handles mouse-mode
    // reporting cleanly, so default-on there gives users in-app text
    // selection (and the side-effect of clamping selection to the
    // transcript region instead of the terminal painting across the
    // sidebar via native selection).
    #[test]
    #[cfg(windows)]
    fn mouse_capture_defaults_on_in_windows_terminal() {
        let cli = parse_cli(&["codewhale"]);
        let config = Config::default();

        assert!(should_use_mouse_capture_with(
            &cli,
            &config,
            true,
            None,
            Some("{a3a3b3a8-aa00-0000-0000-000000000000}"),
            None,
        ));
    }

    // ConEmu/Cmder sets `ConEmuPID` and handles VT mouse-mode reporting
    // cleanly; default mouse capture on there so users get in-app scrolling.
    #[test]
    #[cfg(windows)]
    fn mouse_capture_defaults_on_in_conemu() {
        let cli = parse_cli(&["codewhale"]);
        let config = Config::default();

        assert!(should_use_mouse_capture_with(
            &cli,
            &config,
            true,
            None,
            None,
            Some("12345"),
        ));
    }

    #[test]
    fn no_mouse_capture_flag_disables_mouse_capture() {
        let cli = parse_cli(&["codewhale", "--no-mouse-capture"]);
        let config = Config::default();

        assert!(!should_use_mouse_capture_with(
            &cli, &config, true, None, None, None
        ));
    }

    #[test]
    fn config_can_disable_default_mouse_capture() {
        let cli = parse_cli(&["codewhale"]);
        let config = Config {
            tui: Some(crate::config::TuiConfig {
                alternate_screen: None,
                mouse_capture: Some(false),
                selection_copy_markdown: None,
                stream_chunk_timeout_secs: None,
                max_model_steps: None,
                turn_wall_clock_secs: None,
                stream_max_content_mb: None,
                stream_max_duration_secs: None,
                stream_max_resumes: None,
                stream_max_transparent_retries: None,
                stream_max_errors: None,
                stream_open_timeout_secs: None,
                connect_timeout_secs: None,
                force_http1: None,
                status_items: None,
                posture_bar: None,
                metrics_line: None,
                osc8_links: None,
                composer_arrows_scroll: None,
                notification_condition: None,
            }),
            ..Config::default()
        };

        assert!(!should_use_mouse_capture_with(
            &cli, &config, true, None, None, None
        ));
    }

    #[test]
    fn mouse_capture_flag_enables_mouse_capture() {
        let cli = parse_cli(&["codewhale", "--mouse-capture"]);
        let config = Config::default();

        assert!(should_use_mouse_capture_with(
            &cli, &config, true, None, None, None
        ));
    }

    #[test]
    fn config_can_enable_mouse_capture() {
        let cli = parse_cli(&["codewhale"]);
        let config = Config {
            tui: Some(crate::config::TuiConfig {
                alternate_screen: None,
                mouse_capture: Some(true),
                selection_copy_markdown: None,
                stream_chunk_timeout_secs: None,
                max_model_steps: None,
                turn_wall_clock_secs: None,
                stream_max_content_mb: None,
                stream_max_duration_secs: None,
                stream_max_resumes: None,
                stream_max_transparent_retries: None,
                stream_max_errors: None,
                stream_open_timeout_secs: None,
                connect_timeout_secs: None,
                force_http1: None,
                status_items: None,
                posture_bar: None,
                metrics_line: None,
                osc8_links: None,
                composer_arrows_scroll: None,
                notification_condition: None,
            }),
            ..Config::default()
        };

        assert!(should_use_mouse_capture_with(
            &cli, &config, true, None, None, None
        ));
    }

    #[test]
    fn mouse_capture_is_off_without_alternate_screen() {
        let cli = parse_cli(&["codewhale", "--mouse-capture"]);
        let config = Config::default();

        assert!(!should_use_mouse_capture_with(
            &cli, &config, false, None, None, None
        ));
    }

    // Issue #878 / #898: JetBrains JediTerm advertises mouse support but
    // forwards SGR mouse-event escapes as raw input characters, producing
    // the "input box auto-fills with garbled characters when I move the
    // mouse" failure mode in PyCharm/IDEA terminals. Default the capture
    // off when we see TERMINAL_EMULATOR=JetBrains-JediTerm; explicit
    // config / --mouse-capture still wins.

    #[test]
    fn mouse_capture_defaults_off_in_jetbrains_jediterm() {
        let cli = parse_cli(&["codewhale"]);
        let config = Config::default();

        assert!(!should_use_mouse_capture_with(
            &cli,
            &config,
            true,
            Some("JetBrains-JediTerm"),
            None,
            None,
        ));
    }

    #[test]
    fn jetbrains_default_off_is_case_insensitive() {
        let cli = parse_cli(&["codewhale"]);
        let config = Config::default();

        // JetBrains has occasionally varied the casing across releases;
        // a case-insensitive match keeps the protection in place.
        assert!(!should_use_mouse_capture_with(
            &cli,
            &config,
            true,
            Some("jetbrains-jediterm"),
            None,
            None,
        ));
    }

    #[test]
    fn mouse_capture_flag_overrides_jetbrains_default() {
        let cli = parse_cli(&["codewhale", "--mouse-capture"]);
        let config = Config::default();

        assert!(should_use_mouse_capture_with(
            &cli,
            &config,
            true,
            Some("JetBrains-JediTerm"),
            None,
            None,
        ));
    }

    #[test]
    fn config_mouse_capture_true_overrides_jetbrains_default() {
        let cli = parse_cli(&["codewhale"]);
        let config = Config {
            tui: Some(crate::config::TuiConfig {
                alternate_screen: None,
                mouse_capture: Some(true),
                selection_copy_markdown: None,
                stream_chunk_timeout_secs: None,
                max_model_steps: None,
                turn_wall_clock_secs: None,
                stream_max_content_mb: None,
                stream_max_duration_secs: None,
                stream_max_resumes: None,
                stream_max_transparent_retries: None,
                stream_max_errors: None,
                stream_open_timeout_secs: None,
                connect_timeout_secs: None,
                force_http1: None,
                status_items: None,
                posture_bar: None,
                metrics_line: None,
                osc8_links: None,
                composer_arrows_scroll: None,
                notification_condition: None,
            }),
            ..Config::default()
        };

        assert!(should_use_mouse_capture_with(
            &cli,
            &config,
            true,
            Some("JetBrains-JediTerm"),
            None,
            None,
        ));
    }

    #[test]
    fn cli_defaults_doctor_offline_and_keeps_json_incompatible_with_live_flags() {
        let cli =
            crate::Cli::try_parse_from(["codewhale-tui", "doctor"]).expect("parse default doctor");
        let Some(crate::Commands::Doctor(args)) = cli.command else {
            panic!("expected doctor command");
        };
        assert!(!args.check_updates);
        assert!(!args.probe_api);
        assert!(!args.probe_local);
        assert!(!args.probe_mcp);
        assert!(!args.probe_search);

        let cli = crate::Cli::try_parse_from(["codewhale-tui", "doctor", "--probe-search"])
            .expect("parse search probe");
        let Some(crate::Commands::Doctor(args)) = cli.command else {
            panic!("expected doctor command");
        };
        assert!(args.probe_search);
        assert!(!args.probe_api);
        assert!(!args.probe_local);
        assert!(!args.probe_mcp);

        for output_flag in ["--json", "--context-json"] {
            for live_flag in [
                "--check-updates",
                "--probe-api",
                "--probe-local",
                "--probe-mcp",
                "--probe-search",
            ] {
                assert!(
                    crate::Cli::try_parse_from(["codewhale-tui", "doctor", output_flag, live_flag,])
                        .is_err(),
                    "{output_flag} unexpectedly accepted live flag {live_flag}"
                );
            }
        }
    }

    #[test]
    fn default_max_turns_does_not_add_a_hidden_worker_cap() {
        use crate::fleet::executor::build_worker_exec_command_with_launch_spec;
        use crate::fleet::worker_runtime::fleet_task_to_worker_spec_with_profiles;
        use codewhale_config::FleetExecConfig;
        use codewhale_protocol::fleet::{FleetHostSpec, FleetTaskSpec, FleetWorkerSpec};

        let exec = FleetExecConfig::default();
        let workspace = tempfile::TempDir::new().unwrap();
        let worker = FleetWorkerSpec {
            id: "worker-1".to_string(),
            name: "Worker 1".to_string(),
            host: FleetHostSpec::Local,
            trust_level: None,
            labels: std::collections::BTreeMap::new(),
            capabilities: Vec::new(),
            max_concurrent_tasks: Some(1),
        };
        for requested_steps in [None, Some(0), Some(13)] {
            let mut task = serde_json::json!({
                "id": "t1",
                "name": "Smoke",
                "objective": "prove it runs",
                "instructions": "x",
                "worker": { "role": "reviewer", "tool_profile": "read-only" }
            });
            if let Some(max_steps) = requested_steps {
                task["budget"] = serde_json::json!({ "max_steps": max_steps });
            }
            let task: FleetTaskSpec = serde_json::from_value(task).unwrap();
            let spec = fleet_task_to_worker_spec_with_profiles(
                "worker-1",
                "run-1",
                &task,
                &worker,
                "auto",
                workspace.path(),
                workspace.path(),
                &[],
                None,
            )
            .unwrap();
            let cmd = build_worker_exec_command_with_launch_spec(
                "codewhale",
                &task,
                &spec,
                &exec,
                None,
                &[],
            )
            .expect("production worker command");
            let cli = Cli::try_parse_from(
                std::iter::once("codewhale").chain(cmd.args.iter().map(String::as_str)),
            )
            .expect("production worker args must parse");
            let Some(Commands::Exec(args)) = cli.command else {
                panic!("expected worker exec command");
            };
            let expected = requested_steps.filter(|steps| *steps > 0);
            assert_eq!(args.max_turns, expected);
            assert_eq!(args.max_tool_calls, None);
            // Follow omission beyond argv through the real CLI resolver. This
            // previously installed 200 despite correct unbounded launch args.
            let turn = crate::core::turn::TurnContext::new(exec_max_steps(args.max_turns));
            assert_eq!(turn.step_limit(), expected);
            assert_eq!(turn.stop_diagnostics.effective_max_steps, expected);
        }
    }
