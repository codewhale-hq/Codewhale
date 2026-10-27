
    #[test]
    fn review_failure_after_publication_preserves_all_usage_and_post_state() {
        let mut usage = codewhale_models::Usage::default();
        for response_usage in [
            codewhale_models::Usage {
                input_tokens: 21,
                output_tokens: 5,
                reasoning_tokens: Some(2),
                ..Default::default()
            },
            codewhale_models::Usage {
                input_tokens: 34,
                output_tokens: 8,
                reasoning_tokens: Some(3),
                ..Default::default()
            },
        ] {
            crate::tools::review::add_review_usage(&mut usage, &response_usage);
        }

        for (publication, message) in [
            (
                ReviewPublication::Uncertain,
                "PR review publication failed after request dispatch",
            ),
            (
                ReviewPublication::Posted,
                "review posted but receipt write failed",
            ),
        ] {
            let payload = review_failure_payload(
                "fixture-provider",
                "fixture-model",
                &usage,
                2,
                2,
                publication,
                message,
            );
            assert_eq!(payload["success"], false);
            assert_eq!(payload["complete"], false);
            assert_eq!(payload["publication"], publication.as_str());
            assert_eq!(payload["usage"]["input_tokens"], 55);
            assert_eq!(payload["usage"]["output_tokens"], 13);
            assert_eq!(payload["usage"]["reasoning_tokens"], 5);
            assert!(payload.get("review").is_none());
            assert!(payload.get("receipt").is_none());
        }
    }

    #[tokio::test]
    async fn review_provider_flag_pins_route_for_multi_route_model() {
        // A genuinely multi-route model: `shared-review-model` is the
        // configured model of BOTH named routes below, while the active route
        // (custom-a) does not offer it. (Two custom providers can never be
        // ambiguous — the route inventory resolves only the active custom
        // entry — so the multi-route pair has to be named providers.)
        // Without `--provider`, an explicit `--model` lets cross-provider
        // inventory inference run, and a model offered by more than one
        // configured route hard-errors in `resolve_cli_auto_route`
        // ("available from configured provider route(s): ..."). That error
        // already tells the user to "Pass `--provider <provider>`" — until
        // now `codewhale review` had no such flag to pass.
        let mut config = custom_exec_config("custom-a");
        {
            let providers = config.providers.as_mut().expect("providers");
            for entry in [&mut providers.deepseek, &mut providers.openrouter] {
                *entry = codewhale_runtime::config::ProviderConfig {
                    model: Some("shared-review-model".to_string()),
                    api_key: Some("local-test-key".to_string()),
                    ..Default::default()
                };
            }
        }

        let inferred = review_args(&["codewhale", "review", "--model", "shared-review-model"]);
        let (inferred_config, inferred_force) =
            review_execution_route(&config, &inferred).expect("model-only route");
        assert!(
            !inferred_force,
            "an explicit model with no provider stays open to inventory inference"
        );
        assert_eq!(inferred_config.provider.as_deref(), Some("custom-a"));

        // Without the flag the multi-route model must refuse to guess.
        let err = resolve_cli_exec_route(
            &inferred_config,
            &resolve_review_model(&inferred_config, inferred.model.as_deref()),
            "review diff",
            inferred_force,
        )
        .await
        .expect_err("a model offered by two configured routes must hard-error");
        let message = err.to_string();
        assert!(
            message.contains("available from configured provider route(s)"),
            "unexpected error: {message}"
        );
        assert!(
            message.contains("deepseek") && message.contains("openrouter"),
            "both candidate routes must be named: {message}"
        );

        // With the flag the same model pins to the named route and resolves.
        let pinned = review_args(&[
            "codewhale",
            "review",
            "--provider",
            "deepseek",
            "--model",
            "shared-review-model",
        ]);
        let (pinned_config, pinned_force) =
            review_execution_route(&config, &pinned).expect("pinned route");
        assert!(pinned_force, "--provider makes the route authoritative");
        assert_eq!(pinned_config.provider.as_deref(), Some("deepseek"));

        let route = resolve_cli_exec_route(
            &pinned_config,
            &resolve_review_model(&pinned_config, pinned.model.as_deref()),
            "review diff",
            pinned_force,
        )
        .await
        .expect("pinned review route");
        let execution = config_for_cli_route(&pinned_config, &route).expect("admitted execution route");

        assert_eq!(route.provider.provider, codewhale_config::ProviderKind::Deepseek);
        assert_eq!(route.model, "shared-review-model");
        assert_eq!(
            execution.active_provider_identity().unwrap().key.as_str(),
            "deepseek",
            "the review runs on the provider the flag named"
        );

        // Pinning a configured custom provider (the workflow's
        // CODEWHALE_REVIEW_PROVIDER="my-proxy" case) is equally authoritative
        // for the same multi-route model.
        let pinned_custom = review_args(&[
            "codewhale",
            "review",
            "--provider",
            "custom-b",
            "--model",
            "shared-review-model",
        ]);
        let (custom_config, custom_force) =
            review_execution_route(&config, &pinned_custom).expect("pinned custom route");
        assert!(custom_force);
        let custom_route = resolve_cli_exec_route(
            &custom_config,
            &resolve_review_model(&custom_config, pinned_custom.model.as_deref()),
            "review diff",
            custom_force,
        )
        .await
        .expect("pinned custom review route");
        let custom_execution = config_for_cli_route(&custom_config, &custom_route).expect("admitted execution route");

        assert_eq!(custom_route.provider.provider, codewhale_config::ProviderKind::Custom);
        assert_eq!(custom_route.model, "shared-review-model");
        assert_eq!(
            custom_execution
                .active_provider_identity()
                .unwrap()
                .key
                .as_str(),
            "custom-b",
            "the review runs on the custom provider the flag named"
        );
    }

    #[test]
    fn review_provider_flag_rejects_unknown_provider() {
        let config = custom_exec_config("custom-a");
        let args = review_args(&["codewhale", "review", "--provider", "not-a-provider"]);

        let err = review_execution_route(&config, &args)
            .expect_err("unknown provider must fail before any diff is fetched");
        assert!(
            err.to_string().contains("Unrecognized --provider"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn review_without_provider_flag_keeps_configured_route_authoritative() {
        let config = custom_exec_config("custom-a");
        let args = review_args(&["codewhale", "review"]);

        let (resolved, force) = review_execution_route(&config, &args).expect("default route");
        assert!(
            force,
            "the configured/default review route is authoritative"
        );
        assert_eq!(resolved.provider.as_deref(), Some("custom-a"));
    }

    fn review_issue(
        severity: &str,
        path: Option<&str>,
        line: Option<u32>,
    ) -> crate::tools::review::ReviewIssue {
        crate::tools::review::ReviewIssue {
            severity: severity.to_string(),
            title: format!("{severity} finding"),
            description: "detail".to_string(),
            path: path.map(str::to_string),
            line,
            ..Default::default()
        }
    }

    fn review_suggestion(
        path: Option<&str>,
        line: Option<u32>,
        replacement: Option<&str>,
    ) -> crate::tools::review::ReviewSuggestion {
        crate::tools::review::ReviewSuggestion {
            path: path.map(str::to_string),
            line,
            start_line: None,
            end_line: None,
            suggestion: "Use the checked variant".to_string(),
            replacement: replacement.map(str::to_string),
        }
    }

    fn review_with(
        issues: Vec<crate::tools::review::ReviewIssue>,
        suggestions: Vec<crate::tools::review::ReviewSuggestion>,
    ) -> crate::tools::review::ReviewOutput {
        crate::tools::review::ReviewOutput {
            summary: "summary".to_string(),
            issues,
            suggestions,
            ..Default::default()
        }
    }

    /// Two hunks in one file: right lines 10..=12 and 40..=41.
    ///
    /// Built from a slice, not one string literal: a `\` continuation strips
    /// the leading space that marks a context line.
    fn review_test_diff() -> String {
        [
            "diff --git a/crates/tui/src/lib.rs b/crates/tui/src/lib.rs",
            "index 1111111..2222222 100644",
            "--- a/crates/tui/src/lib.rs",
            "+++ b/crates/tui/src/lib.rs",
            "@@ -10,2 +10,3 @@ fn head() {",
            " let a = 1;",
            "+let b = a.unwrap();",
            " let c = 2;",
            "@@ -40,2 +40,2 @@ fn tail() {",
            " let d = 3;",
            "-let e = d;",
            "+let e = d + 1;",
            "",
        ]
        .join("\n")
    }

    #[cfg(unix)]
    #[test]
    fn local_review_collects_raw_changes_without_running_diff_helpers() {
        use std::os::unix::fs::PermissionsExt;

        for helper_kind in ["textconv", "external", "clean", "process"] {
            for mode in ["working", "staged", "base", "path"] {
                let workspace = tempfile::tempdir().unwrap();
                let path = workspace.path();
                let git = |args: &[&str]| {
                    let output = codewhale_dependencies::Git::command()
                        .expect("git")
                        .current_dir(path)
                        .args([
                            "-c",
                            "core.hooksPath=/dev/null",
                            "-c",
                            "commit.gpgSign=false",
                        ])
                        .args(args)
                        .output()
                        .unwrap();
                    assert!(
                        output.status.success(),
                        "{args:?}: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                    String::from_utf8(output.stdout).unwrap()
                };
                git(&["init", "-q"]);
                git(&["config", "user.name", "Review fixture"]);
                git(&["config", "user.email", "fixture@example.invalid"]);
                std::fs::write(
                    path.join(".gitattributes"),
                    "*.txt diff=fixture filter=fixture=odd\n",
                )
                .unwrap();
                for name in ["- source.txt", "other.txt"] {
                    std::fs::write(path.join(name), "before\n").unwrap();
                }
                std::fs::write(path.join("binary.dat"), b"\0before").unwrap();
                git(&["add", "."]);
                git(&["commit", "-qm", "before"]);
                for name in ["- source.txt", "other.txt"] {
                    std::fs::write(path.join(name), "after\n").unwrap();
                }
                std::fs::write(path.join("binary.dat"), b"\0after").unwrap();
                if matches!(mode, "staged" | "base") {
                    git(&["add", "."]);
                }
                if mode == "base" {
                    git(&["commit", "-qm", "after"]);
                }
                let script = match helper_kind {
                    "external" => "#!/bin/sh\nprintf touched > helper-marker\nprintf external-diff\n",
                    "clean" => "#!/bin/sh\nprintf touched > helper-marker\ncat\n",
                    "process" => "#!/bin/sh\nprintf touched > helper-marker\nexit 1\n",
                    _ => "#!/bin/sh\nprintf touched > helper-marker\ncat < \"$1\"\n",
                };
                let helper = path.join("helper.sh");
                std::fs::write(&helper, script).unwrap();
                std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
                git(&[
                    "config",
                    match helper_kind {
                        "external" => "diff.external",
                        "clean" => "filter.fixture=odd.clean",
                        "process" => "filter.fixture=odd.process",
                        _ => "diff.fixture.textconv",
                    },
                    "./helper.sh",
                ]);
                if matches!(helper_kind, "clean" | "process") {
                    git(&["config", "filter.fixture=odd.required", "true"]);
                }
                let (review_flags, diff_flags): (&[&str], &[&str]) = match mode {
                    "staged" => (&["--staged"], &["--cached"]),
                    "base" => (&["--base", "HEAD^"], &["HEAD^...HEAD"]),
                    "path" => (&["--path=- source.txt"], &["--", "- source.txt"]),
                    _ => (&[], &[]),
                };
                // Positive control: the same repository really can invoke its helper.
                let mut baseline = vec!["diff", "--ext-diff"];
                baseline.extend_from_slice(diff_flags);
                let baseline = codewhale_dependencies::Git::command()
                    .unwrap()
                    .current_dir(path)
                    .args(&baseline)
                    .output()
                    .unwrap();
                let marker = path.join("helper-marker");
                let conversion = matches!(helper_kind, "clean" | "process");
                let reads_worktree = matches!(mode, "working" | "path");
                assert_eq!(
                    marker.exists(),
                    !conversion || reads_worktree,
                    "positive control: {mode}, {helper_kind}"
                );
                assert_eq!(
                    baseline.status.success(),
                    helper_kind != "process" || !reads_worktree
                );
                if marker.exists() {
                    std::fs::remove_file(&marker).unwrap();
                }

                let mut argv = vec!["codewhale", "review"];
                argv.extend_from_slice(review_flags);
                let mut args = review_args(&argv);
                let diff = collect_diff(&args, None, path).unwrap();
                assert!(!marker.exists(), "{mode}, {helper_kind}");
                assert!(diff.contains("-before\n+after"), "{diff}");
                assert!(diff.contains("- source.txt"));
                if mode == "path" {
                    assert!(!diff.contains("other.txt"));
                    assert!(!diff.contains("binary.dat"));
                } else {
                    assert!(diff.contains("other.txt"));
                    assert!(diff.contains("Binary files") && diff.contains("binary.dat"));
                }
                args.max_chars = 1;
                assert!(
                    collect_diff(&args, None, path)
                        .unwrap_err()
                        .to_string()
                        .contains("No review was run")
                );
                assert!(!marker.exists());
            }
        }
    }

    #[test]
    fn local_review_budget_rejects_changes_beyond_a_shared_prefix() {
        let prefix = review_test_diff();
        let limit = prefix.chars().count();
        for tail in ["+safe_change();\n", "+dangerous_change();\n"] {
            let diff = format!("{prefix}{tail}");
            let error = ensure_local_review_diff_fits(&diff, limit).unwrap_err();
            assert!(error.to_string().contains("No review was run"));
            assert!(
                error
                    .to_string()
                    .contains("no receipt was written or accepted")
            );
        }
    }

    #[test]
    fn review_in_a_bare_repository_is_not_called_outside_git() {
        let bare = tempfile::tempdir().expect("tempdir");
        let init = std::process::Command::new("git")
            .args(["init", "--bare", "-q"])
            .current_dir(bare.path())
            .status()
            .expect("git init --bare");
        assert!(init.success());
        let error = ensure_review_workspace_is_git_repo(bare.path())
            .expect_err("a bare repository has no work tree");
        let text = error.to_string();
        assert!(text.starts_with("Not inside a git work tree"), "{text}");
    }

    #[test]
    fn review_outside_a_git_repository_says_so_in_one_line() {
        let outside = tempfile::tempdir().expect("tempdir");
        let error = ensure_review_workspace_is_git_repo(outside.path())
            .expect_err("a plain directory is not a work tree");
        assert_eq!(
            error.to_string(),
            format!(
                "Not inside a git repository (cwd: {})",
                outside.path().display()
            )
        );
        assert_eq!(
            first_stderr_line("\nfatal: bad revision 'nope...HEAD'\nusage: git diff\n  --stat\n"),
            "fatal: bad revision 'nope...HEAD'"
        );
    }

    #[test]
    fn exec_resume_error_redacts_the_typed_id_and_names_the_list_command() {
        let id = "sk-live-pasted-by-mistake";
        let text = exec_resume_load_error(id);
        assert!(!text.contains(id), "{text}");
        assert!(text.contains("<redacted:"), "{text}");
        assert!(
            text.ends_with("Run `codewhale sessions` to list ids."),
            "{text}"
        );
    }

    #[test]
    fn doctor_shows_plain_value_errors_with_a_fix_and_hides_the_rest() {
        let invalid = {
            let __cw_config_init_0_0 = Some("chatty".to_string());
            let mut __cw_config_init_0_value: codewhale_runtime::config::Config = Default::default();
            __cw_config_init_0_value.verbosity = __cw_config_init_0_0;
            __cw_config_init_0_value
        }
        .validate()
        .expect_err("unknown verbosity");
        let wrapped = invalid.context("Failed to load config file /tmp/config.toml");
        let text = doctor_config_error_text(&wrapped);
        assert_eq!(
            text,
            "doctor configuration validation failed: Invalid verbosity (value not shown): expected normal or concise.\nfix: codewhale config set verbosity normal (if a profile or managed config sets it, correct it there)"
        );
        assert!(!text.contains("chatty"), "{text}");

        let opaque = anyhow::anyhow!("TOML parse error near api_key = \"sk-live-secret\"");
        let text = doctor_config_error_text(&opaque);
        assert!(text.contains("details omitted"), "{text}");
        assert!(!text.contains("sk-live-secret"), "{text}");
    }

    #[test]
    fn local_review_budget_counts_unicode_characters_without_cutting_input() {
        let diff = format!("{}+鲸鱼\n", review_test_diff());
        let limit = diff.chars().count();
        assert!(diff.len() > limit);
        ensure_local_review_diff_fits(&diff, limit).unwrap();
        assert!(ensure_local_review_diff_fits(&diff, limit - 1).is_err());
    }

    #[test]
    fn inline_review_comments_keep_only_hunk_locatable_issues() {
        let review = review_with(
            vec![
                review_issue("error", Some("./crates/tui/src/lib.rs"), Some(11)),
                review_issue("warning", Some("docs/NOT_IN_DIFF.md"), Some(3)),
                review_issue("info", Some("crates/tui/src/lib.rs"), None),
                review_issue("error", None, Some(7)),
            ],
            Vec::new(),
        );
        let comments = plan_inline_review_comments(&review, &review_test_diff()).comments;
        // GitHub rejects the entire review (422) when a comment's path is not
        // part of the diff, and inline comments need a line to anchor to.
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0]["path"], "crates/tui/src/lib.rs");
        assert_eq!(comments[0]["line"], 11);
        assert_eq!(comments[0]["side"], "RIGHT");
    }

    #[test]
    fn inline_review_comments_drop_one_out_of_hunk_anchor_not_the_whole_review() {
        // Line 25 is inside the file but between the two hunks. Before the
        // hunk filter this single bad anchor 422'd the whole review request.
        let review = review_with(
            vec![
                review_issue("error", Some("crates/tui/src/lib.rs"), Some(11)),
                review_issue("error", Some("crates/tui/src/lib.rs"), Some(25)),
                review_issue("warning", Some("crates/tui/src/lib.rs"), Some(41)),
            ],
            Vec::new(),
        );
        let plan = plan_inline_review_comments(&review, &review_test_diff());
        let lines: Vec<_> = plan.comments.iter().map(|c| c["line"].clone()).collect();
        assert_eq!(lines, vec![serde_json::json!(11), serde_json::json!(41)]);
        // The loss is counted and reported, never silent.
        assert_eq!(plan.dropped_out_of_hunk, 1);
        assert_eq!(plan.dropped_untouched_file, 0);
        assert!(
            plan.receipt()
                .expect("receipt")
                .contains("no line inside a diff hunk")
        );
    }

    #[test]
    fn review_suggestion_with_replacement_posts_a_committable_block() {
        let review = review_with(
            Vec::new(),
            vec![review_suggestion(
                Some("crates/tui/src/lib.rs"),
                Some(11),
                Some("let b = a.unwrap_or_default();"),
            )],
        );
        let comments = plan_inline_review_comments(&review, &review_test_diff()).comments;
        assert_eq!(comments.len(), 1);
        let body = comments[0]["body"].as_str().expect("body");
        assert!(body.starts_with("Use the checked variant"), "{body}");
        assert!(
            body.contains("```suggestion\nlet b = a.unwrap_or_default();\n```"),
            "{body}"
        );
        assert_eq!(comments[0]["side"], "RIGHT");
        // Single-line suggestions must not carry start_line.
        assert!(comments[0].get("start_line").is_none());
    }

    #[test]
    fn multi_line_review_suggestion_spans_start_line_to_line() {
        let mut suggestion = review_suggestion(
            Some("crates/tui/src/lib.rs"),
            Some(12),
            Some("let b = a.unwrap_or_default();\nlet c = 2;"),
        );
        suggestion.start_line = Some(11);
        suggestion.end_line = Some(12);
        let review = review_with(Vec::new(), vec![suggestion]);
        let comments = plan_inline_review_comments(&review, &review_test_diff()).comments;
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0]["start_line"], 11);
        assert_eq!(comments[0]["start_side"], "RIGHT");
        assert_eq!(comments[0]["line"], 12);
        assert_eq!(comments[0]["side"], "RIGHT");
    }

    #[test]
    fn review_suggestion_without_replacement_degrades_to_prose() {
        // SAFETY: a committable suggestion is one click from being merged, so
        // a suggestion the model did not back with literal code must never
        // render as an applicable block.
        let review = review_with(
            Vec::new(),
            vec![review_suggestion(
                Some("crates/tui/src/lib.rs"),
                Some(11),
                None,
            )],
        );
        let plan = plan_inline_review_comments(&review, &review_test_diff());
        assert_eq!(plan.comments.len(), 1);
        let body = plan.comments[0]["body"].as_str().expect("body");
        assert_eq!(body, "Use the checked variant");
        assert!(!body.contains("```suggestion"), "{body}");
        assert_eq!(plan.degraded_to_prose, 1);
    }

    #[test]
    fn review_suggestion_spanning_outside_a_hunk_degrades_to_prose() {
        // Line 12 is in a hunk but line 13 is not, so the span cannot be
        // committed. Keep the finding, drop the one-click apply.
        let mut suggestion = review_suggestion(
            Some("crates/tui/src/lib.rs"),
            Some(12),
            Some("let c = 2;\nlet d = 3;"),
        );
        suggestion.start_line = Some(12);
        suggestion.end_line = Some(13);
        let review = review_with(Vec::new(), vec![suggestion]);
        let comments = plan_inline_review_comments(&review, &review_test_diff()).comments;
        // end_line 13 is outside every hunk, so there is no anchor at all.
        assert!(comments.is_empty());

        // Same replacement anchored at an in-hunk end line but with a start
        // line outside the hunk: anchor is valid, span is not -> prose.
        let mut suggestion = review_suggestion(
            Some("crates/tui/src/lib.rs"),
            Some(11),
            Some("let a = 1;\nlet b = a.unwrap_or_default();"),
        );
        suggestion.start_line = Some(9);
        suggestion.end_line = Some(11);
        let review = review_with(Vec::new(), vec![suggestion]);
        let comments = plan_inline_review_comments(&review, &review_test_diff()).comments;
        assert_eq!(comments.len(), 1);
        let body = comments[0]["body"].as_str().expect("body");
        assert!(!body.contains("```suggestion"), "{body}");
        assert!(comments[0].get("start_line").is_none());
    }

    #[test]
    fn review_suggestion_anchored_to_a_deleted_line_is_not_committable() {
        // Right line 41 is `+let e = d + 1;`; the deleted `-let e = d;` has no
        // RIGHT-side number, so a model that anchors at the pre-image line 42
        // gets nothing rather than a suggestion on the wrong line.
        let review = review_with(
            Vec::new(),
            vec![review_suggestion(
                Some("crates/tui/src/lib.rs"),
                Some(42),
                Some("let e = d + 2;"),
            )],
        );
        assert!(
            plan_inline_review_comments(&review, &review_test_diff())
                .comments
                .is_empty()
        );
    }

    #[test]
    fn review_suggestion_replacement_containing_backticks_is_fenced_safely() {
        let review = review_with(
            Vec::new(),
            vec![review_suggestion(
                Some("crates/tui/src/lib.rs"),
                Some(11),
                Some("let b = \"```\";"),
            )],
        );
        let comments = plan_inline_review_comments(&review, &review_test_diff()).comments;
        let body = comments[0]["body"].as_str().expect("body");
        assert!(body.contains("````suggestion\n"), "{body}");
        assert!(body.ends_with("\n````"), "{body}");
    }

    #[test]
    fn review_suggestion_multi_line_replacement_without_span_degrades_to_prose() {
        // SAFETY: with no start_line/end_line, GitHub would insert these two
        // lines at line 11 and leave the original line 11 duplicated below.
        let review = review_with(
            Vec::new(),
            vec![review_suggestion(
                Some("crates/tui/src/lib.rs"),
                Some(11),
                Some("let b = a.unwrap_or_default();\nlet c = 2;"),
            )],
        );
        let plan = plan_inline_review_comments(&review, &review_test_diff());
        assert_eq!(plan.comments.len(), 1);
        assert!(
            !plan.comments[0]["body"]
                .as_str()
                .expect("body")
                .contains("```suggestion")
        );
        assert_eq!(plan.degraded_to_prose, 1);
    }

    #[test]
    fn review_suggestion_prose_fence_is_never_committable() {
        // SAFETY: only the replacement this reviewer validated against the
        // diff may be one click from merging. A fence the model wrote inside
        // its explanation is downgraded to a plain code block.
        let mut suggestion = review_suggestion(Some("crates/tui/src/lib.rs"), Some(11), None);
        suggestion.suggestion =
            "Try this:\n\n```suggestion\nlet b = a.unwrap_or_default();\n```".to_string();
        let review = review_with(Vec::new(), vec![suggestion]);
        let comments = plan_inline_review_comments(&review, &review_test_diff()).comments;
        let body = comments[0]["body"].as_str().expect("body");
        assert!(!body.contains("```suggestion"), "{body}");
        assert!(body.contains("```text"), "{body}");
        assert!(body.contains("let b = a.unwrap_or_default();"), "{body}");
    }

    #[test]
    fn review_suggestion_tilde_prose_fence_is_neutralized() {
        let mut suggestion = review_suggestion(Some("crates/tui/src/lib.rs"), Some(11), None);
        suggestion.suggestion = "  ~~~suggestion\n  x\n  ~~~".to_string();
        let review = review_with(Vec::new(), vec![suggestion]);
        let comments = plan_inline_review_comments(&review, &review_test_diff()).comments;
        let body = comments[0]["body"].as_str().expect("body");
        assert!(body.contains("  ~~~text"), "{body}");
    }

    #[test]
    fn suggestion_fence_behind_list_marker_or_blockquote_is_neutralized() {
        // Whether GitHub renders a fence nested behind a list marker,
        // blockquote cue, or ordered-list marker applicable is unverified,
        // so those shapes are treated as live and downgraded too. Ordinary
        // prose lines that merely start with a marker stay untouched.
        let neutralized = neutralize_model_suggestion_fences(
            "```suggestion\n- ```suggestion\n  rm -rf /\n> ```suggestion\n1. ```suggestion\n> - ~~~suggestion\n- fix the `foo` call",
        );
        assert_eq!(
            neutralized,
            "```text\n- ```text\n  rm -rf /\n> ```text\n1. ```text\n> - ~~~text\n- fix the `foo` call"
        );
    }

    #[test]
    fn inline_issue_comment_neutralizes_model_suggestion_fences_in_issue_text() {
        // SAFETY: regression for a proven hole — the issue title and
        // description are raw model text, and a ```suggestion fence in
        // either used to reach the inline comment body verbatim: a
        // one-click mergeable block that bypassed every span and size
        // check. Only a replacement validated against the diff hunks may
        // ever be committable.
        let mut fenced_description = review_issue("error", Some("crates/tui/src/lib.rs"), Some(11));
        fenced_description.title = "Cleanup script".to_string();
        fenced_description.description = "Run this:\n\n```suggestion\nrm -rf /\n```\n".to_string();
        let mut fenced_title = review_issue("warning", Some("crates/tui/src/lib.rs"), Some(12));
        fenced_title.title = "Fix all\n```suggestion\nrm -rf ~\n```".to_string();
        fenced_title.description = "detail".to_string();
        let review = review_with(vec![fenced_description, fenced_title], Vec::new());

        let comments = plan_inline_review_comments(&review, &review_test_diff()).comments;
        assert_eq!(comments.len(), 2);
        for comment in &comments {
            let body = comment["body"].as_str().expect("body");
            assert!(!body.contains("```suggestion"), "{body}");
        }
        let description_body = comments[0]["body"].as_str().expect("body");
        assert!(
            description_body.contains("```text\nrm -rf /"),
            "{description_body}"
        );
        let title_body = comments[1]["body"].as_str().expect("body");
        assert!(title_body.contains("```text\nrm -rf ~"), "{title_body}");
    }

    #[test]
    fn pr_prompt_preserves_the_last_patch_beyond_the_old_200kib_cutoff() {
        let diff = format!(
            "{}\ndiff --git a/last.rs b/last.rs\n+LAST_PATCH\n",
            "x".repeat(210 * 1024)
        );
        let view = GhPullRequest {
            head_sha: "b".repeat(40),
            base_sha: "a".repeat(40),
            changed_files: 301,
            ..Default::default()
        };
        let prompt = format_pr_prompt(6002, &view, &diff);
        assert!(prompt.contains(&diff));
        assert!(prompt.contains("+LAST_PATCH"));
        assert!(prompt.contains(&view.head_sha));
        assert!(!prompt.contains("diff truncated"));
    }

    #[test]
    fn pr_review_unreviewed_note_names_budget_stops_and_plan_skips() {
        assert_eq!(pr_review_unreviewed_note(None, 0), "the entire diff");
        fn patch(name: &str, content: &str) -> String {
            format!(
                "diff --git a/{name} b/{name}\nnew file mode 100644\n--- /dev/null\n+++ b/{name}\n@@ -0,0 +1 @@\n+{content}\n"
            )
        }
        let patches = [
            patch("a.txt", "alpha"),
            patch("b.txt", "bravo"),
            patch("c.txt", "charlie"),
        ];
        let diff = patches.concat();
        let max_chars = patches
            .iter()
            .map(|patch| patch.chars().count())
            .max()
            .unwrap();
        let view = GhPullRequest {
            changed_files: 3,
            ..Default::default()
        };
        // Two passes of budget: a and b are planned, c is skipped by the plan.
        let plan = crate::tools::review::plan_pr_review(&diff, &view, max_chars, 2).unwrap();
        let note = pr_review_unreviewed_note(Some(&plan), 0);
        assert!(note.contains("were never read"), "{note}");
        assert!(note.contains("a/b.txt b/b.txt"), "{note}");
        assert!(note.contains("the plan never scheduled"), "{note}");
        assert!(note.contains("a/c.txt b/c.txt"), "{note}");
        // One pass done: only b is left unread, but the plan skip still stands.
        let note = pr_review_unreviewed_note(Some(&plan), 1);
        assert!(!note.contains("a/a.txt b/a.txt"), "{note}");
        assert!(note.contains("a/b.txt b/b.txt"), "{note}");
        assert!(note.contains("a/c.txt b/c.txt"), "{note}");
        // A complete plan with every pass done names nothing.
        let complete = crate::tools::review::plan_pr_review(&diff, &view, max_chars, 3).unwrap();
        assert_eq!(
            pr_review_unreviewed_note(Some(&complete), 3),
            "no planned file was left unread"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn actual_cli_review_report_host_matches_legacy_and_core_keeps_inline_authority() {
        let _home = crate::test_support::SealedHome::new();
        let _policy = crate::plugins::activation::TestPolicyGuard::extension_host(false);
        let Some(node) = crate::extension_host::tests::node_for_tests("cli_review_report_parity")
        else {
            return;
        };
        let root = tempfile::tempdir().unwrap();
        let manager = std::sync::Arc::new(crate::extension_host::ExtensionHostManager::new(
            crate::extension_host::ExtensionHostOptions {
                runtime: codewhale_config::extension_host::ExtensionHostRuntime::Node,
                node_override: Some(node),
                root: Some(root.path().join("host")),
                ..Default::default()
            },
        ));
        let _manager =
            crate::extension_host::TestManagerGuard::install(std::sync::Arc::clone(&manager));
        let mut flags = codewhale_runtime::features::Features::with_defaults();
        flags.enable(codewhale_runtime::features::Feature::ReviewHost);
        let context = crate::tools::spec::ToolContext::new(root.path()).with_features(flags);
        let mut issue = review_issue("warning", Some("crates/tui/src/lib.rs"), Some(11));
        issue.description = "malicious body\n> - ```suggestion\nnot approved\n```".into();
        let valid = review_suggestion(
            Some("crates/tui/src/lib.rs"),
            Some(11),
            Some("let b = a.unwrap_or_default();"),
        );
        let invalid = review_suggestion(Some("untouched.rs"), Some(11), Some("unowned replacement"));
        let review = review_with(vec![issue], vec![valid, invalid]);
        let view = GhPullRequest {
            head_sha: "a".repeat(40),
            base_sha: "b".repeat(40),
            ..Default::default()
        };
        let local = render_review_report_for_context(Some(&review), "unused", None, &context)
            .await
            .unwrap();
        assert_eq!(local, render_review_markdown(&review, None));
        let posted =
            render_review_report_for_context(Some(&review), "unused", Some((7, &view)), &context)
                .await
                .unwrap();
        assert_eq!(posted, render_review_markdown(&review, Some((7, &view))));
        assert!(posted.ends_with(&review_advisory_footer(7, &view)));
        let safe = neutralize_model_suggestion_fences(&posted);
        assert!(!safe.contains("```suggestion"));
        assert!(safe.contains("> - ```text"));
        let plan = plan_inline_review_comments(&review, &review_test_diff());
        assert_eq!(plan.comments.len(), 2);
        assert_eq!(plan.dropped_untouched_file, 1);
        assert!(
            plan.comments[1]["body"]
                .as_str()
                .unwrap()
                .contains("```suggestion")
        );
        assert!(
            !plan.comments[0]["body"]
                .as_str()
                .unwrap()
                .contains("```suggestion")
        );
        assert_eq!(
            render_review_report_for_context(None, "plain prose", None, &context)
                .await
                .unwrap(),
            "plain prose"
        );
        let prompt = crate::tools::review_host::interactive(7, &view, &review_test_diff(), &context)
            .await
            .unwrap();
        assert_eq!(prompt, format_pr_prompt(7, &view, &review_test_diff()));
        manager.shutdown().await;
    }

    #[test]
    fn pr_review_markdown_shows_the_computed_replacement() {
        // A plain `codewhale review --pr` (no --post) computes and validates
        // the literal fix; the local report must show it, not just the prose.
        let view = GhPullRequest {
            title: "T".to_string(),
            body: String::new(),
            base: "main".to_string(),
            head: "feature".to_string(),
            url: "https://example.invalid/pr/1".to_string(),
            head_sha: "abc123".to_string(),
            ..Default::default()
        };
        let mut single = review_suggestion(
            Some("crates/tui/src/lib.rs"),
            Some(11),
            Some("let b = a.unwrap_or_default();"),
        );
        single.suggestion = "Use the checked variant".to_string();
        let mut multi = review_suggestion(
            Some("crates/tui/src/lib.rs"),
            Some(11),
            Some("let b = a.unwrap_or_default();\nlet c = 2;"),
        );
        multi.suggestion = "Replace both lines".to_string();
        multi.start_line = Some(11);
        multi.end_line = Some(12);
        let review = review_with(Vec::new(), vec![single, multi]);

        let local = render_review_markdown(&review, None);
        assert!(local.contains("### Suggestions"), "{local}");
        assert!(
            local.contains("\n  ```suggestion\n  let b = a.unwrap_or_default();\n  ```\n"),
            "{local}"
        );
        assert!(
            local
                .contains("\n  ```suggestion\n  let b = a.unwrap_or_default();\n  let c = 2;\n  ```\n"),
            "{local}"
        );

        // The posted body keeps the fix visible but never as a live
        // one-click block: only diff-validated inline suggestion comments
        // may carry those to GitHub.
        let posted = render_review_markdown(&review, Some((1, &view)));
        assert!(
            posted.contains("let b = a.unwrap_or_default();"),
            "{posted}"
        );
        assert!(!posted.contains("```suggestion"), "{posted}");
        assert!(posted.contains("```diff"), "{posted}");
    }

    /// #6510: a plain-diff review now asks for the one structured review
    /// contract. A reply that keeps it renders as the Markdown report; one
    /// that ignores it is printed verbatim, as the old prose path did.
    #[test]
    fn plain_diff_review_renders_structured_reply_and_keeps_prose() {
        let json = r#"{"summary":"One risky unwrap.","issues":[{"severity":"high","title":"Unchecked unwrap","description":"Panics on None.","path":"src/lib.rs","line":11}],"suggestions":[],"overall_assessment":"request changes"}"#;
        let structured = crate::tools::review::ReviewOutput::from_structured_str(json);
        let report = plain_diff_review_report(structured.as_ref(), json);
        assert!(report.starts_with("## Codewhale review"), "{report}");
        assert!(report.contains("One risky unwrap."), "{report}");
        assert!(report.contains("### Findings"), "{report}");
        assert!(report.contains("`src/lib.rs:11`"), "{report}");
        assert!(
            !report.contains("\"issues\""),
            "raw JSON must not leak: {report}"
        );

        let prose = "Looks fine overall; consider a test for the empty case.";
        let structured = crate::tools::review::ReviewOutput::from_structured_str(prose);
        assert!(structured.is_none());
        assert_eq!(plain_diff_review_report(structured.as_ref(), prose), prose);
    }

    #[test]
    fn oversized_review_suggestion_degrades_to_prose() {
        use crate::tools::review::MAX_COMMITTABLE_SUGGESTION_LINES;

        // A whole-hunk rewrite is not a mechanical fix, so it must not be
        // committable even though every line is inside the diff.
        let span = MAX_COMMITTABLE_SUGGESTION_LINES + 5;
        let added: String = (1..=span).map(|i| format!("+line {i}\n")).collect();
        let diff = format!(
            "diff --git a/big.rs b/big.rs\nnew file mode 100644\n--- /dev/null\n\
                     +++ b/big.rs\n@@ -0,0 +1,{span} @@\n{added}"
        );
        let replacement: String = (1..=span)
            .map(|i| format!("line {i} fixed"))
            .collect::<Vec<_>>()
            .join("\n");

        let mut suggestion = review_suggestion(Some("big.rs"), Some(span), Some(&replacement));
        suggestion.start_line = Some(1);
        suggestion.end_line = Some(span);
        let review = review_with(Vec::new(), vec![suggestion]);
        let comments = plan_inline_review_comments(&review, &diff).comments;
        assert_eq!(comments.len(), 1);
        let body = comments[0]["body"].as_str().expect("body");
        assert!(!body.contains("```suggestion"), "{body}");

        // The same shape, trimmed to the budget, stays committable.
        let mut suggestion = review_suggestion(
            Some("big.rs"),
            Some(MAX_COMMITTABLE_SUGGESTION_LINES),
            Some("line 1 fixed"),
        );
        suggestion.start_line = Some(1);
        suggestion.end_line = Some(MAX_COMMITTABLE_SUGGESTION_LINES);
        let review = review_with(Vec::new(), vec![suggestion]);
        let comments = plan_inline_review_comments(&review, &diff).comments;
        assert!(
            comments[0]["body"]
                .as_str()
                .expect("body")
                .contains("```suggestion"),
            "a span at the budget limit stays committable"
        );
    }

    #[tokio::test]
    async fn configured_workflow_default_keeps_named_custom_route() {
        let config = custom_exec_config("custom-a");
        let model = config.default_model();

        let route = resolve_cli_exec_route(
            &config,
            &model,
            "Run a checked-in Workflow through the host runtime",
            true,
        )
        .await
        .expect("configured workflow route");
        let execution = config_for_cli_route(&config, &route).expect("admitted execution route");

        assert_eq!(route.provider.provider, codewhale_config::ProviderKind::Custom);
        assert_eq!(
            execution.active_provider_identity().unwrap().key.as_str(),
            "custom-a"
        );
        assert_eq!(
            execution.active_route_base_url(),
            "http://127.0.0.1:18181/v1"
        );
        let client = crate::client::CodewhaleClient::new(&execution).expect("workflow client");
        assert_eq!(client.base_url(), "http://127.0.0.1:18181/v1");
    }

    #[test]
    fn exec_json_receipts_keep_exact_named_custom_provider() {
        let config = custom_exec_config("custom-a");
        let identity = config.active_provider_identity().unwrap();
        let provider = identity.key.as_str();
        // #6510: plain exec is an Engine turn now; its `--json` receipt keeps
        // the documented one-shot fields (docs/LIVE_SMOKE.md step 5).
        let mut one_shot = ExecSummary {
            mode: "one-shot".to_string(),
            provider: provider.to_string(),
            model: "model-a".to_string(),
            output: "done".to_string(),
            status: Some("completed".to_string()),
            ..ExecSummary::default()
        };
        one_shot.record_one_shot_outcome(Some(codewhale_models::Usage {
            input_tokens: 12,
            output_tokens: 3,
            ..Default::default()
        }));
        let one_shot = serde_json::to_value(&one_shot).expect("one-shot receipt");
        assert_eq!(one_shot["mode"], "one-shot");
        assert_eq!(one_shot["provider"], "custom-a");
        assert_eq!(one_shot["model"], "model-a");
        assert_eq!(one_shot["output"], "done");
        assert_eq!(one_shot["success"], true);
        assert_eq!(one_shot["usage"]["input_tokens"], 12);
        assert_eq!(one_shot["usage"]["output_tokens"], 3);

        let mut failed = ExecSummary {
            mode: "one-shot".to_string(),
            provider: provider.to_string(),
            model: "model-a".to_string(),
            status: Some("failed".to_string()),
            error: Some("Model response incomplete".to_string()),
            ..ExecSummary::default()
        };
        failed.record_one_shot_outcome(None);
        let failed = serde_json::to_value(&failed).expect("failed one-shot receipt");
        assert_eq!(failed["success"], false);
        assert!(
            failed.get("usage").is_none(),
            "no usage is never zero usage"
        );

        let agent = serde_json::to_value(ExecSummary {
            mode: "agent".to_string(),
            provider: provider.to_string(),
            model: "model-a".to_string(),
            ..ExecSummary::default()
        })
        .expect("agent exec JSON receipt");
        assert_eq!(agent["provider"], "custom-a");
        assert!(
            agent.get("success").is_none() && agent.get("usage").is_none(),
            "agent receipts keep their pre-#6510 shape"
        );
        let serialized = serde_json::to_string(&agent).expect("serialize receipt");
        assert!(!serialized.contains("127.0.0.1"));
        assert!(!serialized.contains("local-test-key"));
    }

    #[test]
    fn exec_stream_provider_pair_preserves_named_literal_and_root_custom_provenance() {
        let mut config = Config::default();
        for key in ["lm-studio", "custom"] {
            config.providers.get_or_insert_with(Default::default).custom.insert(
                key.to_string(),
                codewhale_runtime::config::ProviderConfig {
                    kind: Some("openai-compatible".to_string()),
                    base_url: Some("http://localhost:1234/v1".to_string()),
                    ..Default::default()
                },
            );
        }
        let named = config.resolve_provider_identity("lm-studio").expect("exact named table");
        let literal = config.resolve_provider_identity("custom").expect("exact literal table");
        let root_config = {
            let __cw_config_init_1_0 = Some("custom".to_string());
            let mut __cw_config_init_1_value: Config = Config::default();
            __cw_config_init_1_value.provider = __cw_config_init_1_0;
            __cw_config_init_1_value
        }.with_legacy_root(None, Some("http://localhost:1234/v1".to_string()));
        let root = root_config.active_provider_identity().expect("captured root migration");
        let built_in = Config::default().active_provider_identity().expect("built-in fixture");

        assert_eq!(
            exec_stream_provider_route(&named),
            ("custom".to_string(), Some("lm-studio".to_string()))
        );
        assert_eq!(
            exec_stream_provider_route(&literal),
            ("custom".to_string(), Some("custom".to_string()))
        );
        assert_eq!(
            exec_stream_provider_route(&root),
            ("custom".to_string(), None)
        );
        assert_eq!(
            exec_stream_provider_route(&built_in),
            ("deepseek".to_string(), None)
        );
    }

    #[test]
    fn resumed_exec_persistence_updates_provider_and_model_as_one_route() {
        let saved_a = saved_exec_session("custom-a", codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL);
        let mut config = custom_exec_config("custom-a");
        apply_exec_provider_override(&mut config, "custom-b").expect("custom B");
        let model = resolve_exec_resume_route(&mut config, &saved_a, true, None)
            .expect("explicit provider route");
        let mut persisted = saved_a;
        stamp_exec_session_metadata(
            &mut persisted,
            &model,
            codewhale_config::ProviderKind::Custom.as_str(),
            Some("custom-b"),
            Path::new("/tmp/exec-resume"),
        );

        let mut next_config = custom_exec_config("custom-a");
        let resumed_model = resolve_exec_resume_route(&mut next_config, &persisted, false, None)
            .expect("next plain resume");

        assert_eq!(persisted.metadata.model_provider, "custom");
        assert_eq!(
            persisted.metadata.model_provider_id.as_deref(),
            Some("custom-b")
        );
        assert_eq!(persisted.metadata.model, "model-b");
        assert_eq!(next_config.provider.as_deref(), Some("custom-b"));
        assert_eq!(resumed_model, "model-b");
    }

    #[test]
    fn exec_persistence_omits_id_for_legacy_root_custom_route() {
        let mut saved = session_manager::create_saved_session_with_mode(
            &[],
            "legacy-root-model",
            Path::new("/tmp/exec-root"),
            0,
            None,
            Some("exec"),
        );
        stamp_exec_session_metadata(
            &mut saved,
            "legacy-root-model",
            codewhale_config::ProviderKind::Custom.as_str(),
            None,
            Path::new("/tmp/exec-root"),
        );

        assert_eq!(saved.metadata.model_provider, "custom");
        assert_eq!(saved.metadata.model_provider_id, None);
        assert!(
            !serde_json::to_string(&saved)
                .expect("serialize exec session")
                .contains("model_provider_id")
        );
    }

    #[test]
    fn exec_parses_reasoning_effort_flag_alongside_provider() {
        let cli = parse_cli(&[
            "codewhale",
            "exec",
            "--provider",
            "openrouter",
            "--model",
            "glm-5.2",
            "--reasoning-effort",
            "max",
            "audit",
        ]);
        let Some(Commands::Exec(args)) = cli.command else {
            panic!("expected exec command");
        };

        assert_eq!(args.provider.as_deref(), Some("openrouter"));
        assert_eq!(args.model.as_deref(), Some("glm-5.2"));
        assert_eq!(args.reasoning_effort.as_deref(), Some("max"));
        assert_eq!(args.prompt, vec!["audit"]);
    }

    #[test]
    fn cli_reasoning_effort_normalizes_aliases_and_rejects_typos() {
        // The thinking ladder split these: `xhigh` is a tier the CLI can now
        // name, `ultracode` is still an alias and resolves to `ultra`.
        assert_eq!(
            normalize_cli_reasoning_effort("xhigh").unwrap().as_deref(),
            Some("xhigh")
        );
        assert_eq!(
            normalize_cli_reasoning_effort("ultracode")
                .unwrap()
                .as_deref(),
            Some("ultra")
        );
        assert_eq!(normalize_cli_reasoning_effort("default").unwrap(), None);
        assert!(normalize_cli_reasoning_effort("expensive").is_err());
    }

    #[test]
    fn cli_auto_resolves_to_the_declared_default_before_k3_route_normalization() {
        let config = {
            let __cw_config_init_2_0 = Some("moonshot".to_string());
            let __cw_config_init_2_1 = Some(codewhale_runtime::config::ProvidersConfig {
                moonshot: codewhale_runtime::config::ProviderConfig {
                    base_url: Some(
                        codewhale_config::descriptors::defaults::DEFAULT_KIMI_CODE_BASE_URL.to_string(),
                    ),
                    model: Some(
                        codewhale_config::catalog::reviewed::constants::KIMI_CODE_K3_MODEL.to_string(),
                    ),
                    ..Default::default()
                },
                ..Default::default()
            });
            let mut __cw_config_init_2_value: Config = Default::default();
            __cw_config_init_2_value.provider = __cw_config_init_2_0;
            __cw_config_init_2_value.providers = __cw_config_init_2_1;
            __cw_config_init_2_value
        };

        // #6290 rework: Auto no longer classifies the prompt. Any wording
        // resolves the declared policy tier, normalized for the K3 route.
        assert_eq!(
            cli_reasoning_effort_value_for_prompt(
                &config,
                codewhale_config::catalog::reviewed::constants::KIMI_CODE_K3_MODEL,
                crate::reasoning_preference::ReasoningEffort::Auto,
            )
            .as_deref(),
            Some("high"),
            "Auto resolves the declared default, not a classification"
        );

        assert_eq!(
            cli_reasoning_effort_value_for_prompt(
                &config,
                codewhale_config::catalog::reviewed::constants::KIMI_CODE_K3_MODEL,
                crate::reasoning_preference::ReasoningEffort::Off,
            )
            .as_deref(),
            Some("low"),
            "membership K3 still applies its exact-route always-thinking floor"
        );
    }

    #[test]
    fn cli_route_tracks_auto_reasoning_independently_from_auto_model() {
        use crate::reasoning_preference::ReasoningEffort;

        let fixed_model_auto_reasoning = CliAutoRoute {
            provider: Config::default().test_identity_for_kind(codewhale_config::ProviderKind::Deepseek),
            model: codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL.to_string(),
            reasoning_effort: Some(ReasoningEffort::Auto),
            auto_controls_reasoning: true,
            auto_model: false,
        };
        let auto_model_fixed_reasoning = CliAutoRoute {
            provider: Config::default().test_identity_for_kind(codewhale_config::ProviderKind::OpenaiCodex),
            model: codewhale_config::descriptors::defaults::DEFAULT_OPENAI_CODEX_MODEL.to_string(),
            reasoning_effort: Some(ReasoningEffort::High),
            auto_controls_reasoning: false,
            auto_model: true,
        };

        assert!(fixed_model_auto_reasoning.auto_controls_reasoning);
        assert!(!fixed_model_auto_reasoning.auto_model);
        assert!(!auto_model_fixed_reasoning.auto_controls_reasoning);
        assert!(auto_model_fixed_reasoning.auto_model);
    }

    #[test]
    fn saved_reasoning_preference_overrides_config_for_non_tui_runtimes() {
        let mut config = {
            let __cw_config_init_3_0 = Some("max".to_string());
            let __cw_config_init_3_1 = true;
            let mut __cw_config_init_3_value: Config = Default::default();
            __cw_config_init_3_value.reasoning_effort = __cw_config_init_3_0;
            __cw_config_init_3_value.reasoning_effort_inferred_from_legacy_alias = __cw_config_init_3_1;
            __cw_config_init_3_value
        };
        let settings = {
            let __cw_config_init_4_0 = Some("low".to_string());
            let mut __cw_config_init_4_value: codewhale_runtime::settings::Settings =
                Default::default();
            __cw_config_init_4_value.reasoning_effort = __cw_config_init_4_0;
            __cw_config_init_4_value
        };

        apply_saved_reasoning_preference(&mut config, &settings);

        assert_eq!(config.reasoning_effort(), Some("low"));
        assert!(config.reasoning_effort_is_explicit());
    }

    /// `run_exec_agent` must hand the engine a concrete tier, never the literal
    /// `"auto"` sentinel, for a fixed-model Auto launch.
    #[test]
    fn fixed_model_exec_auto_resolves_to_a_concrete_tier_not_the_auto_sentinel() {
        let config = {
            let __cw_config_init_5_0 = Some("zai".to_string());
            let mut __cw_config_init_5_value: Config = Default::default();
            __cw_config_init_5_value.provider = __cw_config_init_5_0;
            __cw_config_init_5_value
        };

        let resolved = cli_reasoning_effort_value_for_prompt(
            &config,
            codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL,
            crate::reasoning_preference::ReasoningEffort::Auto,
        )
        .expect("Auto must resolve to a concrete tier");

        assert_ne!(
            resolved, "auto",
            "the literal auto sentinel must never reach a provider"
        );
        assert!(
            matches!(resolved.as_str(), "off" | "low" | "medium" | "high" | "max"),
            "unexpected resolved tier: {resolved}"
        );
    }

    #[test]
    fn exec_accepts_resume_session_flags_for_harnesses() {
        let cli = parse_cli(&[
            "codewhale",
            "exec",
            "--resume",
            "abc123",
            "--output-format",
            "stream-json",
            "follow up",
        ]);
        let Some(Commands::Exec(args)) = cli.command else {
            panic!("expected exec command");
        };

        assert_eq!(args.resume.as_deref(), Some("abc123"));
        assert_eq!(args.output_format, ExecOutputFormat::StreamJson);
        assert_eq!(args.prompt, vec!["follow up"]);
        assert!(!args.hooks, "headless hooks stay opt-in by default");
    }

    #[test]
    fn exec_accepts_session_id_alias() {
        let cli = parse_cli(&["codewhale", "exec", "--session-id", "abc123", "follow up"]);
        let Some(Commands::Exec(args)) = cli.command else {
            panic!("expected exec command");
        };

        assert_eq!(args.session_id.as_deref(), Some("abc123"));
        assert_eq!(args.output_format, ExecOutputFormat::Text);
    }

    #[test]
    fn exec_parses_tool_gate_and_hardening_flags() {
        let envelope = r#"{"schema_version":1,"owner":"fleet-worker-1","authority":"read_only"}"#;
        let cli = parse_cli(&[
            "codewhale",
            "exec",
            "--allowed-tools",
            "File,Git",
            "--disallowed-tools",
            "Bash",
            "--max-turns",
            "7",
            "--max-tool-calls",
            "9",
            "--append-system-prompt",
            "extra rules",
            "--tool-authority-json",
            envelope,
            "--hooks",
            "do the thing",
        ]);
        let Some(Commands::Exec(args)) = cli.command else {
            panic!("expected exec command");
        };

        assert_eq!(
            args.allowed_tools.as_deref(),
            Some(&["File".to_string(), "Git".to_string()][..])
        );
        assert_eq!(
            args.disallowed_tools.as_deref(),
            Some(&["Bash".to_string()][..])
        );
        assert_eq!(args.max_turns, Some(7));
        assert_eq!(args.max_tool_calls, Some(9));
        assert_eq!(args.append_system_prompt.as_deref(), Some("extra rules"));
        assert_eq!(args.tool_authority_json.as_deref(), Some(envelope));
        assert!(args.hooks);
        assert_eq!(args.prompt, vec!["do the thing"]);
    }

    #[test]
    fn exec_rejects_zero_max_tool_calls() {
        let err = Cli::try_parse_from(["codewhale", "exec", "--max-tool-calls", "0", "do the thing"])
            .expect_err("max-tool-calls must be >= 1");
        assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
    }

    #[test]
    fn fleet_tool_authority_cannot_cross_an_exec_resume_boundary() {
        assert!(validate_exec_tool_authority_resume(None, true).is_ok());
        assert!(validate_exec_tool_authority_resume(Some("{}"), false).is_ok());
        let error = validate_exec_tool_authority_resume(Some("{}"), true)
            .expect_err("authority must remain bound to its fresh Fleet launch")
            .to_string();
        assert!(error.contains("cannot be combined with exec --resume"));
    }

    #[test]
    fn exec_auto_does_not_authorize_sandbox_elevation() {
        let cli = parse_cli(&["codewhale", "exec", "--auto", "run it"]);
        let Some(Commands::Exec(args)) = cli.command else {
            panic!("expected exec command");
        };

        assert!(!exec_sandbox_elevation_authorized(
            args.allow_sandbox_elevation,
            args.sandbox.as_deref()
        ));
    }

    #[test]
    fn exec_explicit_sandbox_elevation_opt_ins_authorize_retry() {
        let danger = parse_cli(&[
            "codewhale",
            "exec",
            "--auto",
            "--sandbox",
            "danger-full-access",
            "run it",
        ]);
        let Some(Commands::Exec(args)) = danger.command else {
            panic!("expected exec command");
        };
        assert!(exec_sandbox_elevation_authorized(
            args.allow_sandbox_elevation,
            args.sandbox.as_deref()
        ));

        let flag = parse_cli(&[
            "codewhale",
            "exec",
            "--auto",
            "--allow-sandbox-elevation",
            "run it",
        ]);
        let Some(Commands::Exec(args)) = flag.command else {
            panic!("expected exec command");
        };
        assert!(exec_sandbox_elevation_authorized(
            args.allow_sandbox_elevation,
            args.sandbox.as_deref()
        ));
    }

    #[test]
    fn exec_sandbox_denial_stream_event_is_typed() {
        let event = ExecStreamEvent::SandboxDenied {
            tool_id: "call_1".to_string(),
            tool_name: "exec_shell".to_string(),
            reason: "write blocked".to_string(),
            outcome: "approval_required".to_string(),
        };
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&event).expect("serializes"))
                .expect("valid json");
        assert_eq!(value["type"], "sandbox_denied");
        assert_eq!(value["outcome"], "approval_required");
    }

    #[test]
    fn exec_help_discloses_full_access_and_session_persistence() {
        let _env_lock = crate::test_support::lock_test_env();
        let _locale = crate::test_support::EnvVarGuard::set("LC_ALL", "en_US.UTF-8");
        let mut cli = Cli::command();
        let help = cli
            .find_subcommand_mut("exec")
            .expect("exec command")
            .render_help()
            .to_string();
        assert!(help.contains("--auto"));
        assert!(help.contains("--sandbox"));
        assert!(help.contains("--allow-sandbox-elevation"));
        assert!(help.contains("Full Access"));
        assert!(help.contains("no shell sandbox"));
        assert!(help.contains("trust mode on"));
        assert!(help.contains("--sandbox workspace-write"));
        assert!(help.contains("Any --sandbox other than danger-full-access"));
        assert!(help.contains("equivalent sandbox_mode in config"));
        assert!(help.contains("Put -- before a prompt starting with -"));
        assert!(help.contains("Runs that offer tools"));
        assert!(help.contains("codewhale receipts <id>"));
    }

    #[test]
    fn exec_shell_only_tool_surface_env_sets_shell_allowlist() {
        let _env_lock = crate::test_support::lock_test_env();
        let _surface =
            crate::test_support::EnvVarGuard::set(CODEWHALE_TOOL_SURFACE_ENV, " shell-only ");

        let allowed_tools = resolve_exec_allowed_tools(None, exec_tool_surface_from_env())
            .expect("shell-only surface should set an allowlist");

        assert_eq!(allowed_tools, vec!["bash".to_string()]);
    }

    #[test]
    fn exec_explicit_allowed_tools_override_shell_only_env() {
        let _env_lock = crate::test_support::lock_test_env();
        let _surface = crate::test_support::EnvVarGuard::set(CODEWHALE_TOOL_SURFACE_ENV, "shell-only");
        let explicit = vec![" File ".to_string(), "GIT".to_string()];

        let allowed_tools = resolve_exec_allowed_tools(Some(&explicit), exec_tool_surface_from_env())
            .expect("explicit allowlist should be preserved");

        assert_eq!(allowed_tools, vec!["file".to_string(), "git".to_string()]);
    }

    #[test]
    fn exec_full_tool_surface_env_leaves_allowlist_unset() {
        let _env_lock = crate::test_support::lock_test_env();
        let _surface = crate::test_support::EnvVarGuard::set(CODEWHALE_TOOL_SURFACE_ENV, "full");

        assert_eq!(
            resolve_exec_allowed_tools(None, exec_tool_surface_from_env()),
            None
        );
    }

    #[test]
    fn exec_unknown_tool_surface_env_warns_without_allowlist() {
        assert!(should_warn_unknown_exec_tool_surface("shell_onyl"));
        assert!(!should_warn_unknown_exec_tool_surface("shell-only"));
        assert!(!should_warn_unknown_exec_tool_surface("native-tools"));
        assert!(!should_warn_unknown_exec_tool_surface("full"));
        assert!(!should_warn_unknown_exec_tool_surface(" "));
        assert_eq!(parse_exec_tool_surface("shell_onyl"), None);
    }

    #[test]
    fn exec_rejects_zero_max_turns() {
        let err = Cli::try_parse_from(["codewhale", "exec", "--max-turns", "0", "hello"])
            .expect_err("max-turns must be >= 1");
        assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
    }
