use super::*;
use codewhale_config::catalog::{
    CatalogOffering, CatalogSource, CatalogStatus, ProviderCatalogDelta, base_url_fingerprint,
    now_unix,
};
use codewhale_engine::cost_status::EffectiveRouteEnvelope;
use codewhale_engine::model_inventory::ModelInventory;
use codewhale_runtime::config::ProviderIdentity;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const SINGULAR: &str = "model list / model resolve";
const PLURAL: &str = "models";
const RUNTIME: &str = "runtime api";
const ROUTING: &str = "routing inventory";
const COST: &str = "cost";
const UNKNOWN: &str = "unknown";

const ROSTER: &[&str] = &["claude-opus-5", "claude-haiku-4-5", "claude-parity-probe-1"];

type Readings = BTreeMap<(String, String, &'static str), BTreeMap<&'static str, String>>;

struct Route {
    label: &'static str,
    api_path: &'static str,
    identity: ProviderIdentity,
    base_url: String,
    models: &'static [&'static str],
}

fn cold_process() {
    codewhale_engine::provider_catalog_live::reset_cache_for_test();
    codewhale_engine::provider_lake::clear_live_snapshot();
}

fn known<T: ToString>(value: Option<T>) -> String {
    value.map_or_else(|| UNKNOWN.to_string(), |value| value.to_string())
}

fn rate(value: &Value) -> String {
    let number = match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.parse::<f64>().ok(),
        _ => None,
    };
    known(number)
}

fn roster(mut models: Vec<String>) -> String {
    models.sort();
    models.dedup();
    models.join(",")
}

fn read(
    readings: &mut Readings,
    route: &Route,
    model: &str,
    field: &'static str,
    surface: &'static str,
    value: String,
) {
    readings
        .entry((route.label.to_string(), model.to_string(), field))
        .or_default()
        .insert(surface, value);
}

fn persist_id_only_roster(identity: &ProviderIdentity, base_url: &str) {
    let fingerprint = base_url_fingerprint(base_url);
    let fetched_at = now_unix();
    let ticket = codewhale_engine::provider_catalog_live::begin_refresh_for_identity(
        identity.provider,
        identity.key.as_str(),
        base_url,
    );
    assert_eq!(
        codewhale_engine::provider_catalog_live::record_success_if_current(
            &ticket,
            ProviderCatalogDelta {
                provider: identity.key.to_string(),
                base_url_fingerprint: fingerprint.clone(),
                fetched_at,
                offerings: ROSTER
                    .iter()
                    .map(|id| CatalogOffering {
                        provider: identity.key.to_string(),
                        wire_model_id: (*id).to_string(),
                        endpoint_key: "chat".into(),
                        source: CatalogSource::Live {
                            base_url_fingerprint: fingerprint.clone(),
                            fetched_at,
                        },
                        ..Default::default()
                    })
                    .collect(),
            },
        ),
        Some(CatalogStatus::Fresh)
    );
}

fn read_singular(readings: &mut Readings, config_path: &Path, route: &Route) -> Result<()> {
    cold_process();
    let store = codewhale_config::ConfigStore::load(Some(config_path.to_path_buf()))?;
    let listed =
        codewhale_engine::route_preferences::list_models(&store, Some(route.identity.provider))?
            .into_iter()
            .find(|(listed, _)| listed == route.identity.key.as_str())
            .map(|(_, models)| roster(models));
    read(
        readings,
        route,
        "*",
        "roster",
        SINGULAR,
        listed.unwrap_or_else(|| "route not listed".into()),
    );
    for model in route.models {
        let limits = codewhale_engine::route_preferences::resolve_model(
            config_path,
            route.identity.key.as_str(),
            &route.base_url,
            Some(model),
        )
        .map(|candidate| candidate.limits());
        let (context, output) = match limits {
            Ok(limits) => (known(limits.context_tokens), known(limits.output_tokens)),
            Err(error) => (format!("refused: {error}"), format!("refused: {error}")),
        };
        read(readings, route, model, "limit.context", SINGULAR, context);
        read(readings, route, model, "limit.output", SINGULAR, output);
    }
    Ok(())
}

fn read_plural(readings: &mut Readings, config: &Config, route: &Route) -> Result<()> {
    cold_process();
    codewhale_engine::models_dev_live::maybe_load_persisted_cache();
    let mut route_config = config.clone();
    route_config
        .scope_to_provider_identity(&route.identity)
        .map_err(anyhow::Error::msg)?;
    read(
        readings,
        route,
        "*",
        "roster",
        PLURAL,
        roster(codewhale_engine::provider_lake::listed_models_for_route(
            &route_config,
            &route.identity,
        )),
    );
    Ok(())
}

async fn read_runtime(
    readings: &mut Readings,
    client: &reqwest::Client,
    addr: &SocketAddr,
    route: &Route,
) -> Result<()> {
    let body: Value = client
        .get(format!("http://{addr}/v1/providers/{}", route.api_path))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let models = body["models"].as_array().context("models array")?;
    read(
        readings,
        route,
        "*",
        "roster",
        RUNTIME,
        roster(
            models
                .iter()
                .filter_map(|model| model["id"].as_str().map(str::to_string))
                .collect(),
        ),
    );
    for model in route.models {
        let facts = models
            .iter()
            .find(|entry| entry["id"] == *model)
            .map_or(&Value::Null, |entry| &entry["facts"]);
        read(
            readings,
            route,
            model,
            "limit.context",
            RUNTIME,
            known(facts["effective_limits"]["context_tokens"].as_u64()),
        );
        read(
            readings,
            route,
            model,
            "limit.output",
            RUNTIME,
            known(facts["effective_limits"]["output_tokens"].as_u64()),
        );
        read(
            readings,
            route,
            model,
            "window",
            RUNTIME,
            known(facts["effective_context"]["tokens"].as_u64()),
        );
        read(
            readings,
            route,
            model,
            "window.source",
            RUNTIME,
            known(facts["effective_context"]["source"].as_str()),
        );
        read(
            readings,
            route,
            model,
            "price.input",
            RUNTIME,
            rate(&facts["pricing"]["input_per_million"]),
        );
        read(
            readings,
            route,
            model,
            "price.output",
            RUNTIME,
            rate(&facts["pricing"]["output_per_million"]),
        );
        read(
            readings,
            route,
            model,
            "price.basis",
            RUNTIME,
            format!(
                "provenance={} status={} source={}",
                facts["pricing"]["provenance"], facts["pricing_status"], facts["source"]
            ),
        );
    }
    Ok(())
}

fn read_routing(readings: &mut Readings, config: &Config, route: &Route) -> Result<()> {
    let inventory = ModelInventory::from_config(config).map_err(anyhow::Error::msg)?;
    let candidates: Vec<_> = inventory
        .candidates
        .iter()
        .filter(|candidate| candidate.identity == route.identity)
        .collect();
    read(
        readings,
        route,
        "*",
        "roster",
        ROUTING,
        roster(
            candidates
                .iter()
                .map(|candidate| candidate.model.clone())
                .collect(),
        ),
    );
    for model in route.models {
        let candidate = candidates
            .iter()
            .find(|candidate| candidate.model == *model);
        read(
            readings,
            route,
            model,
            "window",
            ROUTING,
            known(candidate.map(|candidate| candidate.context_window)),
        );
        read(
            readings,
            route,
            model,
            "window.source",
            ROUTING,
            known(candidate.map(|candidate| {
                if candidate.context_window_unverified {
                    "unverified"
                } else {
                    "verified"
                }
            })),
        );
        read(
            readings,
            route,
            model,
            "limit.output",
            ROUTING,
            known(candidate.and_then(|candidate| candidate.max_output)),
        );
    }
    Ok(())
}

fn read_cost(readings: &mut Readings, config: &Config, route: &Route) {
    let usage = Usage {
        input_tokens: 100_000,
        output_tokens: 10_000,
        ..Default::default()
    };
    for model in route.models {
        let envelope = EffectiveRouteEnvelope::from_admitted(
            Some(config),
            &route.identity,
            *model,
            Some(&route.base_url),
            chrono::Utc::now(),
        );
        let quote = envelope.provider_live_pricing.as_ref();
        let audit = envelope.audit(&usage);
        read(
            readings,
            route,
            model,
            "price.input",
            COST,
            quote
                .and_then(|quote| quote.input_per_million.as_deref())
                .map_or_else(|| UNKNOWN.to_string(), |text| rate(&json!(text))),
        );
        read(
            readings,
            route,
            model,
            "price.output",
            COST,
            quote
                .and_then(|quote| quote.output_per_million.as_deref())
                .map_or_else(|| UNKNOWN.to_string(), |text| rate(&json!(text))),
        );
        read(
            readings,
            route,
            model,
            "price.basis",
            COST,
            format!(
                "provenance={:?} billing={:?} charge_usd={} unpriced={:?}",
                quote.map(|quote| quote.provenance.clone()),
                envelope.billing_mode,
                known(audit.exact_usd.as_deref()),
                audit.unpriced_reason,
            ),
        );
    }
}

#[tokio::test]
async fn one_captured_catalog_reads_the_same_on_every_model_surface() -> Result<()> {
    let _env = lock_test_env();
    let _live = codewhale_engine::provider_lake::lock_live_snapshot();
    let root = tempfile::tempdir()?;
    let captured = codewhale_config::catalog::BUNDLED_MODELS_DEV_JSON;
    let catalog_path = root.path().join("captured-models-dev.json");
    fs::write(&catalog_path, captured)?;
    let _catalog = EnvVarGuard::set("CODEWHALE_MODELS_DEV_PATH", &catalog_path);
    let _home = EnvVarGuard::set("CODEWHALE_HOME", root.path());
    let _config_path = EnvVarGuard::remove("CODEWHALE_CONFIG_PATH");
    let _legacy_config_path = EnvVarGuard::remove("DEEPSEEK_CONFIG_PATH");
    let _cli = EnvVarGuard::remove(codewhale_config::CLI_API_KEY_ENV);
    cold_process();

    let config_path = root.path().join("config.toml");
    fs::write(
        &config_path,
        r#"provider = "anthropic"

[providers.anthropic]
api_key = "parity-fixture-key"
model = "claude-opus-5"

[providers.zai]
api_key = "parity-fixture-key"
model = "GLM-5.3"

[providers.lab]
kind = "openai-compatible"
base_url = "http://127.0.0.1:9/v1"
api_key = "parity-fixture-key"
model = "claude-haiku-4-5"
"#,
    )?;
    let config = Config::load(Some(config_path.clone()), None)?;
    let route = |label: &'static str,
                 api_path: &'static str,
                 key: &str,
                 models: &'static [&'static str]|
     -> Result<Route> {
        let identity = config
            .resolve_provider_pin_identity(key)
            .map_err(anyhow::Error::msg)?;
        Ok(Route {
            label,
            api_path,
            base_url: config.base_url_for_route(&identity),
            identity,
            models,
        })
    };
    let routes = [
        route(
            "anthropic (official, id-only roster)",
            "anthropic/models",
            "anthropic",
            ROSTER,
        )?,
        route(
            "lab (custom endpoint, same spelling)",
            "custom/models?model_provider_id=lab",
            "lab",
            &["claude-haiku-4-5"],
        )?,
        route(
            "zai (plan endpoint)",
            "zai/models",
            "zai",
            &["GLM-5.3", "GLM-5.2"],
        )?,
    ];

    assert!(
        codewhale_engine::models_dev_live::refresh(false)
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?
            > 0
    );
    persist_id_only_roster(&routes[0].identity, &routes[0].base_url);

    let mut readings = Readings::new();
    for route in &routes {
        read_singular(&mut readings, &config_path, route)?;
        read_plural(&mut readings, &config, route)?;
    }
    cold_process();
    let (addr, _, handle) = spawn_test_server_with_config_path(config_path.clone())
        .await?
        .expect("loopback server required");
    let client = codewhale_release::tls::reqwest_client();
    for route in &routes {
        read_runtime(&mut readings, &client, &addr, route).await?;
        read_routing(&mut readings, &config, route)?;
        read_cost(&mut readings, &config, route);
    }
    handle.abort();
    cold_process();

    println!(
        "PARITY captured_catalog sha256={} bytes={}",
        Sha256::digest(captured.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        captured.len()
    );
    let mut differing = Vec::new();
    let mut known_differences: Vec<String> = Vec::new();
    for ((route, model, field), surfaces) in &readings {
        let mut values: Vec<String> = surfaces
            .values()
            .map(|value| {
                if *field == "roster" {
                    roster(
                        value
                            .to_ascii_lowercase()
                            .split(',')
                            .map(str::to_string)
                            .collect(),
                    )
                } else {
                    value.clone()
                }
            })
            .collect();
        values.sort();
        values.dedup();
        let compared = !matches!(*field, "price.basis" | "window.source");
        let borrowed_window = *field == "window"
            && surfaces.get(RUNTIME).map(String::as_str) == Some(UNKNOWN)
            && readings
                .get(&(route.clone(), model.clone(), "window.source"))
                .and_then(|sources| sources.get(ROUTING))
                .map(String::as_str)
                == Some("unverified");
        let verdict = if !compared {
            "info"
        } else if borrowed_window {
            known_differences.push(format!("{route} | {model} | {surfaces:?}"));
            "KNOWN-DIFFERENCE"
        } else if values.len() == 1 {
            "agree"
        } else {
            differing.push(format!("{route} | {model} | {field} | {surfaces:?}"));
            "DIFFER"
        };
        println!("PARITY {verdict} | {route} | {model} | {field} | {surfaces:?}");
    }
    let value = |route: &Route, model: &str, field: &'static str, surface: &'static str| {
        readings
            .get(&(route.label.to_string(), model.to_string(), field))
            .and_then(|surfaces| surfaces.get(surface))
            .cloned()
            .unwrap_or_default()
    };
    assert_eq!(
        value(&routes[0], "*", "roster", SINGULAR),
        "claude-haiku-4-5,claude-opus-5,claude-parity-probe-1",
        "the singular command lists the route's own roster, not every bundled row"
    );
    assert_eq!(
        value(&routes[0], "claude-opus-5", "limit.context", RUNTIME),
        "1000000"
    );
    assert_eq!(
        value(&routes[0], "claude-haiku-4-5", "limit.context", RUNTIME),
        "200000"
    );
    assert_eq!(value(&routes[0], "claude-opus-5", "price.input", COST), "5");
    assert_eq!(
        value(&routes[0], "claude-haiku-4-5", "price.output", COST),
        "5"
    );
    for field in ["limit.context", "limit.output", "price.input"] {
        assert_eq!(
            value(&routes[1], "claude-haiku-4-5", field, RUNTIME),
            UNKNOWN,
            "a custom endpoint borrows no fact from a same-named model: {field}"
        );
    }
    assert_eq!(
        value(&routes[1], "claude-haiku-4-5", "window.source", ROUTING),
        "unverified",
        "routing never presents a same-named model's window as this endpoint's"
    );
    assert_eq!(
        value(&routes[1], "claude-haiku-4-5", "limit.output", ROUTING),
        UNKNOWN
    );
    for model in ["GLM-5.3", "GLM-5.2"] {
        let basis = value(&routes[2], model, "price.basis", COST);
        assert!(
            basis.contains("NotMoneyMetered") && basis.contains("charge_usd=unknown"),
            "a plan route is never billed per token: {model}: {basis}"
        );
    }
    for (model, rate) in [("GLM-5.3", UNKNOWN), ("GLM-5.2", "1.4")] {
        for surface in [COST, RUNTIME] {
            assert_eq!(
                value(&routes[2], model, "price.input", surface),
                rate,
                "the list rate is shown the same everywhere and never turned into a charge: {model} {surface}"
            );
        }
    }
    let basis = value(&routes[1], "claude-haiku-4-5", "price.basis", COST);
    assert!(
        basis.contains("UnknownBillingBasis") && basis.contains("charge_usd=unknown"),
        "a custom endpoint is never given a charge: {basis}"
    );
    for surface in [COST, RUNTIME] {
        assert_eq!(
            value(&routes[0], "claude-parity-probe-1", "price.input", surface),
            UNKNOWN,
            "a metered first-party model with no price row reads unknown: {surface}"
        );
    }
    let basis = value(&routes[0], "claude-parity-probe-1", "price.basis", COST);
    assert!(
        basis.contains("charge_usd=unknown") && !basis.contains("unpriced=None"),
        "an unpriced model produces no charge and says why: {basis}"
    );
    assert_eq!(
        known_differences,
        vec![
            "lab (custom endpoint, same spelling) | claude-haiku-4-5 | {\"routing inventory\": \"200000\", \"runtime api\": \"unknown\"}"
        ],
        "the only named difference is the custom endpoint's borrowed routing window"
    );
    assert!(
        differing.is_empty(),
        "surfaces disagree on the same captured catalog:\n{}",
        differing.join("\n")
    );
    Ok(())
}
