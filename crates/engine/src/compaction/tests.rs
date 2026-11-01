use super::*;

fn report(error: &anyhow::Error) -> String {
    report_compaction_failure("Auto-compaction failed", "compact_fixture", true, error)
}

#[test]
fn strip_compaction_summaries_removes_only_summary_blocks() {
    let base = SystemBlock {
        block_type: "text".to_string(),
        text: "stable base prompt".to_string(),
        cache_control: None,
    };
    let summary = SystemBlock {
        block_type: "text".to_string(),
        text: format!("{LEGACY_V2_COMPACTION_SUMMARY_MARKER} and its body"),
        cache_control: None,
    };
    let legacy = SystemBlock {
        block_type: "text".to_string(),
        text: format!("{LEGACY_COMPACTION_SUMMARY_MARKER}\nold-format body"),
        cache_control: None,
    };

    let stripped = strip_compaction_summaries(Some(&SystemPrompt::Blocks(vec![
        base.clone(),
        summary.clone(),
        legacy,
    ])))
    .expect("base block survives");
    match stripped {
        SystemPrompt::Blocks(blocks) => {
            assert_eq!(blocks.len(), 1);
            assert_eq!(blocks[0].text, "stable base prompt");
        }
        SystemPrompt::Text(_) => panic!("blocks stay blocks"),
    }

    // A prompt that is nothing but a summary strips to None.
    assert!(strip_compaction_summaries(Some(&SystemPrompt::Text(summary.text))).is_none());
    // A prompt without summaries is unchanged.
    assert_eq!(
        strip_compaction_summaries(Some(&SystemPrompt::Text("plain".to_string()))),
        Some(SystemPrompt::Text("plain".to_string()))
    );
    // Host text that quotes the new marker outside the carrier delimiters is
    // project instruction, not a checkpoint: nothing is truncated or extracted.
    let quoted = SystemPrompt::Text(format!(
        "stable base prompt\nRead the {COMPACTION_SUMMARY_MARKER} before resuming.\nmore rules"
    ));
    assert_eq!(
        strip_compaction_summaries(Some(&quoted)),
        Some(quoted.clone())
    );
    assert!(extract_compaction_summary(Some(&quoted)).is_none());
}

#[test]
fn persisted_summary_carrier_round_trips_without_losing_the_base_prompt() {
    let carrier = format!(
        "stable base prompt\n\n{COMPACTION_SUMMARY_BEGIN}\n{COMPACTION_SUMMARY_MARKER}\nnew summary\n{COMPACTION_SUMMARY_END}"
    );

    assert_eq!(
        extract_compaction_summary(Some(&SystemPrompt::Text(carrier.clone()))),
        Some(SystemPrompt::Text(format!(
            "{COMPACTION_SUMMARY_MARKER}\nnew summary"
        )))
    );
    assert_eq!(
        strip_compaction_summaries(Some(&SystemPrompt::Text(carrier))),
        Some(SystemPrompt::Text("stable base prompt".to_string()))
    );
}

#[test]
fn combined_block_carrier_preserves_block_metadata_and_base_text() {
    let carrier = SystemBlock {
        block_type: "text".to_string(),
        text: format!(
            "stable block\n\n{COMPACTION_SUMMARY_BEGIN}\n{COMPACTION_SUMMARY_MARKER}\nblock summary\n{COMPACTION_SUMMARY_END}"
        ),
        cache_control: Some(CacheControl {
            cache_type: "ephemeral".to_string(),
        }),
    };

    let extracted = extract_compaction_summary(Some(&SystemPrompt::Blocks(vec![carrier.clone()])))
        .expect("checkpoint");
    let SystemPrompt::Blocks(extracted) = extracted else {
        panic!("blocks stay blocks");
    };
    assert_eq!(extracted.len(), 1);
    assert_eq!(
        extracted[0].text,
        format!("{COMPACTION_SUMMARY_MARKER}\nblock summary")
    );
    assert_eq!(extracted[0].cache_control, carrier.cache_control);

    let stripped =
        strip_compaction_summaries(Some(&SystemPrompt::Blocks(vec![carrier]))).expect("base block");
    let SystemPrompt::Blocks(stripped) = stripped else {
        panic!("blocks stay blocks");
    };
    assert_eq!(stripped.len(), 1);
    assert_eq!(stripped[0].text, "stable block");
    assert_eq!(
        stripped[0]
            .cache_control
            .as_ref()
            .map(|c| c.cache_type.as_str()),
        Some("ephemeral")
    );
}

#[test]
fn untyped_usage_limit_text_never_becomes_quota_exhaustion() {
    let error = anyhow::anyhow!(
        "[auth] Authorization failed: You've reached your usage limit for this billing cycle"
    );
    let message = report(&error);
    assert!(message.contains("provider rate limit blocked making room"));
    assert!(!message.contains("quota exhausted"));
}

#[test]
fn typed_quota_renders_quota_and_is_not_transient() {
    let error = anyhow::Error::new(crate::llm_client::LlmError::from_http_response(
        429,
        r#"{"error":{"code":"insufficient_quota"}}"#,
    ))
    .context("summary request failed");
    assert_eq!(
        report(&error),
        "Auto-compaction failed: provider plan quota exhausted — switch provider/model or renew the provider plan"
    );
    assert!(!is_transient_error(&error));
}

#[test]
fn typed_rate_limit_stays_transient_and_does_not_become_quota() {
    let error = anyhow::Error::new(crate::llm_client::LlmError::RateLimited {
        message: "Too Many Requests".into(),
        retry_after: None,
    });
    assert!(report(&error).contains("provider rate limit blocked making room"));
    assert!(is_transient_error(&error));
}

#[test]
fn unknown_diagnostic_is_preserved_safely() {
    let error = anyhow::anyhow!("summary response was structurally empty");
    assert_eq!(
        report(&error),
        "Auto-compaction failed: summary response was structurally empty"
    );
}

#[test]
fn untyped_transient_and_deterministic_classification_remains_compatible() {
    for message in [
        "Connection timeout",
        "429 Too Many Requests",
        "503 Service Unavailable",
        "network error: connection refused",
    ] {
        assert!(is_transient_error(&anyhow::anyhow!(message)), "{message}");
    }
    for message in [
        "401 Unauthorized: Invalid API key",
        "Failed to parse JSON response",
        "Invalid request: missing required field",
    ] {
        assert!(!is_transient_error(&anyhow::anyhow!(message)), "{message}");
    }
    assert_eq!(
        classify_compaction_failure(&anyhow::anyhow!(
            "prompt is too long for this model's context window"
        )),
        CompactionFailureKind::ContextOverflow
    );
}

fn pressure_fixture() -> Vec<Message> {
    (0..30)
        .map(|index| Message {
            role: if index % 2 == 0 {
                Role::User
            } else {
                Role::Assistant
            },
            content: vec![ContentBlock::Text {
                text: "x".repeat(8_000),
                cache_control: None,
            }],
        })
        .collect()
}

fn oversized_tool_pair(id: &str, content: String) -> Vec<Message> {
    vec![
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                execution_id: None,
                id: id.to_string(),
                name: "read_file".to_string(),
                input: serde_json::json!({"path": "src/compaction.rs"}),
                caller: None,
                thought_signature: None,
            }],
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                execution_id: None,
                tool_use_id: id.to_string(),
                content,
                is_error: None,
                content_blocks: None,
            }],
        },
    ]
}

#[test]
fn pinned_tool_result_local_pruning_is_reclaimable() {
    let mut messages =
        oversized_tool_pair("old-read", "error: ".to_string() + &"x".repeat(300_000));
    messages.extend(pressure_fixture());
    let full_pressure = estimate_input_tokens_for_pressure(&messages, None);
    let mut projected = messages.clone();
    let pruned_bytes = prune_tool_results_until(&mut projected, KEEP_RECENT_MESSAGES, |_, _| false);
    let projected_pressure = estimate_input_tokens_for_pressure(&projected, None);
    assert!(
        pruned_bytes > 250_000,
        "fixture must prune the pinned result"
    );
    assert!(projected_pressure < full_pressure);

    let config = CompactionConfig {
        token_threshold: projected_pressure + (full_pressure - projected_pressure) / 2,
        ..Default::default()
    };
    assert!(compaction_pressure_reached(&messages, None, &config));
    assert!(!compaction_pressure_reached(&projected, None, &config));
    assert!(should_compact(
        &messages,
        None,
        &PreparedCompactionEnvelope::new(config),
    ));
}

#[test]
fn local_pruning_removes_nested_tool_result_images() {
    let mut messages = oversized_tool_pair("image-read", "screenshot captured".to_string());
    let ContentBlock::ToolResult { content_blocks, .. } = &mut messages[1].content[0] else {
        panic!("tool result fixture");
    };
    *content_blocks = Some(vec![serde_json::json!({
        "type": "image",
        "mime_type": "image/png",
        "data": "A".repeat(300_000),
    })]);
    messages.extend(pressure_fixture());

    let before = estimate_input_tokens_for_pressure(&messages, None);
    let pruned = prune_tool_results_until(&mut messages, KEEP_RECENT_MESSAGES, |_, _| false);
    let after = estimate_input_tokens_for_pressure(&messages, None);
    let ContentBlock::ToolResult { content_blocks, .. } = &messages[1].content[0] else {
        panic!("tool result fixture");
    };

    assert!(pruned > 250_000, "nested image bytes must be reclaimable");
    assert!(after < before);
    assert!(content_blocks.is_none(), "base64 must not survive pruning");
}

#[test]
fn successor_floor_counts_retained_user_messages_not_tool_results() {
    let mut messages = pressure_fixture();
    messages.extend(oversized_tool_pair(
        "recent-read",
        "z".repeat(RETAINED_TOOL_RESULT_MAX_CHARS * 4),
    ));
    let base_config = CompactionConfig::default();
    let prepared = PreparedCompactionEnvelope::new(base_config.clone());
    let retained_floor = estimate_retained_floor_conservative(&messages, None, &prepared);
    let full_pressure = estimate_input_tokens_conservative(&messages, None);
    assert!(
        retained_floor < full_pressure,
        "user-only retention must reclaim the giant tool result"
    );

    let config = CompactionConfig {
        token_threshold: retained_floor + 1,
        ..base_config
    };
    assert!(compaction_pressure_reached(&messages, None, &config));
    assert!(should_compact(
        &messages,
        None,
        &PreparedCompactionEnvelope::new(config),
    ));
}

/// #5956: with no `[compaction] summary_instructions` configured, the
/// summarizer prompt is exactly the handoff body plus the language contract.
#[test]
fn compact_prompt_without_operator_instructions_is_unchanged() {
    assert_eq!(
        compact_prompt(None, None),
        format!("{} {COMPACTION_LANGUAGE_CONTRACT}", compact_prompt_body())
    );
    // Whitespace-only is unset, not an empty section.
    assert_eq!(
        compact_prompt(None, Some("   \n ")),
        compact_prompt(None, None)
    );
    // The one-off `/compact <focus>` line keeps its exact shape.
    assert_eq!(
        compact_prompt(Some("the flaky test"), None),
        format!(
            "{} {COMPACTION_LANGUAGE_CONTRACT}\n\nThe user asked this handoff to focus on: the flaky test",
            compact_prompt_body()
        )
    );
}

/// #5956: the operator suffix is a clearly delimited section, and a manual
/// `/compact <focus>` still composes *after* it.
#[test]
fn compact_prompt_appends_operator_instructions_before_focus() {
    let prompt = compact_prompt(
        Some("the flaky test"),
        Some("Always restate open decisions."),
    );

    assert!(prompt.starts_with(&compact_prompt_body()));
    assert!(prompt.contains(OPERATOR_INSTRUCTIONS_HEADER));
    assert!(prompt.contains("Always restate open decisions."));
    assert!(prompt.contains(OPERATOR_INSTRUCTIONS_FOOTER));

    let instructions_at = prompt.find(OPERATOR_INSTRUCTIONS_HEADER).expect("section");
    let focus_at = prompt.find("focus on: the flaky test").expect("focus");
    assert!(
        instructions_at < focus_at,
        "the standing instructions come first; the one-off focus composes after them"
    );

    // The quality-retry prompt is the same summarizer call, so it carries the
    // same standing instructions.
    let retry = compact_quality_retry_prompt(None, Some("Always restate open decisions."));
    assert!(retry.contains("Always restate open decisions."));
    assert!(!compact_quality_retry_prompt(None, None).contains(OPERATOR_INSTRUCTIONS_HEADER));
}

/// #5956: an oversized standing instruction is truncated at the cap rather
/// than failing the compaction pass that keeps the session alive.
#[test]
fn operator_instructions_are_truncated_at_the_cap() {
    let max = codewhale_runtime::config::COMPACTION_SUMMARY_INSTRUCTIONS_MAX_CHARS;
    let long = "é".repeat(max + 500);
    let section = operator_instructions_section(Some(&long)).expect("section is present");

    let body = section
        .trim_start_matches('\n')
        .trim_start_matches(OPERATOR_INSTRUCTIONS_HEADER)
        .trim_start_matches('\n')
        .trim_end_matches(OPERATOR_INSTRUCTIONS_FOOTER)
        .trim_end_matches('\n');
    assert_eq!(body.chars().count(), max);
    assert!(operator_instructions_section(None).is_none());
    assert!(operator_instructions_section(Some("  ")).is_none());
}

/// #5956: the replacement history spends the configured verbatim budget, so a
/// larger budget keeps more of the user's own earlier messages.
#[test]
fn replacement_history_honours_the_configured_retention_budget() {
    let user = |text: &str| Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: text.to_string(),
            cache_control: None,
        }],
    };
    let assistant = |text: &str| Message {
        role: Role::Assistant,
        content: vec![ContentBlock::Text {
            text: text.to_string(),
            cache_control: None,
        }],
    };

    // Each older user message is ~1 000 conservative tokens (3 chars/token).
    let mut messages = Vec::new();
    for idx in 0..10 {
        messages.push(user(&format!("{idx}{}", "a".repeat(3_000))));
        messages.push(assistant("ack"));
    }
    messages.push(user("the live request"));
    messages.push(assistant("working on it"));

    let checkpoint = format!("{COMPACTION_SUMMARY_MARKER} and produced this handoff.");
    let count_verbatim = |budget: usize| {
        last_round::build_replacement_history(&messages, &checkpoint, None, budget, 128)
            .expect("replacement history")
            .len()
    };

    let small = count_verbatim(2_000);
    let large = count_verbatim(20_000);
    assert!(
        large > small,
        "a larger budget must keep more user messages verbatim ({small} vs {large})"
    );
    // The floor still keeps the last round plus the checkpoint.
    assert!(
        small >= 3,
        "the bounded last round and checkpoint always survive"
    );
}

/// #5956: the receipt clause names the effective budget and whether standing
/// operator instructions were applied, so the knob is verifiable without logs.
#[test]
fn receipt_clause_reports_the_effective_compaction_tuning() {
    let mut coverage = CompactionCoverage {
        path: CompactionPath::Summary,
        last_round_messages: 2,
        last_round_tool_results: 0,
        last_round_assistant: true,
        dropped_messages: 8,
        anchors_chars: 0,
        retained_user_message_tokens: 60_000,
        operator_instructions_applied: true,
    };
    let clause = coverage.receipt_clause();
    assert!(
        clause.contains("verbatim user budget 60000 tokens"),
        "{clause}"
    );
    assert!(clause.contains("operator instructions applied"), "{clause}");

    coverage.operator_instructions_applied = false;
    assert!(!coverage.receipt_clause().contains("operator instructions"));

    // The prune-only path builds no replacement history, so it reports no budget.
    let prune_only = CompactionCoverage {
        path: CompactionPath::PruneOnly,
        ..CompactionCoverage::default()
    };
    assert!(!prune_only.receipt_clause().contains("verbatim user budget"));
}

/// Both summarizer requests ask for the same layout, the fold-in rule and the
/// focus line, and neither is the text the note itself opens with.
#[test]
fn handoff_prompts_share_sections_fold_in_and_focus() {
    let first = compact_prompt(Some("the flaky test"), None);
    let retry = compact_quality_retry_prompt(Some("the flaky test"), None);
    for prompt in [&first, &retry] {
        for (heading, _) in HANDOFF_SECTIONS {
            assert!(
                prompt.contains(&format!("\n## {heading} - ")),
                "{heading}: {prompt}"
            );
        }
        assert!(prompt.contains(HANDOFF_FOLD_IN_RULE), "{prompt}");
        assert!(
            prompt.ends_with("The user asked this handoff to focus on: the flaky test"),
            "{prompt}"
        );
        assert!(prompt.contains(COMPACTION_LANGUAGE_CONTRACT));
        assert!(!prompt.contains(COMPACTION_SUMMARY_MARKER));
        assert!(!prompt.contains(LEGACY_V2_COMPACTION_SUMMARY_MARKER));
    }
    assert!(first.starts_with(COMPACT_PROMPT_OPENING));
    // Headings come in the documented order.
    let positions = HANDOFF_SECTIONS
        .iter()
        .map(|(heading, _)| first.find(&format!("\n## {heading} - ")).expect(heading))
        .collect::<Vec<_>>();
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
}

/// The note opens with the marker, restates no authority and ends with the
/// continuation line, so detection and the resume contract stay intact.
#[test]
fn handoff_note_opens_with_marker_and_closes_with_continuation() {
    let note = build_compaction_summary_block_text(
        "## Objective\nShip it",
        "\n\nUser-pinned anchors (verbatim):\nno force-push",
    );
    assert!(note.starts_with(COMPACTION_SUMMARY_MARKER));
    assert!(note.starts_with(SUMMARY_HEADER));
    assert!(note.contains("## Objective\nShip it"));
    assert!(note.contains("no force-push"));
    assert!(note.ends_with(SUMMARY_CLOSING));
    assert!(!note.contains(LEGACY_V2_COMPACTION_SUMMARY_MARKER));
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

/// A user message that quotes the new marker is ordinary user text: it is
/// retained, kept by restore, and never mistaken for a checkpoint.
#[test]
fn user_quote_of_new_marker_is_not_a_checkpoint() {
    let quote = text_message(
        Role::User,
        &format!("What is in the {COMPACTION_SUMMARY_MARKER}? {COMPACTION_SUMMARY_MARKER}."),
    );
    let header_quote = text_message(
        Role::User,
        &build_compaction_summary_block_text("pasted by the user", ""),
    );
    for message in [&quote, &header_quote] {
        assert!(!is_compaction_checkpoint_message(message));
        assert!(!is_wire_compaction_checkpoint_message(message));
    }

    let retained = retained_user_messages(&[quote.clone(), header_quote.clone()], 10_000);
    assert_eq!(retained, vec![quote.clone(), header_quote.clone()]);

    let summary = SystemPrompt::Text(build_compaction_summary_block_text("Summary", ""));
    let history = vec![
        quote.clone(),
        compaction_checkpoint_message(&summary),
        header_quote.clone(),
    ];
    let restored = restore_compaction_checkpoint(history.clone(), Some(&summary));
    assert_eq!(restored, history);

    // Without a typed checkpoint the legacy cleanup runs, and it still keeps
    // both quotes because the new marker is never matched by substring.
    let restored = restore_compaction_checkpoint(vec![quote.clone(), header_quote.clone()], None);
    assert_eq!(restored, vec![quote, header_quote]);
}

/// A checkpoint saved with the previous header and the provenance block is
/// still a wire checkpoint: it keeps its boundary on restore and is replaced,
/// not stacked, by the next pass.
#[test]
fn legacy_v2_checkpoint_with_provenance_is_recognised_and_replaced() {
    let old = compaction_checkpoint_message(&SystemPrompt::Text(format!(
        "{LEGACY_V2_COMPACTION_SUMMARY_MARKER} and produced a summary.\n\nold work"
    )));
    assert!(is_wire_compaction_checkpoint_message(&old));
    assert!(is_compaction_checkpoint_message(&old));

    let later = text_message(Role::User, "a later turn");
    let fresh = SystemPrompt::Text(build_compaction_summary_block_text("new work", ""));
    let restored = restore_compaction_checkpoint(
        vec![text_message(Role::User, "before"), old, later.clone()],
        Some(&fresh),
    );
    assert_eq!(restored.len(), 3);
    assert!(is_wire_compaction_checkpoint_message(&restored[1]));
    assert!(
        user_text_of(&restored[1]).is_some_and(|text| text.starts_with(COMPACTION_SUMMARY_MARKER))
    );
    assert_eq!(restored[2], later);

    let first = vec![
        text_message(Role::User, "Run the suite now."),
        text_message(Role::Assistant, "Rerunning."),
        compaction_checkpoint_message(&SystemPrompt::Text(format!(
            "{LEGACY_V2_COMPACTION_SUMMARY_MARKER}: first handoff"
        ))),
    ];
    let replaced = last_round::build_replacement_history(
        &first,
        &build_compaction_summary_block_text("second handoff", ""),
        None,
        COMPACT_RETAINED_USER_MESSAGE_MAX_TOKENS,
        20_000,
    )
    .expect("replacement");
    let checkpoints = replaced
        .iter()
        .filter(|message| is_compaction_checkpoint_message(message))
        .collect::<Vec<_>>();
    assert_eq!(checkpoints.len(), 1, "{replaced:?}");
    assert!(user_text_of(checkpoints[0]).is_some_and(|text| text.contains("second handoff")));
}

/// Single-block messages carrying either legacy marker predate provenance and
/// still count as checkpoints.
#[test]
fn legacy_single_block_markers_still_count_as_checkpoints() {
    for marker in [
        LEGACY_COMPACTION_SUMMARY_MARKER,
        LEGACY_V2_COMPACTION_SUMMARY_MARKER,
    ] {
        let legacy = text_message(Role::User, &format!("{marker}\nold summary"));
        assert!(is_compaction_checkpoint_message(&legacy), "{marker}");
        assert!(!is_wire_compaction_checkpoint_message(&legacy), "{marker}");
    }
}

#[test]
fn pruning_metadata_belongs_to_the_exact_execution() {
    let messages: Vec<Message> = serde_json::from_value(serde_json::json!([
        {"role":"assistant","content":[{"type":"tool_use","id":"reused","execution_id":"first","name":"read","input":{"path":"first.txt"}}]},
        {"role":"user","content":[{"type":"tool_result","tool_use_id":"reused","execution_id":"first","content":"A".repeat(20000)}]},
        {"role":"assistant","content":[{"type":"tool_use","id":"reused","execution_id":"second","name":"search","input":{"pattern":"second"}}]},
        {"role":"user","content":[{"type":"tool_result","tool_use_id":"reused","execution_id":"second","content":"B".repeat(20000)}]}
    ])).unwrap();
    let plan = plan_tool_result_prunes(&messages, 0);
    assert_eq!(plan.len(), 2);
    let first = plan.iter().find(|item| item.message_idx == 1).unwrap();
    assert!(first.summary.contains("first.txt"), "{}", first.summary);
    assert!(!first.summary.contains("second"), "{}", first.summary);
    let mut ambiguous = messages.clone();
    ambiguous.push(messages[0].clone());
    assert!(
        !plan_tool_result_prunes(&ambiguous, 0)
            .iter()
            .any(|item| item.message_idx == 1)
    );
}

#[test]
fn estimate_tokens_is_available_for_future_budgeting() {
    let messages = vec![Message {
        role: codewhale_models::Role::from("user"),
        content: vec![ContentBlock::Text {
            text: "src/main.rs".to_string(),
            cache_control: None,
        }],
    }];
    assert!(estimate_tokens(&messages) > 0);
}
