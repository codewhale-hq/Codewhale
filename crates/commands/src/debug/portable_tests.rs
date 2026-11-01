//! Facet-only checks for the portable diagnostics leaf closure.
//! No App, provider, file system or request builder is used by these tests.

use std::cell::RefCell;
use std::rc::Rc;

use codewhale_command_contract::facets::{
    CommandDebugDiagnosticsContext, CommandPresentationContext, DebugBalanceProjection,
    DebugCacheInspectionObservation, DebugCacheInspectionUnavailable, DebugCacheTelemetry,
    DebugCostProjection, DebugPromptContext, DebugPromptInspection, DebugPromptSourceMap,
    DebugSystemProjection, DebugTokenProjection, DebugToolSnapshot, DebugWarmupKey,
};
use codewhale_command_contract::handler::CommandContexts;

use super::{balance, cache, preview_request, tokens, tool_inspection};

#[derive(Default)]
struct FakeDiagnostics {
    events: Rc<RefCell<Vec<&'static str>>>,
    observation: Option<Result<DebugCacheInspectionObservation, DebugCacheInspectionUnavailable>>,
    remembered: Option<DebugPromptInspection>,
    balance_supported: bool,
}

impl CommandDebugDiagnosticsContext for FakeDiagnostics {
    fn balance_projection(&self) -> DebugBalanceProjection {
        self.events.borrow_mut().push("balance");
        DebugBalanceProjection {
            provider_display_name: "Example Provider".into(),
            supports_balance_api: self.balance_supported,
        }
    }
    fn system_projection(&self) -> DebugSystemProjection {
        panic!("system authority is unrelated to cache inspection")
    }
    fn token_projection(&self) -> DebugTokenProjection {
        panic!("usage authority is unrelated to cache inspection")
    }
    fn cost_projection(&self) -> DebugCostProjection {
        panic!("cost authority is unrelated to cache inspection")
    }
    fn cache_telemetry(&self) -> DebugCacheTelemetry {
        panic!("telemetry authority is unrelated to cache inspection")
    }
    fn context_source_map(&self) -> DebugPromptSourceMap {
        panic!("context authority is unrelated to cache inspection")
    }
    fn prompt_context(&self) -> DebugPromptContext {
        panic!("prompt authority is unrelated to cache inspection")
    }
    fn tool_snapshot(&self) -> Option<DebugToolSnapshot> {
        self.events.borrow_mut().push("tool_snapshot");
        None
    }
    fn inspect_cache(
        &self,
    ) -> Result<DebugCacheInspectionObservation, DebugCacheInspectionUnavailable> {
        self.events.borrow_mut().push("inspect");
        self.observation.clone().expect("configured inspection")
    }
    fn remember_cache_inspection(&mut self, inspection: DebugPromptInspection) {
        self.events.borrow_mut().push("remember");
        assert!(
            self.remembered.replace(inspection).is_none(),
            "remember exactly once"
        );
    }
}

struct FakePresentation;
impl CommandPresentationContext for FakePresentation {
    fn translate(&self, _key: &str, _replacements: &[(&str, &str)]) -> Result<String, String> {
        panic!("inspection and warmup do not need translation")
    }
}

fn inspection(hash: &str) -> DebugPromptInspection {
    DebugPromptInspection {
        base_static_prefix_hash: hash.into(),
        full_request_prefix_hash: format!("full-{hash}"),
        tool_catalog_hash: String::new(),
        layers: Vec::new(),
    }
}

fn observation() -> DebugCacheInspectionObservation {
    DebugCacheInspectionObservation {
        current: inspection("current"),
        previous: Some(inspection("previous")),
        current_warmup_key: DebugWarmupKey {
            provider: "test".into(),
            model: "test-model".into(),
            base_url: "https://invalid.example/v1".into(),
            static_prefix_hash: "current".into(),
            tool_catalog_hash: String::new(),
            project_pack_hash: String::new(),
            skills_hash: String::new(),
        },
        last_warmup_key: None,
        current_warmup_hash_short: "current-key".into(),
        last_warmup_hash_short: None,
    }
}

fn cache_with(fake: &mut FakeDiagnostics, arg: Option<&str>) -> super::CommandResult {
    let mut presentation = FakePresentation;
    cache::cache(
        CommandContexts::empty()
            .with_debug_diagnostics(fake)
            .with_presentation(&mut presentation),
        arg,
    )
}

#[test]
fn missing_diagnostics_facet_fails_before_any_action() {
    for result in [
        balance::balance(CommandContexts::empty()),
        tokens::system_prompt(CommandContexts::empty()),
        tokens::context(CommandContexts::empty(), None),
        tool_inspection::tools(CommandContexts::empty(), Some("invalid")),
        tokens::tokens(CommandContexts::empty()),
        tokens::cost(CommandContexts::empty(), None),
        cache::cache(CommandContexts::empty(), Some("warmup")),
    ] {
        assert!(result.is_error, "missing authority must be an error");
        assert_eq!(
            result.message.as_deref(),
            Some("Error: Command capability unavailable: debug_diagnostics")
        );
        assert!(result.action.is_none());
    }
    let result = preview_request::preview_request(Some("--json --prompt  keep  trailing "));
    assert!(
        !result.is_error,
        "pure preview never constructs a host bundle"
    );
    assert!(result.action.is_some());
}

#[test]
fn missing_presentation_is_not_an_implicit_host_fallback() {
    let mut fake = FakeDiagnostics::default();
    for result in [
        tokens::tokens(CommandContexts::empty().with_debug_diagnostics(&mut fake)),
        tokens::cost(
            CommandContexts::empty().with_debug_diagnostics(&mut fake),
            None,
        ),
        cache::cache(
            CommandContexts::empty().with_debug_diagnostics(&mut fake),
            Some("warmup"),
        ),
    ] {
        assert!(result.is_error);
        assert_eq!(
            result.message.as_deref(),
            Some("Error: Command capability unavailable: presentation")
        );
        assert!(result.action.is_none());
    }
    assert!(fake.events.borrow().is_empty());
}

#[test]
fn cache_inspection_rejections_and_argument_only_warmup_never_store() {
    let mut fake = FakeDiagnostics::default();
    let conflict = cache_with(&mut fake, Some("inspect --json --verbose"));
    assert_eq!(
        conflict.message.as_deref(),
        Some("cache inspect: --json and --verbose cannot be combined")
    );
    assert!(fake.events.borrow().is_empty());
    let warmup = cache_with(&mut fake, Some("warmup"));
    assert!(warmup.action.is_some());
    assert!(fake.events.borrow().is_empty());

    for (failure, expected) in [
        (
            DebugCacheInspectionUnavailable::NoConcreteRoute,
            "cache inspect: Auto has no concrete route yet; send a turn first",
        ),
        (
            DebugCacheInspectionUnavailable::MissingCapturedEndpoint,
            "cache inspect: the restored Auto route has no captured endpoint; send a turn first",
        ),
    ] {
        fake.observation = Some(Err(failure));
        let result = cache_with(&mut fake, Some("inspect --json"));
        assert_eq!(result.message.as_deref(), Some(expected));
        assert_eq!(fake.events.borrow().last().unwrap(), &"inspect");
        assert!(
            fake.remembered.is_none(),
            "failure must preserve previous state"
        );
    }
}

#[test]
fn cache_observes_then_renders_then_stores_the_same_snapshot_once() {
    let mut fake = FakeDiagnostics {
        observation: Some(Ok(observation())),
        ..FakeDiagnostics::default()
    };
    let result = cache_with(&mut fake, Some("inspect --json"));
    let rendered: serde_json::Value =
        serde_json::from_str(result.message.as_deref().unwrap()).unwrap();
    assert_eq!(rendered["base_static_prefix_hash"], "current");
    assert_eq!(rendered["current_warmup_key"]["model"], "test-model");
    assert_eq!(
        rendered["warmup_status"],
        "Warmup status: no previous warmup (current key: current-key)"
    );
    assert_eq!(&*fake.events.borrow(), &["inspect", "remember"]);
    assert_eq!(fake.remembered.as_ref(), Some(&inspection("current")));
}

#[test]
fn balance_and_tool_snapshot_branches_observe_only_their_declared_facts() {
    let mut fake = FakeDiagnostics::default();
    let unsupported = balance::balance(CommandContexts::empty().with_debug_diagnostics(&mut fake));
    assert_eq!(
        unsupported.message.as_deref(),
        Some(
            "Balance check is not supported for Example Provider yet. Check the provider dashboard for account balance details."
        )
    );
    assert!(unsupported.action.is_none());
    fake.balance_supported = true;
    let supported = balance::balance(CommandContexts::empty().with_debug_diagnostics(&mut fake));
    assert!(supported.message.is_none());
    assert!(matches!(
        supported.action,
        Some(super::DebugAction::FetchBalance)
    ));
    let unavailable = tool_inspection::tools(
        CommandContexts::empty().with_debug_diagnostics(&mut fake),
        Some("yaml"),
    );
    assert_eq!(
        unavailable.message.as_deref(),
        Some(
            "Tool request snapshot unavailable — no model request has been captured for the latest turn."
        )
    );
    assert!(
        !unavailable.is_error,
        "availability precedes format validation"
    );
    assert_eq!(
        &*fake.events.borrow(),
        &["balance", "balance", "tool_snapshot"]
    );
}

#[test]
fn json_serialization_fallback_still_commits_the_observed_inspection_once() {
    // Real semantic inspection data cannot make serde_json fail. Inject only
    // the serializer result, not a production callback or host state, then
    // exercise the same fallback/commit helpers as the live command.
    let output = cache::json_or_fallback(Err(serde_json::Error::io(std::io::Error::other(
        "forced serializer failure",
    ))));
    assert_eq!(
        output,
        "{\"error\":\"cache inspection serialization failed\"}"
    );
    let mut fake = FakeDiagnostics::default();
    let delivered = cache::commit_rendered_inspection(&mut fake, observation(), output);
    assert_eq!(
        delivered,
        "{\"error\":\"cache inspection serialization failed\"}"
    );
    assert_eq!(&*fake.events.borrow(), &["remember"]);
    assert_eq!(fake.remembered.as_ref(), Some(&inspection("current")));
}
