//! Host-bound diagnostics regressions relocated outside the future movable
//! `groups/debug` source closure. Selected by CW-SLICE's
//! `commands::debug_diagnostics` filter; mutation-only tests stay in the group.

use super::debug_mutation_host_tests::{create_test_app, test_tool};
use crate::client::CacheWarmupKey;
use crate::tui::app::{App, AppAction, TurnCacheRecord};
use crate::tui::history::HistoryCell;
use codewhale_commands::debug::cache_format::format_tokens;
use codewhale_models::Role;
use codewhale_models::{ContentBlock, Message, SystemBlock, SystemPrompt};
use std::time::Instant;

// Exercise the real registry/dispatcher, not the legacy debug group's
// mutation-only dispatch. The caller-supplied argument is unchanged after
// the one separator inserted by the public slash-command parser.
fn dispatch(
    app: &mut App,
    name: &str,
    arg: Option<&str>,
) -> Option<crate::commands::CommandResult> {
    let input = match arg {
        Some(arg) => format!("/{name} {arg}"),
        None => format!("/{name}"),
    };
    Some(super::execute(&input, app))
}

fn system_prompt(app: &mut App) -> crate::commands::CommandResult {
    dispatch(app, "system", None).expect("registered system command")
}

fn context(app: &mut App, arg: Option<&str>) -> crate::commands::CommandResult {
    dispatch(app, "context", arg).expect("registered context command")
}

fn cost(app: &mut App) -> crate::commands::CommandResult {
    dispatch(app, "cost", None).expect("registered cost command")
}

fn tokens(app: &mut App) -> crate::commands::CommandResult {
    dispatch(app, "tokens", None).expect("registered tokens command")
}

fn cache(app: &mut App, arg: Option<&str>) -> crate::commands::CommandResult {
    dispatch(app, "cache", arg).expect("registered cache command")
}

fn format_warmup_status(last: Option<&CacheWarmupKey>, current: &CacheWarmupKey) -> String {
    let previous = last.cloned().map(super::contract::project_debug_warmup_key);
    let projected = super::contract::project_debug_warmup_key(current.clone());
    codewhale_commands::debug::cache_format::format_warmup_status(
        previous.as_ref(),
        &projected,
        last.map(CacheWarmupKey::hash_short).as_deref(),
        &current.hash_short(),
    )
}

#[test]
fn test_tokens_shows_usage_info() {
    let mut app = create_test_app();
    app.session.total_tokens = 1234;
    app.session.session_cost = 0.05;
    app.session.last_prompt_tokens = Some(100);
    app.session.last_completion_tokens = Some(25);
    app.session.last_prompt_cache_hit_tokens = Some(70);
    app.session.last_prompt_cache_miss_tokens = Some(30);
    app.api_messages_mut().push(Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "test".to_string(),
            cache_control: None,
        }],
    });
    app.history.push(HistoryCell::User {
        content: "test".to_string(),
    });

    let result = tokens(&mut app);
    assert!(result.message.is_some());
    let msg = result.message.unwrap();
    assert!(msg.contains("Token Usage"));
    assert!(msg.contains("Active context:"));
    assert!(msg.contains("Last API input:"));
    assert!(msg.contains("Last API output:"));
    assert!(msg.contains("Cache hit/miss:"));
    assert!(msg.contains("70 hit / 30 miss"));
    assert!(msg.contains("Cumulative tokens:"));
    // Not "approx session cost": the figure is the priced subtotal, and a
    // session with no priced turn reports `unknown` rather than the raw
    // accumulator (#4318).
    assert!(msg.contains("Priced amount:"));
    assert!(msg.contains("Priced amount:         unknown"), "{msg}");
    assert!(msg.contains("API messages:"));
    assert!(msg.contains("Chat messages:"));
    assert!(msg.contains("Model:"));
}

#[test]
fn tokens_report_uses_codex_oauth_route_context() {
    let mut app = create_test_app();
    app.api_provider = codewhale_config::ProviderKind::OpenaiCodex;
    app.set_model_selection("gpt-5.5".to_string());
    app.active_route_limits = Some(codewhale_config::route::RouteLimits {
        context_tokens: Some(272_000),
        input_tokens: None,
        output_tokens: None,
        output_declared: false,
    });

    let message = tokens(&mut app).message.expect("tokens report");

    assert!(message.contains("/ 272000"), "{message}");
    assert!(!message.contains("1050000"), "{message}");
}

#[test]
fn test_cost_shows_spending_info() {
    let mut app = create_test_app();
    // A total is only reportable with the coverage that qualifies it. Setting
    // the accumulator alone is the legacy shape, checked separately below.
    app.session.cost_priced_turns = 1;
    app.session.session_cost = 0.1234;
    let result = cost(&mut app);
    assert!(result.message.is_some());
    let msg = result.message.unwrap();
    assert!(msg.contains("Session Cost"), "{msg}");
    assert!(msg.contains("Estimated total:"), "{msg}");
    assert!(msg.contains("$0.1234"), "{msg}");
    // The old copy hedged with "approximate" and then printed a static
    // "Provider API Pricing" rate card that was not what the number was
    // computed from. Both are gone: the report now names its own basis and
    // its coverage instead of gesturing at a price list (#4318).
    assert!(msg.contains("estimate, not a bill"), "{msg}");
    assert!(msg.contains("Covered: 1 of 1"), "{msg}");
    assert!(!msg.contains("Provider API Pricing"), "{msg}");
    assert!(!msg.contains("DeepSeek API Pricing"), "{msg}");
}

/// The same accumulator with no coverage behind it is not a total. It is the
/// exact state a pre-coverage session restores into, and reporting it as
/// "Estimated total: $0.1234" would claim the figure is complete.
#[test]
fn cost_report_will_not_promote_an_unqualified_accumulator_to_a_total() {
    let mut app = create_test_app();
    app.session.session_cost = 0.1234;

    let msg = cost(&mut app).message.expect("cost report");
    assert!(msg.contains("Estimated total: unknown"), "{msg}");
    assert!(!msg.contains("$0.1234"), "{msg}");
}

#[test]
fn cost_report_states_its_coverage_and_names_what_it_excludes() {
    use crate::pricing::audit_turn_cost_for_provider_at;

    let mut app = create_test_app();
    let write_heavy = codewhale_models::Usage {
        input_tokens: 1_000_000,
        output_tokens: 100_000,
        prompt_cache_hit_tokens: Some(200_000),
        prompt_cache_write_tokens: Some(100_000),
        ..Default::default()
    };
    let now = chrono::Utc::now();

    // One priced turn, one turn whose route publishes no cache-write rate, and
    // one subscription turn that is not money-metered at all.
    let priced = audit_turn_cost_for_provider_at(
        codewhale_config::ProviderKind::Anthropic,
        "claude-haiku-4-5",
        &write_heavy,
        now,
    );
    assert!(priced.is_priced(), "fixture must be priced");
    app.record_turn_cost_audit(&priced);
    app.accrue_session_cost_estimate(priced.estimate.expect("priced"));

    let unpriced = audit_turn_cost_for_provider_at(
        codewhale_config::ProviderKind::Moonshot,
        "kimi-k2.7-code",
        &write_heavy,
        now,
    );
    assert!(!unpriced.is_priced(), "fixture must fail closed");
    app.record_turn_cost_audit(&unpriced);

    let billing =
        crate::route_billing::for_dispatched_receipt(crate::route_billing::DispatchedReceipt {
            provider: codewhale_config::ProviderKind::OpenaiCodex,
            identity: Some("openai_codex"),
            base_url: "https://api.openai.com/v1",
            product: crate::route_billing::RouteProduct::Subscription("ChatGPT plan allowance"),
        });
    let oauth = crate::pricing::audit_turn_cost_for_route(
        codewhale_config::ProviderKind::OpenaiCodex,
        "gpt-5.5",
        None,
        &write_heavy,
        now,
        billing,
    );
    app.record_turn_cost_audit(&oauth);
    assert!(
        !app.session
            .cost_unpriced_reasons
            .contains("not_money_metered")
    );

    let msg = cost(&mut app).message.expect("cost report");

    // Non-metered routes are not counted as "incomplete dollars".
    assert!(msg.contains("Covered: 1 of 2"), "{msg}");
    assert!(msg.contains("estimate, not a bill"), "{msg}");
    assert!(msg.contains("Excluded: 1"), "{msg}");
    assert!(msg.contains("Priced subtotal:"), "{msg}");
    assert!(msg.contains("token class rate unavailable"), "{msg}");
    assert!(msg.contains("cache_write"), "{msg}");

    // A run with no unpriced turns says so without an exclusion note.
    let mut clean = create_test_app();
    clean.record_turn_cost_audit(&priced);
    let clean_msg = cost(&mut clean).message.expect("cost report");
    assert!(clean_msg.contains("Covered: 1 of 1"), "{clean_msg}");
    assert!(clean_msg.contains("Estimated total:"), "{clean_msg}");
    assert!(!clean_msg.contains("Excluded:"), "{clean_msg}");
    assert!(clean_msg.contains("estimate, not a bill"), "{clean_msg}");
    // Provenance of the row the total was built from is part of explaining it.
    assert!(clean_msg.contains("Pricing sources used:"), "{clean_msg}");
}

#[test]
fn cost_coverage_is_currency_specific_for_mixed_deepseek_openai() {
    let mut app = create_test_app();
    let usage = codewhale_models::Usage {
        input_tokens: 10_000,
        output_tokens: 1_000,
        ..Default::default()
    };
    let deepseek = crate::pricing::audit_turn_cost_for_provider_at(
        codewhale_config::ProviderKind::Deepseek,
        "deepseek-v4-flash",
        &usage,
        chrono::Utc::now(),
    );
    let openai = crate::pricing::audit_turn_cost_for_provider_at(
        codewhale_config::ProviderKind::Openai,
        "gpt-5.5",
        &usage,
        chrono::Utc::now(),
    );
    app.record_turn_cost_audit(&deepseek);
    app.record_turn_cost_audit(&openai);
    app.accrue_session_cost_estimate(deepseek.estimate.expect("DeepSeek priced"));
    app.accrue_session_cost_estimate(openai.estimate.expect("OpenAI priced"));

    app.cost_currency = codewhale_config::pricing::CostCurrency::Usd;
    let usd = cost(&mut app).message.expect("USD report");
    assert!(usd.contains("Covered: 2 of 2"), "{usd}");
    assert!(usd.contains("Estimated total:"), "{usd}");

    app.cost_currency = codewhale_config::pricing::CostCurrency::Cny;
    let cny = cost(&mut app).message.expect("CNY report");
    assert!(cny.contains("Covered: 1 of 2"), "{cny}");
    assert!(cny.contains("Priced subtotal:"), "{cny}");
}

/// A session restored from a pre-coverage save has real money and no evidence of
/// what it covers. `/cost` must say the coverage is unknown rather than render a
/// fabricated "0 of 0 priced", which would assert the total is complete (#4318).
#[test]
fn cost_report_shows_unknown_coverage_for_a_legacy_session_instead_of_zero_of_zero() {
    let mut app = create_test_app();
    app.session.session_cost = 1.25;
    app.session.cost_coverage_unknown_legacy = true;

    let msg = cost(&mut app).message.expect("cost report");
    assert!(msg.contains("Coverage: unknown"), "{msg}");
    assert!(
        !msg.contains("Covered: 0 of 0"),
        "a legacy session must never claim a complete zero-turn total: {msg}"
    );
    assert!(msg.contains("estimate, not a bill"), "{msg}");
}

#[test]
fn cost_report_distinguishes_unknown_from_authoritatively_priced_zero() {
    let mut unknown = create_test_app();
    let unknown_msg = cost(&mut unknown).message.expect("unknown report");
    assert!(
        unknown_msg.contains("Estimated total: unknown"),
        "{unknown_msg}"
    );
    assert!(!unknown_msg.contains("$0.0000"), "{unknown_msg}");

    let mut priced_zero = create_test_app();
    priced_zero.session.cost_priced_turns = 1;
    let zero_msg = cost(&mut priced_zero).message.expect("priced zero report");
    assert!(zero_msg.contains("$0.0000"), "{zero_msg}");
    assert!(!zero_msg.contains("<$0.0001"), "{zero_msg}");
}

#[test]
fn cost_report_distinguishes_bundled_fallback_from_no_usable_fallback() {
    let mut app = create_test_app();
    app.session.cost_priced_turns = 1;
    app.session
        .cost_live_pricing_defects
        .insert("live_pricing_stale".to_string());
    app.session
        .cost_live_pricing_unusable_defects
        .insert("live_pricing_scope_mismatch".to_string());

    let msg = cost(&mut app).message.expect("cost report");
    assert!(msg.contains("bundled published rates were used"), "{msg}");
    assert!(
        msg.contains("no usable bundled rate was available"),
        "{msg}"
    );
    assert!(msg.contains("this spend is unknown"), "{msg}");
}

#[test]
fn all_zero_legacy_coverage_stays_unknown_when_rendered_and_resaved() {
    let mut app = create_test_app();
    let legacy: crate::session_manager::SessionCostSnapshot =
        serde_json::from_value(serde_json::json!({
            "session_cost_usd": 0.0,
            "session_cost_cny": 0.0
        }))
        .expect("legacy zero snapshot");
    assert!(legacy.coverage_is_legacy_unknown());
    app.session.cost_coverage_unknown_legacy = true;

    let msg = cost(&mut app).message.expect("cost report");
    assert!(msg.contains("Coverage: unknown"), "{msg}");
    assert!(!msg.contains("Covered: 0 of 0"), "{msg}");

    let mut metadata = crate::session_manager::create_saved_session_with_id_and_mode(
        "legacy-zero".to_string(),
        &[],
        "deepseek-v4-flash",
        std::path::Path::new("/tmp"),
        0,
        None,
        None,
    )
    .metadata;
    app.sync_cost_to_metadata(&mut metadata);
    assert!(!metadata.cost.coverage_recorded);
    assert!(metadata.cost.coverage_is_legacy_unknown());
}

/// Loading a session must not leave the previous session's coverage counters
/// attached to a total that no longer contains those turns.
#[test]
fn reset_cost_coverage_clears_every_counter() {
    use crate::pricing::audit_turn_cost_for_provider_at;

    let mut app = create_test_app();
    let usage = codewhale_models::Usage {
        input_tokens: 1_000_000,
        output_tokens: 100_000,
        prompt_cache_write_tokens: Some(100_000),
        ..Default::default()
    };
    let now = chrono::Utc::now();
    app.record_turn_cost_audit(&audit_turn_cost_for_provider_at(
        codewhale_config::ProviderKind::Anthropic,
        "claude-haiku-4-5",
        &usage,
        now,
    ));
    app.record_turn_cost_audit(&audit_turn_cost_for_provider_at(
        codewhale_config::ProviderKind::Moonshot,
        "kimi-k2.7-code",
        &usage,
        now,
    ));
    app.record_turn_cost_route_receipt("provider=anthropic model=x".to_string());
    app.session.cost_coverage_unknown_legacy = true;
    assert_ne!(app.session.cost_priced_turns, 0);
    assert_ne!(app.session.cost_unpriced_turns, 0);
    assert!(!app.session.cost_unpriced_reasons.is_empty());
    assert!(!app.session.cost_unpriced_classes.is_empty());
    assert!(!app.session.cost_pricing_provenances.is_empty());
    assert!(!app.session.cost_route_receipts.is_empty());

    app.reset_cost_coverage();

    assert_eq!(app.session.cost_priced_turns, 0);
    assert_eq!(app.session.cost_unpriced_turns, 0);
    assert!(app.session.cost_unpriced_reasons.is_empty());
    assert!(app.session.cost_cny_unpriced_reasons.is_empty());
    assert!(app.session.cost_unpriced_classes.is_empty());
    assert!(app.session.cost_pricing_provenances.is_empty());
    assert!(app.session.cost_live_pricing_defects.is_empty());
    assert!(app.session.cost_live_pricing_unusable_defects.is_empty());
    assert!(app.session.cost_route_receipts.is_empty());
    assert!(!app.session.cost_coverage_unknown_legacy);
    // With nothing recorded, `/cost` reports an honest empty coverage rather
    // than the legacy-unknown state.
    let msg = cost(&mut app).message.expect("cost report");
    assert!(msg.contains("Covered: 0 of 0"), "{msg}");
    assert!(!msg.contains("Coverage: unknown"), "{msg}");
}

/// `/tokens` quotes the same total as `/cost`, so it carries the same estimate
/// disclaimer and the same coverage state, and reports cache-write with a
/// pointer to `/cache` for the per-turn detail.
#[test]
fn tokens_report_says_estimate_and_exposes_coverage_and_cache_write() {
    let mut app = create_test_app();
    app.session.total_cache_write_tokens = 250_000;
    app.record_turn_cost_audit(&crate::pricing::audit_turn_cost_for_provider_at(
        codewhale_config::ProviderKind::Moonshot,
        "kimi-k2.7-code",
        &codewhale_models::Usage {
            input_tokens: 1_000_000,
            output_tokens: 100_000,
            prompt_cache_write_tokens: Some(100_000),
            ..Default::default()
        },
        chrono::Utc::now(),
    ));

    let msg = tokens(&mut app).message.expect("tokens report");
    assert!(msg.contains("estimate, not a bill"), "{msg}");
    assert!(msg.contains("Covered: 0 of 1"), "{msg}");
    assert!(msg.contains("Excluded: 1"), "{msg}");
    assert!(msg.contains("250000"), "{msg}");
    // The cache-write line links to /cache rather than duplicating the table.
    assert!(msg.contains("/cache"), "{msg}");
}

#[test]
fn test_system_prompt_displays_text() {
    let mut app = create_test_app();
    app.system_prompt = Some(SystemPrompt::Text("Test system prompt".to_string()));
    let result = system_prompt(&mut app);
    assert!(result.message.is_some());
    let msg = result.message.unwrap();
    assert!(msg.contains("System Prompt"));
    assert!(msg.contains("Test system prompt"));
}

#[test]
fn test_system_prompt_displays_blocks() {
    let mut app = create_test_app();
    app.system_prompt = Some(SystemPrompt::Blocks(vec![
        SystemBlock {
            block_type: "text".to_string(),
            text: "Block 1".to_string(),
            cache_control: None,
        },
        SystemBlock {
            block_type: "text".to_string(),
            text: "Block 2".to_string(),
            cache_control: None,
        },
    ]));
    let result = system_prompt(&mut app);
    assert!(result.message.is_some());
    let msg = result.message.unwrap();
    assert!(msg.contains("System Prompt"));
    assert!(msg.contains("Block 1"));
    assert!(msg.contains("Block 2"));
}

#[test]
fn test_system_prompt_none() {
    let mut app = create_test_app();
    app.system_prompt = None;
    let result = system_prompt(&mut app);
    assert!(result.message.is_some());
    let msg = result.message.unwrap();
    assert!(msg.contains("(no system prompt)"));
}

#[test]
fn test_system_prompt_truncates_long_text() {
    let mut app = create_test_app();
    let long_text = "x".repeat(600);
    app.system_prompt = Some(SystemPrompt::Text(long_text));
    let result = system_prompt(&mut app);
    assert!(result.message.is_some());
    let msg = result.message.unwrap();
    assert!(msg.contains("..."));
    assert!(msg.contains("chars total"));
}

#[test]
fn cache_command_reports_no_data_before_first_turn() {
    let mut app = create_test_app();
    let result = cache(&mut app, None);
    let msg = result.message.expect("cache produces a message");
    assert!(msg.contains("no turns recorded yet"), "got: {msg}");
}

#[test]
fn cache_inspect_reports_hashes_without_prompt_text() {
    let mut app = create_test_app();
    app.system_prompt = Some(SystemPrompt::Text(
            "Base policy\n\n<project_instructions source=\"AGENTS.md\">\nSECRET_PROJECT_RULE\n</project_instructions>"
                .to_string(),
        ));
    app.api_messages_mut().push(Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "SECRET_USER_TASK".to_string(),
            cache_control: None,
        }],
    });

    let result = cache(&mut app, Some("inspect"));
    let msg = result.message.expect("inspect output");

    assert!(msg.contains("Cache Inspect"));
    assert!(msg.contains("Base static prefix hash:"));
    assert!(msg.contains("Full request prefix hash:"));
    assert!(msg.contains("Static base prefix stability: no previous request"));
    assert!(msg.contains("First divergence from previous request: unavailable"));
    assert!(msg.contains("Global system prefix: static"));
    assert!(msg.contains("Project context: static"));
    assert!(msg.contains("User task: dynamic"));
    assert!(!msg.contains("SECRET_PROJECT_RULE"));
    assert!(!msg.contains("SECRET_USER_TASK"));
}

#[test]
fn cache_inspect_uses_last_request_tool_catalog() {
    let mut app = create_test_app();
    app.system_prompt = Some(SystemPrompt::Text("Base policy".to_string()));
    app.session.last_tool_catalog = Some(vec![test_tool("read_file")]);
    app.api_messages_mut().push(Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "Current task".to_string(),
            cache_control: None,
        }],
    });

    let msg = cache(&mut app, Some("inspect"))
        .message
        .expect("inspect output");

    assert!(msg.contains("Tool catalog hash: "), "got: {msg}");
    assert!(!msg.contains("(no tools registered)"), "got: {msg}");
    assert!(msg.contains("Tool catalog: static"), "got: {msg}");
    assert!(msg.contains("bytes="), "got: {msg}");
    assert!(msg.contains("~"), "got: {msg}");
}

#[test]
fn cache_inspect_json_reports_tool_catalog_hash_and_layer_sizes() {
    let mut app = create_test_app();
    app.system_prompt = Some(SystemPrompt::Text("Base policy".to_string()));
    app.session.last_tool_catalog = Some(vec![test_tool("read_file")]);
    app.api_messages_mut().push(Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "Current task".to_string(),
            cache_control: None,
        }],
    });

    let msg = cache(&mut app, Some("inspect --json"))
        .message
        .expect("inspect json output");
    let parsed: serde_json::Value = serde_json::from_str(&msg).expect("valid json");

    assert_eq!(parsed["tool_catalog_hash"].as_str().unwrap().len(), 64);
    assert!(
        parsed["warmup_status"]
            .as_str()
            .is_some_and(|status| status.starts_with("Warmup status: no previous warmup"))
    );
    assert!(parsed["current_warmup_key"].is_object());
    let tool_layer = parsed["layers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|layer| layer["name"] == "Tool catalog")
        .expect("tool catalog layer");
    assert!(tool_layer["byte_len"].as_u64().unwrap() > 0);
    assert!(tool_layer["token_estimate"].as_u64().unwrap() > 0);
}

#[test]
fn cache_inspect_json_keys_auto_replay_to_the_last_concrete_route() {
    let mut app = create_test_app();
    app.model = "auto".to_string();
    app.auto_model = true;
    app.reasoning_effort = crate::reasoning_preference::ReasoningEffort::Off;
    app.last_effective_provider = Some(codewhale_config::ProviderKind::OpenaiCodex);
    app.last_effective_provider_identity = Some(
        codewhale_config::ProviderKind::OpenaiCodex
            .as_str()
            .to_string(),
    );
    app.last_effective_model =
        Some(codewhale_config::descriptors::defaults::DEFAULT_OPENAI_CODEX_MODEL.to_string());
    app.session.last_base_url =
        Some(codewhale_config::descriptors::defaults::DEFAULT_OPENAI_CODEX_BASE_URL.to_string());
    app.push_turn_cache_record(TurnCacheRecord {
        provider: Some(codewhale_config::ProviderKind::OpenaiCodex),
        provider_identity: Some(
            codewhale_config::ProviderKind::OpenaiCodex
                .as_str()
                .to_string(),
        ),
        model: Some(
            codewhale_config::descriptors::defaults::DEFAULT_OPENAI_CODEX_MODEL.to_string(),
        ),
        auto_model: true,
        input_tokens: 1,
        output_tokens: 1,
        cache_hit_tokens: None,
        cache_miss_tokens: None,
        cache_write_tokens: None,
        reasoning_tokens: None,
        cost_audit: None,
        reasoning_replay_tokens: None,
        recorded_at: Instant::now(),
    });

    let message = cache(&mut app, Some("inspect --json"))
        .message
        .expect("inspect json output");
    let parsed: serde_json::Value = serde_json::from_str(&message).expect("valid json");
    let key = &parsed["current_warmup_key"];

    assert_eq!(
        key["provider"],
        codewhale_config::ProviderKind::OpenaiCodex.as_str()
    );
    assert_eq!(
        key["model"],
        codewhale_config::descriptors::defaults::DEFAULT_OPENAI_CODEX_MODEL
    );
    assert_eq!(
        key["base_url"],
        codewhale_config::descriptors::defaults::DEFAULT_OPENAI_CODEX_BASE_URL
    );
}

fn warmup_key(model: &str, static_hash: &str) -> CacheWarmupKey {
    CacheWarmupKey {
        provider: "Deepseek".to_string(),
        model: model.to_string(),
        base_url: "https://api.deepseek.com".to_string(),
        static_prefix_hash: static_hash.to_string(),
        tool_catalog_hash: "tool".to_string(),
        project_pack_hash: "project".to_string(),
        skills_hash: "skills".to_string(),
    }
}

#[test]
fn warmup_status_reports_valid_matching_key() {
    let key = warmup_key("deepseek-v4-pro", "static-a");
    let result = format_warmup_status(Some(&key), &key);
    assert!(result.contains("Warmup status: valid"), "got: {result}");
}

#[test]
fn warmup_status_reports_invalidation_reason() {
    let previous = warmup_key("deepseek-v4-pro", "static-a");
    let current = warmup_key("deepseek-v4-flash", "static-b");
    let result = format_warmup_status(Some(&previous), &current);
    assert!(result.contains("Warmup status: invalid"), "got: {result}");
    assert!(result.contains("model changed"), "got: {result}");
    assert!(result.contains("static prefix changed"), "got: {result}");
}

#[test]
fn warmup_status_reports_project_and_skills_reasons() {
    let previous = warmup_key("deepseek-v4-pro", "static-a");
    let mut current = previous.clone();
    current.project_pack_hash = "project-b".to_string();
    current.skills_hash = "skills-b".to_string();

    let result = format_warmup_status(Some(&previous), &current);

    assert!(result.contains("project pack changed"), "got: {result}");
    assert!(result.contains("skills changed"), "got: {result}");
    assert!(!result.contains("; )"), "got: {result}");
}

#[test]
fn cache_inspect_rejects_json_verbose_combo() {
    let mut app = create_test_app();
    let msg = cache(&mut app, Some("inspect --json --verbose"))
        .message
        .expect("inspect output");

    assert_eq!(
        msg,
        "cache inspect: --json and --verbose cannot be combined"
    );
}

#[test]
fn cache_inspect_json_uses_cjk_aware_token_estimate() {
    let mut app = create_test_app();
    app.system_prompt = Some(SystemPrompt::Text("缓存命中测试".to_string()));

    let msg = cache(&mut app, Some("inspect --json"))
        .message
        .expect("inspect json output");
    let parsed: serde_json::Value = serde_json::from_str(&msg).expect("valid json");
    let system_layer = parsed["layers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|layer| layer["name"] == "Global system prefix")
        .expect("system layer");

    assert_eq!(
        system_layer["token_estimate"].as_u64(),
        system_layer["char_len"].as_u64()
    );
}

#[test]
fn cache_inspect_reports_divergence_from_previous_request() {
    let mut app = create_test_app();
    app.system_prompt = Some(SystemPrompt::Text(
        "Base policy\n\n## Environment\n\n- shell: powershell".to_string(),
    ));
    app.api_messages_mut().push(Message {
        role: Role::Assistant,
        content: vec![codewhale_models::ContentBlock::Text {
            text: "Prior answer".to_string(),
            cache_control: None,
        }],
    });
    app.api_messages_mut().push(Message {
        role: Role::User,
        content: vec![codewhale_models::ContentBlock::Text {
            text: "First task".to_string(),
            cache_control: None,
        }],
    });

    let first = cache(&mut app, Some("inspect"))
        .message
        .expect("first inspect output");
    assert!(first.contains("Static base prefix stability: no previous request"));

    if let Some(last) = app.api_messages_mut().last_mut()
        && let Some(codewhale_models::ContentBlock::Text { text, .. }) = last.content.first_mut()
    {
        *text = "Second task".to_string();
    }

    let second = cache(&mut app, Some("inspect"))
        .message
        .expect("second inspect output");
    assert!(second.contains("Static base prefix stability: OK"));
    assert!(second.contains("First divergence from previous request: User task"));
    assert!(second.contains("Message #1 assistant: history"));
}

fn push_repeated_shell_results(app: &mut App, output: &str) {
    for id in ["tool-1", "tool-2"] {
        app.api_messages_mut().push(Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                execution_id: None,
                id: id.to_string(),
                name: "shell_command".to_string(),
                input: serde_json::json!({"command": "cargo test"}),
                caller: None,
                thought_signature: None,
            }],
        });
        app.api_messages_mut().push(Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                execution_id: None,
                tool_use_id: id.to_string(),
                content: output.to_string(),
                is_error: None,
                content_blocks: None,
            }],
        });
    }
}

#[test]
fn cache_inspect_displays_tool_result_budget_metadata() {
    // Past the 100,000-char wire backstop (#6508), so each sighting is
    // excerpted rather than sent whole or deduplicated.
    let mut app = create_test_app();
    let long_output = format!("{}{}", "A".repeat(60_000), "Z".repeat(60_000));
    push_repeated_shell_results(&mut app, &long_output);

    let result = cache(&mut app, Some("inspect"));
    let msg = result.message.expect("inspect output");

    let tool_budget_lines: Vec<_> = msg
        .lines()
        .filter(|line| line.contains("original_chars=120000"))
        .collect();
    assert_eq!(tool_budget_lines.len(), 2, "got: {msg}");

    for sighting in tool_budget_lines {
        assert!(sighting.contains("sent_chars="), "got: {msg}");
        assert!(!sighting.contains("sent_chars=120000"), "got: {msg}");
        assert!(sighting.contains("truncated=true"), "got: {msg}");
        assert!(sighting.contains("deduplicated=false"), "got: {msg}");
    }
}

#[test]
fn cache_inspect_shows_repeated_in_budget_tool_result_sent_whole_then_deduplicated() {
    // Under the wire backstop (#6508), a repeated result is sent whole the
    // first time and replaced by a reference to that copy the second time.
    let mut app = create_test_app();
    let output = format!("{}{}", "A".repeat(7_000), "Z".repeat(7_000));
    push_repeated_shell_results(&mut app, &output);

    let result = cache(&mut app, Some("inspect"));
    let msg = result.message.expect("inspect output");

    let lines: Vec<_> = msg
        .lines()
        .filter(|line| line.contains("original_chars=14000"))
        .collect();
    assert_eq!(lines.len(), 2, "got: {msg}");
    assert!(lines[0].contains("sent_chars=14000"), "got: {msg}");
    assert!(lines[0].contains("truncated=false"), "got: {msg}");
    assert!(lines[0].contains("deduplicated=false"), "got: {msg}");
    assert!(lines[1].contains("truncated=false"), "got: {msg}");
    assert!(lines[1].contains("deduplicated=true"), "got: {msg}");
    assert!(!lines[1].contains("sent_chars=14000"), "got: {msg}");
}

#[test]
fn cache_inspect_displays_turn_meta_dedup_metadata() {
    let mut app = create_test_app();
    let turn_meta = format!(
        "<turn_meta>\nCurrent local date: 2026-05-09\n{}\n</turn_meta>",
        "Working set: src/lib.rs\n".repeat(20)
    );
    app.api_messages_mut().push(Message {
        role: Role::User,
        content: vec![
            ContentBlock::Text {
                text: turn_meta.clone(),
                cache_control: None,
            },
            ContentBlock::Text {
                text: "first task".to_string(),
                cache_control: None,
            },
        ],
    });
    app.api_messages_mut().push(Message {
        role: Role::User,
        content: vec![
            ContentBlock::Text {
                text: turn_meta,
                cache_control: None,
            },
            ContentBlock::Text {
                text: "second task".to_string(),
                cache_control: None,
            },
        ],
    });

    let result = cache(&mut app, Some("inspect"));
    let msg = result.message.expect("inspect output");

    assert!(msg.contains("turn_meta_original_chars="), "got: {msg}");
    assert!(msg.contains("turn_meta_sent_chars="), "got: {msg}");
    assert!(msg.contains("turn_meta_deduplicated=false"), "got: {msg}");
    assert!(msg.contains("turn_meta_deduplicated=true"), "got: {msg}");
    assert!(msg.contains("turn_meta_sha256="), "got: {msg}");
    assert!(!msg.contains("Working set: src/lib.rs"), "got: {msg}");
}

#[test]
fn cache_command_renders_recorded_turns_with_ratio() {
    let mut app = create_test_app();
    let now = Instant::now();
    // Three turns: 75% hit, 50% hit, miss-only (provider didn't report hit).
    app.push_turn_cache_record(TurnCacheRecord {
        provider: Some(codewhale_config::ProviderKind::Deepseek),
        provider_identity: Some("deepseek".to_string()),
        model: Some("deepseek-v4-pro".to_string()),
        auto_model: true,
        input_tokens: 4_000,
        output_tokens: 200,
        cache_hit_tokens: Some(3_000),
        cache_miss_tokens: Some(1_000),
        reasoning_replay_tokens: None,
        cache_write_tokens: None,
        reasoning_tokens: None,
        cost_audit: None,
        recorded_at: now,
    });
    app.push_turn_cache_record(TurnCacheRecord {
        provider: None,
        provider_identity: None,
        model: None,
        auto_model: false,
        input_tokens: 6_000,
        output_tokens: 250,
        cache_hit_tokens: Some(3_000),
        cache_miss_tokens: Some(3_000),
        reasoning_replay_tokens: Some(150),
        cache_write_tokens: None,
        reasoning_tokens: None,
        cost_audit: None,
        recorded_at: now,
    });
    // Turn 3: hit reported but provider didn't report miss separately —
    // infer miss = input − hit and mark with `*`.
    app.push_turn_cache_record(TurnCacheRecord {
        provider: None,
        provider_identity: None,
        model: None,
        auto_model: false,
        input_tokens: 5_000,
        output_tokens: 100,
        cache_hit_tokens: Some(2_500),
        cache_miss_tokens: None,
        reasoning_replay_tokens: None,
        cache_write_tokens: None,
        reasoning_tokens: None,
        cost_audit: None,
        recorded_at: now,
    });
    // Turn 4: no telemetry at all — must not pollute aggregate ratios.
    app.push_turn_cache_record(TurnCacheRecord {
        provider: None,
        provider_identity: None,
        model: None,
        auto_model: false,
        input_tokens: 1_000,
        output_tokens: 50,
        cache_hit_tokens: None,
        cache_miss_tokens: None,
        reasoning_replay_tokens: None,
        cache_write_tokens: None,
        reasoning_tokens: None,
        cost_audit: None,
        recorded_at: now,
    });

    let result = cache(&mut app, None);
    let msg = result.message.expect("cache produces a message");

    // Header reflects total rows and model.
    assert!(msg.contains("last 4 of 4 turn(s)"), "got: {msg}");
    // Per-turn ratios are rendered.
    assert!(msg.contains("75.0%"), "got: {msg}");
    assert!(msg.contains("50.0%"), "got: {msg}");
    assert!(msg.contains("auto:deepseek/deepsee..."), "got: {msg}");
    // Turn 3: hit=2500, inferred miss=2500 → 50.0% with `*`-marked miss.
    assert!(msg.contains("2500*"), "got: {msg}");
    // Turn 4 (no telemetry) shows em-dashes and is excluded from totals.
    // Aggregate over turns 1-3: hit=8500, miss=6500 → 56.7%.
    assert!(msg.contains("avg hit ratio: 56.7%"), "got: {msg}");
    // Footer guidance is present.
    assert!(msg.contains("70%"), "got: {msg}");
}

#[test]
fn cache_history_shows_cache_write_tokens_and_explains_unpriced_turns() {
    use crate::pricing::audit_turn_cost_for_provider_at;

    let mut app = create_test_app();
    let write_heavy = codewhale_models::Usage {
        input_tokens: 1_000_000,
        output_tokens: 100_000,
        prompt_cache_hit_tokens: Some(200_000),
        prompt_cache_write_tokens: Some(100_000),
        ..Default::default()
    };
    let now = chrono::Utc::now();

    app.push_turn_cache_record(TurnCacheRecord {
        provider: Some(codewhale_config::ProviderKind::Anthropic),
        provider_identity: None,
        model: Some("claude-haiku-4-5".to_string()),
        auto_model: false,
        input_tokens: 1_000_000,
        output_tokens: 100_000,
        cache_hit_tokens: Some(200_000),
        cache_miss_tokens: Some(700_000),
        reasoning_replay_tokens: None,
        cache_write_tokens: Some(100_000),
        reasoning_tokens: Some(40_000),
        cost_audit: Some(audit_turn_cost_for_provider_at(
            codewhale_config::ProviderKind::Anthropic,
            "claude-haiku-4-5",
            &write_heavy,
            now,
        )),
        recorded_at: Instant::now(),
    });
    app.push_turn_cache_record(TurnCacheRecord {
        provider: Some(codewhale_config::ProviderKind::Moonshot),
        provider_identity: None,
        model: Some("kimi-k2.7-code".to_string()),
        auto_model: false,
        input_tokens: 1_000_000,
        output_tokens: 100_000,
        cache_hit_tokens: Some(200_000),
        cache_miss_tokens: Some(700_000),
        reasoning_replay_tokens: None,
        cache_write_tokens: Some(100_000),
        reasoning_tokens: Some(10_000),
        cost_audit: Some(audit_turn_cost_for_provider_at(
            codewhale_config::ProviderKind::Moonshot,
            "kimi-k2.7-code",
            &write_heavy,
            now,
        )),
        recorded_at: Instant::now(),
    });

    let msg = cache(&mut app, None).message.expect("cache message");

    assert!(msg.contains("write"), "{msg}");
    assert!(msg.contains("sum_write: 200000"), "{msg}");
    assert!(msg.contains("sum_reasoning: 50000"), "{msg}");
    // The priced turn shows money; the unpriced one shows why it does not.
    assert!(msg.contains("$1.3450"), "{msg}");
    assert!(msg.contains("token class rate unavailable"), "{msg}");
    assert!(msg.contains("cache_write"), "{msg}");
}

#[test]
fn cache_command_replays_reported_1177_low_hit_fixture() {
    let mut app = create_test_app();
    let now = Instant::now();
    // Fixture from #1177 / douglarek's 2026-05-10 `/cache` report.
    // It captures a real low-hit sequence with one 56.8% tail turn.
    for (input, output, hit, miss) in [
        (25_839, 12, 4_608, 21_231),
        (25_906, 288, 25_728, 178),
        (264_500, 2_528, 235_648, 28_852),
        (202_230, 3_191, 193_536, 8_694),
        (45_982, 294, 26_112, 19_870),
    ] {
        app.push_turn_cache_record(TurnCacheRecord {
            provider: None,
            provider_identity: None,
            model: None,
            auto_model: false,
            input_tokens: input,
            output_tokens: output,
            cache_hit_tokens: Some(hit),
            cache_miss_tokens: Some(miss),
            reasoning_replay_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            cost_audit: None,
            recorded_at: now,
        });
    }

    let result = cache(&mut app, None);
    let msg = result.message.expect("cache produces a message");

    assert!(msg.contains("last 5 of 5 turn(s)"), "got: {msg}");
    assert!(msg.contains("56.8%"), "got: {msg}");
    assert!(msg.contains("Σ in: 564457"), "got: {msg}");
    assert!(msg.contains("Σ hit: 485632"), "got: {msg}");
    assert!(msg.contains("Σ miss: 78825"), "got: {msg}");
    assert!(msg.contains("avg hit ratio: 86.0%"), "got: {msg}");
}

#[test]
fn cache_command_count_argument_clamps_to_history() {
    let mut app = create_test_app();
    for _ in 0..3 {
        app.push_turn_cache_record(TurnCacheRecord {
            provider: None,
            provider_identity: None,
            model: None,
            auto_model: false,
            input_tokens: 1_000,
            output_tokens: 100,
            cache_hit_tokens: Some(500),
            cache_miss_tokens: Some(500),
            reasoning_replay_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            cost_audit: None,
            recorded_at: Instant::now(),
        });
    }
    let result = cache(&mut app, Some("100"));
    let msg = result.message.expect("cache produces a message");
    // Asked for 100 turns, only 3 exist — should report "last 3 of 3".
    assert!(msg.contains("last 3 of 3 turn(s)"), "got: {msg}");
}

#[test]
fn turn_cache_history_is_capped_at_50() {
    let mut app = create_test_app();
    for i in 0..(crate::tui::app::App::TURN_CACHE_HISTORY_CAP + 12) {
        app.push_turn_cache_record(TurnCacheRecord {
            provider: None,
            provider_identity: None,
            model: None,
            auto_model: false,
            input_tokens: i as u32,
            output_tokens: 1,
            cache_hit_tokens: Some(i as u32),
            cache_miss_tokens: Some(0),
            reasoning_replay_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            cost_audit: None,
            recorded_at: Instant::now(),
        });
    }
    assert_eq!(
        app.session.turn_cache_history.len(),
        crate::tui::app::App::TURN_CACHE_HISTORY_CAP
    );
    // Oldest record was evicted; newest record is still at the back.
    assert_eq!(
        app.session.turn_cache_history.back().unwrap().input_tokens,
        (crate::tui::app::App::TURN_CACHE_HISTORY_CAP + 11) as u32
    );
}

#[test]
fn test_context_shows_usage_stats() {
    let mut app = create_test_app();
    app.api_messages_mut().push(Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "Hello".to_string(),
            cache_control: None,
        }],
    });
    app.history.push(HistoryCell::User {
        content: "Hello".to_string(),
    });

    let result = context(&mut app, None);
    assert!(matches!(
        result.action,
        Some(AppAction::OpenContextInspector)
    ));
    assert!(result.message.is_none());
}

#[test]
fn test_context_report_subcommands_return_source_map() {
    // The source map reads the user's global instructions and skills: seal the
    // home so the report does not change with whoever runs the suite.
    let _home = crate::test_support::SealedHome::new();
    let mut app = create_test_app();
    app.api_messages_mut().push(Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "Hello".to_string(),
            cache_control: None,
        }],
    });
    app.session.last_tool_catalog = Some(vec![test_tool("read_file")]);

    let report = context(&mut app, Some("report"))
        .message
        .expect("report text");
    assert!(report.contains("Context Source Map"));
    assert!(report.contains("Tool schemas"));

    let summary = context(&mut app, Some("summary"))
        .message
        .expect("summary text");
    assert!(summary.contains("Context Summary"));

    let json = context(&mut app, Some("json")).message.expect("json text");
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid context json");
    assert!(!parsed["entries"].as_array().unwrap().is_empty());

    let prompt_json = context(&mut app, Some("prompt-json"))
        .message
        .expect("prompt context json");
    let prompt: serde_json::Value =
        serde_json::from_str(&prompt_json).expect("valid prompt context json");
    assert_eq!(prompt["schema_version"], 1);
    assert_eq!(prompt["model"], app.model);
    assert_eq!(prompt["system_prompt_state"], "current_session");
    assert_eq!(prompt["tool_catalog_state"], "last_sent");
    assert_eq!(prompt["tools"][0]["name"], "read_file");
    assert!(prompt["sections"].is_array());
    assert!(
        !prompt["source_map"]["entries"]
            .as_array()
            .expect("source entries")
            .is_empty()
    );
}

#[test]
fn cache_stats_no_data_before_first_turn() {
    let mut app = create_test_app();
    let result = cache(&mut app, Some("stats"));
    let msg = result.message.expect("cache stats produces a message");
    assert!(msg.contains("Cache Stats"), "got: {msg}");
    assert!(
        msg.contains("unknown (no checks recorded yet)"),
        "got: {msg}"
    );
    assert!(msg.contains("Pinned hash: unavailable"), "got: {msg}");
    assert!(msg.contains("No turn telemetry recorded yet"), "got: {msg}");
}

#[test]
fn cache_stats_shows_stable_prefix_with_hash() {
    let mut app = create_test_app();
    app.prefix_stability_pct = Some(100);
    app.prefix_checks_total = 5;
    app.prefix_change_count = 0;
    app.last_pinned_prefix_hash =
        Some("a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2".to_string());

    let result = cache(&mut app, Some("stats"));
    let msg = result.message.expect("cache stats produces a message");

    assert!(msg.contains("Stability: 100%"), "got: {msg}");
    assert!(msg.contains("stable (no prefix changes"), "got: {msg}");
    assert!(msg.contains("Pinned hash: a1b2c3d4e5f6"), "got: {msg}");
    assert!(
        msg.contains("Drift:       none (hash stable)"),
        "got: {msg}"
    );
}

#[test]
fn cache_stats_warns_on_prefix_change() {
    let mut app = create_test_app();
    app.prefix_stability_pct = Some(67);
    app.prefix_checks_total = 3;
    app.prefix_change_count = 1;
    // The one change was an undeclared drift — the case worth warning about.
    app.prefix_drift_count = 1;
    app.prefix_pin_reason = Some("initial".to_string());
    app.prefix_last_miss_reason = Some("drift:sys".to_string());
    app.last_prefix_change_desc =
        Some("drift — prefix cache invalidated: system prompt changed".to_string());
    app.last_pinned_prefix_hash =
        Some("deadbeef0000deadbeef0000deadbeef0000deadbeef0000deadbeef0000deadbeef".to_string());

    let result = cache(&mut app, Some("stats"));
    let msg = result.message.expect("cache stats produces a message");

    assert!(msg.contains("Stability: 67%"), "got: {msg}");
    assert!(msg.contains("WARNING — 1 undeclared drift"), "got: {msg}");
    assert!(msg.contains("Last miss:  drift:sys"), "got: {msg}");
    assert!(msg.contains("system prompt changed"), "got: {msg}");
    assert!(msg.contains("1 change detected"), "got: {msg}");
}

#[test]
fn cache_stats_does_not_warn_on_declared_header_change() {
    let mut app = create_test_app();
    app.prefix_stability_pct = Some(67);
    app.prefix_checks_total = 3;
    app.prefix_change_count = 1;
    // A declared header change (e.g. /model): expected, not drift.
    app.prefix_drift_count = 0;
    app.prefix_pin_reason = Some("change:model".to_string());
    app.prefix_last_miss_reason = Some("change:model".to_string());
    app.last_prefix_change_desc = Some("change:model — tool set changed".to_string());
    app.last_pinned_prefix_hash =
        Some("deadbeef0000deadbeef0000deadbeef0000deadbeef0000deadbeef0000deadbeef".to_string());

    let result = cache(&mut app, Some("stats"));
    let msg = result.message.expect("cache stats produces a message");

    assert!(
        msg.contains("stable (all changes were declared header changes)"),
        "got: {msg}"
    );
    assert!(!msg.contains("WARNING — "), "got: {msg}");
    assert!(msg.contains("Pin reason: change:model"), "got: {msg}");
}

#[test]
fn cache_stats_shows_cache_hit_summary() {
    let mut app = create_test_app();
    app.prefix_stability_pct = Some(100);
    app.prefix_checks_total = 1;
    app.last_pinned_prefix_hash =
        Some("abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234".to_string());

    app.push_turn_cache_record(TurnCacheRecord {
        provider: None,
        provider_identity: None,
        model: None,
        auto_model: false,
        input_tokens: 10_000,
        output_tokens: 1_000,
        cache_hit_tokens: Some(8_000),
        cache_miss_tokens: Some(2_000),
        reasoning_replay_tokens: None,
        cache_write_tokens: None,
        reasoning_tokens: None,
        cost_audit: None,
        recorded_at: Instant::now(),
    });
    app.push_turn_cache_record(TurnCacheRecord {
        provider: None,
        provider_identity: None,
        model: None,
        auto_model: false,
        input_tokens: 5_000,
        output_tokens: 500,
        cache_hit_tokens: Some(4_500),
        cache_miss_tokens: Some(500),
        reasoning_replay_tokens: None,
        cache_write_tokens: None,
        reasoning_tokens: None,
        cost_audit: None,
        recorded_at: Instant::now(),
    });

    let result = cache(&mut app, Some("stats"));
    let msg = result.message.expect("cache stats produces a message");

    assert!(msg.contains("Turns recorded: 2"), "got: {msg}");
    // Total: 12,500 hit out of 15,000 cache-aware = 83.3%
    assert!(msg.contains("83.3%"), "got: {msg}");
}

#[test]
fn cache_stats_low_hit_rate_shows_note() {
    let mut app = create_test_app();
    app.prefix_stability_pct = Some(100);
    app.prefix_checks_total = 1;
    app.last_pinned_prefix_hash =
        Some("abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234".to_string());

    app.push_turn_cache_record(TurnCacheRecord {
        provider: None,
        provider_identity: None,
        model: None,
        auto_model: false,
        input_tokens: 10_000,
        output_tokens: 1_000,
        cache_hit_tokens: Some(1_000),
        cache_miss_tokens: Some(9_000),
        reasoning_replay_tokens: None,
        cache_write_tokens: None,
        reasoning_tokens: None,
        cost_audit: None,
        recorded_at: Instant::now(),
    });

    let result = cache(&mut app, Some("stats"));
    let msg = result.message.expect("cache stats produces a message");

    // 10% hit rate → below 80% threshold
    assert!(msg.contains("10.0%"), "got: {msg}");
    assert!(
        msg.contains("cache hit rate is low"),
        "should show low-hit-rate advisory, got: {msg}"
    );
}

#[test]
fn cache_stats_flags_reported_1747_low_hit_fixture() {
    let mut app = create_test_app();
    app.prefix_stability_pct = Some(100);
    app.prefix_checks_total = 1;
    app.last_pinned_prefix_hash =
        Some("abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234".to_string());

    // Fixture from #1747 / Amund's DeepSeek-TUI session aggregate:
    // hit=21,356,928, miss=8,470,281, output=165,624.
    app.push_turn_cache_record(TurnCacheRecord {
        provider: None,
        provider_identity: None,
        model: None,
        auto_model: false,
        input_tokens: 29_827_209,
        output_tokens: 165_624,
        cache_hit_tokens: Some(21_356_928),
        cache_miss_tokens: Some(8_470_281),
        reasoning_replay_tokens: None,
        cache_write_tokens: None,
        reasoning_tokens: None,
        cost_audit: None,
        recorded_at: Instant::now(),
    });

    let result = cache(&mut app, Some("stats"));
    let msg = result.message.expect("cache stats produces a message");

    assert!(msg.contains("71.6%"), "got: {msg}");
    assert!(msg.contains("Cache hit tokens:  21.4M"), "got: {msg}");
    assert!(msg.contains("Cache miss tokens: 8.5M"), "got: {msg}");
    assert!(
        msg.contains("cache hit rate is low"),
        "reported #1747 fixture should remain below the advisory threshold: {msg}"
    );
}

#[test]
fn format_tokens_handles_all_scales() {
    assert_eq!(format_tokens(0), "0");
    assert_eq!(format_tokens(999), "999");
    assert_eq!(format_tokens(1_000), "1.0K");
    assert_eq!(format_tokens(15_500), "15.5K");
    assert_eq!(format_tokens(1_000_000), "1.0M");
    assert_eq!(format_tokens(2_500_000), "2.5M");
}

#[test]
fn tools_command_is_truthful_before_any_request_snapshot() {
    let mut app = create_test_app();
    let result = dispatch(&mut app, "tools", None).expect("registered tools command");

    assert!(!result.is_error);
    assert!(
        result
            .message
            .expect("message")
            .contains("snapshot unavailable — no model request has been captured")
    );
}

#[test]
fn tools_command_and_compatibility_alias_render_same_exact_snapshot() {
    let mut app = create_test_app();
    app.session.last_tool_request_snapshot = Some(
        crate::tool_inspection::ToolInspectionSnapshot::from_prepared_request(
            "turn-1",
            2,
            Some(&[test_tool("read_file")]),
        ),
    );

    let primary = dispatch(&mut app, "tools", Some("json")).expect("primary command");
    let alias = dispatch(&mut app, "tool-studio", Some("json")).expect("compatibility alias");
    let primary = match primary.action.expect("primary pager") {
        AppAction::OpenTextPager { content, .. } => content,
        other => panic!("unexpected primary action: {other:?}"),
    };
    let alias = match alias.action.expect("alias pager") {
        AppAction::OpenTextPager { content, .. } => content,
        other => panic!("unexpected alias action: {other:?}"),
    };

    assert_eq!(primary, alias);
    let parsed: serde_json::Value = serde_json::from_str(&primary).expect("valid JSON output");
    assert_eq!(parsed["turn_id"]["value"], "turn-1");
    assert_eq!(parsed["step"], 2);
    assert_eq!(parsed["tool_count"], 1);
    assert_eq!(parsed["tools"][0]["name"]["value"], "read_file");
}

#[test]
fn tools_command_rejects_unknown_formats_without_mutating_state() {
    let mut app = create_test_app();
    app.session.last_tool_request_snapshot = Some(
        crate::tool_inspection::ToolInspectionSnapshot::from_prepared_request(
            "turn-1",
            1,
            Some(&[]),
        ),
    );
    let before = app.session.last_tool_request_snapshot.clone();

    let result = dispatch(&mut app, "tools", Some("yaml")).expect("tools command");

    assert!(result.is_error);
    assert_eq!(app.session.last_tool_request_snapshot, before);
}

#[cfg(test)]
mod cost_breakdown_tests {
    use super::*;
    use crate::commands::contract::DebugCostComponents as CostComponents;
    use crate::pricing::{CostEstimate, TurnCostAudit};
    use crate::tui::app::App;
    use crate::tui::app::{TuiOptions, TurnCacheRecord};
    use codewhale_config::pricing::CostCurrency;
    use codewhale_runtime::config::Config;
    use std::path::PathBuf;
    use std::time::Instant;

    fn test_app() -> App {
        let options = TuiOptions {
            skills_dir: PathBuf::from("/tmp/test-skills"),
            ..crate::tui::test_support::test_tui_options(PathBuf::from("/tmp/test-workspace"))
        };
        let mut app = App::new(options, &Config::default());
        app.ui_locale = codewhale_localization::Locale::En;
        app.cost_currency = CostCurrency::Usd;
        app.api_provider = codewhale_config::ProviderKind::Deepseek;
        app
    }

    fn priced_audit(estimate: CostEstimate) -> TurnCostAudit {
        TurnCostAudit {
            estimate: Some(estimate),
            provenance: None,
            unpriced_classes: Vec::new(),
            unpriced_reason: None,
            live_pricing_defect: None,
            usd_priced: true,
            cny_priced: estimate.cny > 0.0,
            exact_usd: None,
            exact_cny: None,
        }
    }

    fn turn_record(model: &str, audit: TurnCostAudit) -> TurnCacheRecord {
        TurnCacheRecord {
            provider: Some(codewhale_config::ProviderKind::Deepseek),
            provider_identity: None,
            model: Some(model.to_string()),
            auto_model: false,
            input_tokens: 100,
            output_tokens: 10,
            cache_hit_tokens: None,
            cache_miss_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            cost_audit: Some(audit),
            reasoning_replay_tokens: None,
            recorded_at: Instant::now(),
        }
    }

    #[test]
    fn configured_model_cost_copy_does_not_claim_published_rates() {
        let _env = crate::test_support::lock_test_env();
        let mut app = test_app();
        app.session
            .cost_pricing_provenances
            .insert("user_override".into());
        app.session.cost_priced_turns = 1;
        let report = cost(&mut app).message.expect("cost report");
        assert!(report.contains("user-declared, unverified price estimates"));
        assert!(!report.contains("published rates"));
        assert!(!report.contains("money-metered"));
    }

    /// The decomposition's terms are exactly the headline's inputs, so their
    /// sum reproduces the headline — including when the #244 monotonic floor,
    /// not the live accumulators, is the number on display (#4939).
    #[test]
    fn cost_breakdown_components_sum_to_headline() {
        let mut app = test_app();
        app.session.cost_priced_turns = 2;
        app.accrue_session_cost_estimate(CostEstimate {
            usd: 0.05,
            cny: 0.0,
        });
        app.accrue_subagent_cost_estimate(CostEstimate {
            usd: 0.02,
            cny: 0.0,
        });

        // Live sum is the headline: no floor component.
        let components = CostComponents::compute(&app);
        assert_eq!(components.parent_turns, 0.05);
        assert_eq!(components.subagents, 0.02);
        assert_eq!(components.display_floor, 0.0);
        assert_eq!(
            components.sum(),
            app.displayed_session_cost_for_currency(CostCurrency::Usd),
            "components must sum to the /cost headline"
        );

        // After a downward reconciliation the high-water is the headline; the
        // difference surfaces as an explicit floor component, and the sum still
        // reproduces the headline exactly.
        app.session.displayed_cost_high_water = 0.10;
        let components = CostComponents::compute(&app);
        assert!(components.display_floor > 0.0);
        assert_eq!(
            components.sum(),
            app.displayed_session_cost_for_currency(CostCurrency::Usd),
            "floor component must absorb exactly the high-water excess"
        );

        let msg = cost(&mut app).message.expect("cost report");
        assert!(msg.contains("Breakdown"), "{msg}");
        assert!(msg.contains("Parent turns: $0.0500"), "{msg}");
        assert!(msg.contains("Sub-agents: $0.0200"), "{msg}");
        assert!(msg.contains("Reconciliation floor:"), "{msg}");
    }

    /// Per-route attribution comes from the same `TurnCostAudit`s that fed the
    /// total, in the display currency; with every priced turn itemized, the
    /// route amounts account for the whole parent component. CNY amounts are
    /// the audits' provider-published CNY figures — never an FX projection of
    /// the USD column (#4939).
    #[test]
    fn cost_breakdown_itemizes_routes_from_turn_audits() {
        let mut app = test_app();
        app.cost_currency = CostCurrency::Cny;
        let turns = [
            CostEstimate {
                usd: 0.01,
                cny: 0.07,
            },
            CostEstimate {
                usd: 0.02,
                cny: 0.14,
            },
        ];
        for estimate in turns {
            let audit = priced_audit(estimate);
            app.record_turn_cost_audit(&audit);
            app.accrue_session_cost_estimate(estimate);
            app.push_turn_cache_record(turn_record("deepseek-chat", audit));
        }

        let components = CostComponents::compute(&app);
        assert_eq!(
            components.sum(),
            app.displayed_session_cost_for_currency(CostCurrency::Cny),
            "CNY components must sum to the CNY headline"
        );

        let msg = cost(&mut app).message.expect("cost report");
        assert!(
            msg.contains("Parent-turn spend by route (2 of 2 priced turns itemized):"),
            "{msg}"
        );
        // 0.07 + 0.14 accumulated in ring order equals the parent component's
        // accumulation, so the route line shows the whole parent spend.
        let route_amount = codewhale_commands::diagnostics_reports::format_cost_amount_precise(
            components.parent_turns,
            crate::commands::contract::to_command_currency(
                app.cost_display_currency(app.cost_currency),
            ),
        );
        assert!(
            msg.contains(&format!("deepseek/deepseek-chat: {route_amount}")),
            "{msg}"
        );
        assert!(msg.contains("¥"), "CNY display must use CNY symbol: {msg}");
    }

    /// An unpriced headline renders no breakdown: decomposing a number that is
    /// not being shown would fabricate amounts the report just declined to
    /// claim.
    #[test]
    fn cost_breakdown_absent_when_headline_unknown() {
        let mut app = test_app();
        let msg = cost(&mut app).message.expect("cost report");
        assert!(!msg.contains("Breakdown"), "{msg}");
        assert!(!msg.contains("Parent turns:"), "{msg}");
    }
}

#[cfg(test)]
mod route_tests {
    use super::*;

    #[test]
    fn cache_route_keeps_exact_named_custom_identity() {
        let record = TurnCacheRecord {
            provider: Some(codewhale_config::ProviderKind::Custom),
            provider_identity: Some("lm-studio".to_string()),
            model: Some("local-code-model".to_string()),
            auto_model: false,
            input_tokens: 1,
            output_tokens: 1,
            cache_hit_tokens: None,
            cache_miss_tokens: None,
            reasoning_replay_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            cost_audit: None,
            recorded_at: Instant::now(),
        };

        let mut app = create_test_app();
        app.push_turn_cache_record(record);
        let mut bundle = app.command_contexts();
        let mut parts = bundle
            .contexts(codewhale_command_contract::handler::CommandCapabilities::DEBUG_DIAGNOSTICS)
            .into_parts();
        let turn = parts
            .debug_diagnostics
            .as_deref_mut()
            .unwrap()
            .cache_telemetry()
            .history
            .pop()
            .unwrap();
        assert_eq!(
            codewhale_commands::debug::cache_format::format_turn_cache_route(&turn),
            "lm-studio/local-code-..."
        );
    }
}

#[cfg(test)]
mod zones_tests {
    use super::*;
    use codewhale_runtime::config::Config;
    use std::path::PathBuf;

    #[test]
    fn cache_zones_output_reports_real_wiring() {
        let mut app = App::new(
            crate::tui::test_support::test_tui_options(PathBuf::from(".")),
            &Config::default(),
        );
        app.api_messages = std::sync::Arc::new(Vec::new());
        app.last_pinned_prefix_hash = None;
        app.prefix_change_count = 0;

        let expected = "\
Cache Zones (#2264 three-zone contract)

── PinnedPrefix (system + tools, frozen baseline)
  Status:    unavailable (not yet frozen)
  Run a turn first to freeze the baseline.

── AppendLog (conversation history, append-only)
  Status:      wired — backs the engine session history
  Messages:    0
  History msgs: 0

── TurnScratch (per-turn ephemeral data)
  Status:      not wired — type scaffolding, unused by requests

── Contract Status
  PinnedPrefix: not frozen
  AppendLog:    wired (session history)
  TurnScratch:  not wired
";
        assert_eq!(
            cache(&mut app, Some("zones")).message.as_deref(),
            Some(expected)
        );
    }
}

#[cfg(test)]
mod arg_tests {
    use super::*;
    use codewhale_runtime::config::Config;
    use std::path::PathBuf;

    fn app() -> App {
        App::new(
            crate::tui::test_support::test_tui_options(PathBuf::from(".")),
            &Config::default(),
        )
    }

    #[test]
    fn cache_rejects_unknown_word_args() {
        let mut app = app();
        for arg in ["stat", "inspector", "inspect--json"] {
            let result = cache(&mut app, Some(arg));
            assert!(result.is_error, "/cache {arg} must be a usage error");
            let text = result.message.as_deref().unwrap_or_default();
            assert!(
                text.contains(arg) && text.contains("Usage: /cache"),
                "{text}"
            );
        }
    }

    #[test]
    fn cache_inspect_matches_whole_word_with_optional_flags() {
        let mut app = app();
        for arg in ["inspect", "inspect --json", "inspect  --verbose"] {
            let result = cache(&mut app, Some(arg));
            let text = result.message.as_deref().unwrap_or_default();
            assert!(!result.is_error, "/cache {arg}: {text}");
            assert!(
                !text.contains("Unknown /cache argument"),
                "/cache {arg}: {text}"
            );
        }
    }

    #[test]
    fn cache_numeric_arg_still_selects_count() {
        let mut app = app();
        let result = cache(&mut app, Some("5"));
        assert!(!result.is_error);
    }
}

mod preview_host_tests {
    use super::*;
    use codewhale_models::Role;
    use codewhale_runtime::config::Config;

    #[test]
    fn unknown_argument_is_rejected_without_touching_state() {
        let options = crate::tui::test_support::test_tui_options(std::path::PathBuf::from(
            "/tmp/test-workspace-preview-request",
        ));
        let mut app = App::new(options, &Config::default());
        let messages_before = app.api_messages.len();
        let history_before = app.history.len();

        let result = dispatch(&mut app, "preview-request", Some("nope"))
            .expect("registered preview command");

        assert!(!result.is_error);
        assert!(
            result
                .message
                .as_deref()
                .is_some_and(|message| message.contains("/preview-request")),
            "{result:?}"
        );
        assert!(result.action.is_none());
        assert_eq!(app.api_messages.len(), messages_before);
        assert_eq!(app.history.len(), history_before);
    }

    #[test]
    fn command_delegates_to_the_engine_and_mutates_nothing() {
        let options = crate::tui::test_support::test_tui_options(std::path::PathBuf::from(
            "/tmp/test-workspace-preview-request-pure",
        ));
        let mut app = App::new(options, &Config::default());
        app.api_messages_mut().push(codewhale_models::Message {
            role: Role::User,
            content: vec![codewhale_models::ContentBlock::Text {
                text: "hello".to_string(),
                cache_control: None,
            }],
        });

        let result = dispatch(&mut app, "preview-request", Some("json"))
            .expect("registered preview command");

        // The command itself renders nothing: the engine is the authority.
        assert!(result.message.is_none(), "{result:?}");
        assert!(matches!(
            result.action,
            Some(AppAction::PreviewOutboundRequest { json: true, .. })
        ));
        assert_eq!(app.api_messages.len(), 1);
        assert!(app.history.is_empty());
    }

    #[test]
    fn base_prompt_provenance_is_runtime_not_a_source_path() {
        let label = crate::prompts::base_prompt_origin().label();
        assert!(!label.contains("crates/"), "{label}");
        assert!(!label.contains(".rs"), "{label}");
        assert!(
            label.contains("bundled") || label.contains("override"),
            "{label}"
        );
    }
}

/// FEAT-029 must retain main's newer per-scope cache display when moving
/// formatting behind the facet. Parent writes (including in-flight writes)
/// and agent writes belong in their own denominators, never as invented hits.
#[test]
fn cache_report_preserves_upstream_scoped_session_rates() {
    for with_history in [false, true] {
        let mut harness = super::debug_diagnostics_test_support::DiagnosticsHarness::new();
        let app = &mut harness.app;
        app.ui_locale = codewhale_localization::Locale::En;
        if with_history {
            app.push_turn_cache_record(TurnCacheRecord {
                provider: None,
                provider_identity: None,
                model: None,
                auto_model: false,
                input_tokens: 1000,
                output_tokens: 10,
                cache_hit_tokens: Some(800),
                cache_miss_tokens: Some(200),
                reasoning_replay_tokens: None,
                cache_write_tokens: None,
                reasoning_tokens: None,
                cost_audit: None,
                recorded_at: Instant::now(),
            });
        }
        app.session.total_cache_hit_tokens = 800;
        app.session.total_cache_miss_tokens = 200;
        let parent_only = cache(app, None).message.expect("cache report");
        assert!(
            !parent_only.contains("Session cache hit rate:"),
            "{parent_only}"
        );

        app.session.subagent_cache_hit_tokens = Some(600);
        app.session.subagent_cache_miss_tokens = Some(0);
        app.session.subagent_cache_write_tokens = Some(400);
        let scopes = cache(app, None).message.expect("cache report");
        assert!(
            scopes.ends_with("\n\nSession cache hit rate: parent 80% · agents 60% · combined 70%"),
            "{scopes}"
        );

        app.session.total_cache_write_tokens = 200;
        app.session.pending_turn_cache_write_tokens = 400;
        let writes = cache(app, None).message.expect("cache report");
        assert!(
            writes.ends_with("\n\nSession cache hit rate: parent 50% · agents 60% · combined 54%"),
            "{writes}"
        );

        app.session.reset_token_breakdown();
        app.session.subagent_cache_hit_tokens = Some(0);
        app.session.subagent_cache_miss_tokens = Some(0);
        app.session.subagent_cache_write_tokens = Some(400);
        let no_parent = cache(app, None).message.expect("cache report");
        assert!(
            no_parent.ends_with("\n\nSession cache hit rate: agents 0% · combined 0%"),
            "{no_parent}"
        );
    }
}
