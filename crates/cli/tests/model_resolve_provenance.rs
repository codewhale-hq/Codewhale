//! `model resolve` must report the route the runtime would actually take.
//!
//! Regression coverage for #4832, where a Z.ai config reported
//! `provider: deepseek` because the subcommand read only the CLI flags and
//! never consulted the resolved runtime. A diagnostic that confidently
//! reports the wrong provider is worse than one that reports nothing, so
//! every provider is asserted here rather than DeepSeek alone.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use tempfile::TempDir;

/// Run `model resolve` against a sealed HOME containing `config`.
///
/// `env_clear` plus a temporary HOME keeps this off the real
/// `~/.codewhale/config.toml`; the suite has written to real user state before
/// (#4831) and this test must never be the one that does it again.
fn resolve_with_config(config: &str, args: &[&str]) -> BTreeMap<String, String> {
    let fixture = TempDir::new().expect("fixture root");
    let home = fixture.path().join("sealed-home");
    fs::create_dir_all(home.join(".codewhale")).expect("sealed config dir");
    fs::write(home.join(".codewhale").join("config.toml"), config).expect("seed config");

    let mut command = Command::new(codewhale_binary());
    command.arg("model").arg("resolve").args(args);
    let output = command
        .env_clear()
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("CODEWHALE_HOME", home.join(".codewhale"))
        .env("CODEWHALE_SECRET_BACKEND", "file")
        .output()
        .expect("run model resolve");

    assert!(
        output.status.success(),
        "model resolve {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_once(": "))
        .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
        .collect()
}

#[test]
fn resolve_reports_the_configured_provider_not_a_deepseek_fallback() {
    let report = resolve_with_config(
        "provider = \"zai\"\n\n[providers.zai]\napi_key = \"k\"\n",
        &[],
    );

    assert_eq!(
        report.get("provider").map(String::as_str),
        Some("zai"),
        "configured provider must survive to the diagnostic: {report:?}"
    );
    assert_eq!(
        report.get("provider_source").map(String::as_str),
        Some("config"),
        "provenance must name the config file: {report:?}"
    );
}

#[test]
fn resolve_reports_a_provider_scoped_model_as_explicitly_configured() {
    let report = resolve_with_config(
        "provider = \"moonshot\"\n\n[providers.moonshot]\napi_key = \"k\"\nmodel = \"kimi-k3-turbo\"\n",
        &[],
    );

    assert_eq!(report.get("provider").map(String::as_str), Some("moonshot"));
    assert_eq!(
        report.get("requested").map(String::as_str),
        Some("kimi-k3-turbo"),
        "a configured model is a request, not a fallback: {report:?}"
    );
    assert_eq!(
        report.get("used_fallback").map(String::as_str),
        Some("false"),
        "{report:?}"
    );
    assert_eq!(
        report.get("model_source").map(String::as_str),
        Some("config [providers.*].model"),
        "{report:?}"
    );
    let base = codewhale_config::ProviderKind::Zai
        .provider()
        .default_base_url();
    let configured = format!(
        "provider = 'zai'\n[providers.zai]\napi_key = 'fixture-only'\nmodel = 'GLM-5.2'\n[[custom_models]]\nprovider = 'zai'\nbase_url = '{base}'\nid = 'operator-only-fixture'\nlimit = {{ context = 96000, output = 8000 }}\n"
    );
    let report = resolve_with_config(&configured, &["operator-only-fixture"]);
    assert_eq!(
        report.get("resolved").map(String::as_str),
        Some("operator-only-fixture")
    );
    assert_eq!(report.get("provider").map(String::as_str), Some("zai"));
    assert_eq!(
        report.get("used_fallback").map(String::as_str),
        Some("false")
    );
}

#[test]
fn resolve_admits_when_nothing_was_configured() {
    // The honest answer to "what did the user ask for" is "nothing". The
    // built-in default may still be shown, but it must be labelled as ours.
    let report = resolve_with_config("", &[]);

    assert_eq!(
        report.get("requested").map(String::as_str),
        Some(""),
        "an unconfigured model must not be presented as a request: {report:?}"
    );
    assert_eq!(
        report.get("used_fallback").map(String::as_str),
        Some("true"),
        "{report:?}"
    );
    assert_eq!(
        report.get("model_source").map(String::as_str),
        Some("provider default"),
        "{report:?}"
    );
}

#[test]
fn a_foreign_model_argument_cannot_switch_the_configured_provider() {
    let fixture = TempDir::new().expect("fixture root");
    let home = fixture.path().join("sealed-home");
    fs::create_dir_all(home.join(".codewhale")).expect("sealed config dir");
    fs::write(
        home.join(".codewhale").join("config.toml"),
        "provider = \"zai\"\n\n[providers.zai]\napi_key = \"k\"\n",
    )
    .expect("seed config");

    let output = Command::new(codewhale_binary())
        .arg("model")
        .arg("resolve")
        .arg("deepseek-v4-flash")
        .env_clear()
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("CODEWHALE_HOME", home.join(".codewhale"))
        .env("CODEWHALE_SECRET_BACKEND", "file")
        .output()
        .expect("run failing model resolve");

    assert!(!output.status.success());
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .to_ascii_lowercase();
    assert!(
        combined.contains("not served by direct provider zai"),
        "{combined}"
    );
    assert!(!combined.contains("provider: deepseek"), "{combined}");
}

#[test]
fn an_explicit_matching_provider_can_resolve_its_model() {
    let report = resolve_with_config(
        "provider = \"zai\"\n\n[providers.zai]\napi_key = \"k\"\n",
        &["deepseek-v4-flash", "--provider", "deepseek"],
    );

    assert_eq!(report.get("provider").map(String::as_str), Some("deepseek"));
    assert_eq!(
        report.get("requested").map(String::as_str),
        Some("deepseek-v4-flash")
    );
    assert_eq!(
        report.get("used_fallback").map(String::as_str),
        Some("false")
    );
    assert_eq!(
        report.get("provider_source").map(String::as_str),
        Some("--provider")
    );
}

#[test]
fn unknown_providerless_model_stays_on_configured_provider() {
    let report = resolve_with_config(
        "provider = \"zai\"\n\n[providers.zai]\napi_key = \"k\"\n",
        &["totally-unknown-model"],
    );

    assert_eq!(report.get("provider").map(String::as_str), Some("zai"));
    assert_eq!(
        report.get("resolved").map(String::as_str),
        Some("totally-unknown-model")
    );
    assert_eq!(
        report.get("used_fallback").map(String::as_str),
        Some("false")
    );
}

#[test]
fn an_explicit_provider_flag_is_reported_as_the_source() {
    let report = resolve_with_config(
        "provider = \"zai\"\n\n[providers.zai]\napi_key = \"k\"\n",
        &["--provider", "moonshot"],
    );

    assert_eq!(report.get("provider").map(String::as_str), Some("moonshot"));
    assert_eq!(
        report.get("provider_source").map(String::as_str),
        Some("--provider"),
        "{report:?}"
    );
}

#[test]
fn explicit_openai_without_a_model_reports_its_own_documented_default() {
    let report = resolve_with_config(
        "provider = \"zai\"\n\n[providers.zai]\napi_key = \"k\"\nmodel = \"GLM-5.2\"\n",
        &["--provider", "openai"],
    );

    assert_eq!(report.get("provider").map(String::as_str), Some("openai"));
    assert_eq!(
        report.get("resolved").map(String::as_str),
        Some("gpt-5.6"),
        "an OpenAI query must use OpenAI's documented default, not the first catalog row or the configured Z.ai model: {report:?}"
    );
    assert_eq!(
        report.get("used_fallback").map(String::as_str),
        Some("true"),
        "{report:?}"
    );
    assert_eq!(
        report.get("provider_source").map(String::as_str),
        Some("--provider"),
        "{report:?}"
    );
    assert_eq!(
        report.get("model_source").map(String::as_str),
        Some("provider default"),
        "the overridden Z.ai model provenance must not leak into the OpenAI hypothetical: {report:?}"
    );
}

/// Run `model resolve` with global flags placed before the subcommand, which
/// is where `--provider` / `--model` actually go.
fn resolve_with_global_flags(
    config: &str,
    global: &[&str],
    args: &[&str],
) -> BTreeMap<String, String> {
    let fixture = TempDir::new().expect("fixture root");
    let home = fixture.path().join("sealed-home");
    fs::create_dir_all(home.join(".codewhale")).expect("sealed config dir");
    fs::write(home.join(".codewhale").join("config.toml"), config).expect("seed config");

    let mut command = Command::new(codewhale_binary());
    command.args(global).arg("model").arg("resolve").args(args);
    let output = command
        .env_clear()
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("CODEWHALE_HOME", home.join(".codewhale"))
        .env("CODEWHALE_SECRET_BACKEND", "file")
        .output()
        .expect("run model resolve");

    assert!(
        output.status.success(),
        "model resolve {global:?} {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_once(": "))
        .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
        .collect()
}

/// v0.9.1 kimi-k3 dogfood report: `codewhale --provider moonshot --model kimi-k3 model resolve`
/// reported `kimi-k2.7-code`. The top-level flags are the route this process
/// is on, not a hypothetical, so the diagnostic has to answer with the runtime
/// resolution instead of re-deriving a registry default and ignoring `--model`.
#[test]
fn top_level_provider_and_model_flags_report_the_runtime_route() {
    let report = resolve_with_global_flags(
        "provider = \"zai\"\n\n[providers.zai]\napi_key = \"k\"\n",
        &["--provider", "moonshot", "--model", "kimi-k3"],
        &[],
    );

    assert_eq!(report.get("provider").map(String::as_str), Some("moonshot"));
    assert_eq!(
        report.get("resolved").map(String::as_str),
        Some("kimi-k3"),
        "the diagnostic must not contradict the model the run will use: {report:?}"
    );
    assert_eq!(
        report.get("requested").map(String::as_str),
        Some("kimi-k3"),
        "{report:?}"
    );
    assert_eq!(
        report.get("used_fallback").map(String::as_str),
        Some("false"),
        "{report:?}"
    );
    assert_eq!(
        report.get("model_source").map(String::as_str),
        Some("--model"),
        "{report:?}"
    );
}

/// Moonshot ships `kimi-k3` on the direct platform API and `k3` on the Kimi
/// Code coding-plan API. Both must resolve, and neither may be answered by
/// another provider's identically named model (OpenCode Go also serves a
/// `kimi-k3`).
#[test]
fn moonshot_k3_products_resolve_without_crossing_providers() {
    for (model, base_url) in [
        ("kimi-k3", "https://api.moonshot.ai/v1"),
        ("k3", "https://api.kimi.com/coding/v1"),
    ] {
        let config = format!(
            "provider = \"moonshot\"\n\n[providers.moonshot]\napi_key = \"k\"\nbase_url = \"{base_url}\"\n"
        );
        let report = resolve_with_global_flags(&config, &[], &[model, "--provider", "moonshot"]);

        assert_eq!(
            report.get("provider").map(String::as_str),
            Some("moonshot"),
            "a Moonshot question must not be answered by another provider: {report:?}"
        );
        assert_eq!(
            report.get("resolved").map(String::as_str),
            Some(model),
            "{report:?}"
        );
        assert_eq!(
            report.get("used_fallback").map(String::as_str),
            Some("false"),
            "{report:?}"
        );
    }
}

#[test]
fn unlisted_model_on_selected_provider_passes_through_without_fallback() {
    let report = resolve_with_config(
        "provider = \"moonshot\"\n\n[providers.moonshot]\napi_key = \"k\"\n",
        &["glm-5.2", "--provider", "moonshot"],
    );

    assert_eq!(report.get("provider").map(String::as_str), Some("moonshot"));
    assert_eq!(report.get("resolved").map(String::as_str), Some("glm-5.2"));
    assert_eq!(
        report.get("used_fallback").map(String::as_str),
        Some("false")
    );
}

#[test]
fn a_new_glm_sibling_resolves_on_zai_and_passes_through_on_moonshot() {
    let served = resolve_with_global_flags(
        "provider = \"zai\"\n\n[providers.zai]\napi_key = \"k\"\n",
        &[],
        &["glm-5.3", "--provider", "zai"],
    );

    assert_eq!(served.get("provider").map(String::as_str), Some("zai"));
    assert_eq!(
        served.get("resolved").map(String::as_str),
        Some("GLM-5.3"),
        "a catalogued model must resolve to itself, not to the provider default: {served:?}"
    );
    assert_eq!(
        served.get("used_fallback").map(String::as_str),
        Some("false"),
        "a model the provider serves must not be reported as a fallback: {served:?}"
    );

    let passthrough = resolve_with_global_flags(
        "provider = \"moonshot\"\n\n[providers.moonshot]\napi_key = \"k\"\n",
        &[],
        &["glm-5.3", "--provider", "moonshot"],
    );

    assert_eq!(
        passthrough.get("provider").map(String::as_str),
        Some("moonshot")
    );
    assert_eq!(
        passthrough.get("resolved").map(String::as_str),
        Some("glm-5.3")
    );
    assert_eq!(
        passthrough.get("used_fallback").map(String::as_str),
        Some("false")
    );
}

/// The OpenRouter sibling carries a different wire id (`z-ai/glm-5.3`) than the
/// direct Z.ai row (`GLM-5.3`), so the bare family alias has to be rewritten
/// per provider rather than passed through. This pins the OpenRouter half of
/// that rewrite, which the Z.ai case above cannot observe, and pins that adding
/// the sibling left the OpenRouter default alone.
#[test]
fn the_openrouter_glm_sibling_resolves_to_its_own_gateway_wire_id() {
    let served = resolve_with_global_flags(
        "provider = \"openrouter\"\n\n[providers.openrouter]\napi_key = \"k\"\n",
        &[],
        &["glm-5.3", "--provider", "openrouter"],
    );

    assert_eq!(
        served.get("provider").map(String::as_str),
        Some("openrouter")
    );
    assert_eq!(
        served.get("resolved").map(String::as_str),
        Some("z-ai/glm-5.3"),
        "the bare alias must be rewritten to the OpenRouter wire id, not passed through: {served:?}"
    );
    assert_eq!(
        served.get("used_fallback").map(String::as_str),
        Some("false"),
        "a gateway row the provider serves must not be reported as a fallback: {served:?}"
    );

    let default_route = resolve_with_config(
        "provider = \"openrouter\"\n\n[providers.openrouter]\napi_key = \"k\"\n",
        &[],
    );
    let resolved = default_route
        .get("resolved")
        .map(String::as_str)
        .unwrap_or_default();
    assert!(
        !resolved.to_ascii_lowercase().contains("glm"),
        "adding a GLM sibling must not make GLM the OpenRouter default: {default_route:?}"
    );
}

/// A Z.ai config that names no model lands on the deliberate default,
/// `GLM-5.3`, with `provider default` provenance. This is the surface where a
/// default move would otherwise change silently under a user.
#[test]
fn zai_default_route_resolves_to_glm_5_3_with_provider_default_provenance() {
    let report = resolve_with_config(
        "provider = \"zai\"\n\n[providers.zai]\napi_key = \"k\"\n",
        &[],
    );

    assert_eq!(
        report.get("resolved").map(String::as_str),
        Some("GLM-5.3"),
        "the Z.ai default is GLM-5.3: {report:?}"
    );
    assert_eq!(
        report.get("model_source").map(String::as_str),
        Some("provider default"),
        "{report:?}"
    );
}

/// An explicit `GLM-5.2` selection keeps its own id after the default moved
/// to `GLM-5.3`: only the default changed, never a user's saved route.
#[test]
fn explicit_glm_5_2_selection_survives_the_default_move() {
    let report = resolve_with_config(
        "provider = \"zai\"\n\n[providers.zai]\napi_key = \"k\"\nmodel = \"GLM-5.2\"\n",
        &[],
    );

    assert_eq!(
        report.get("resolved").map(String::as_str),
        Some("GLM-5.2"),
        "an explicit GLM-5.2 route must not be upgraded: {report:?}"
    );
    assert_ne!(
        report.get("model_source").map(String::as_str),
        Some("provider default"),
        "{report:?}"
    );
}

fn codewhale_binary() -> PathBuf {
    if let Some(path) = std::env::var_os("CARGO_BIN_EXE_codewhale").filter(|v| !v.is_empty()) {
        return PathBuf::from(path);
    }
    if let Some(path) = option_env!("CARGO_BIN_EXE_codewhale") {
        return PathBuf::from(path);
    }

    let mut path = std::env::current_exe().expect("current test executable path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.push(format!("codewhale{}", std::env::consts::EXE_SUFFIX));
    path
}
