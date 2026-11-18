use super::test_support::{EnvGuard, no_managed_config};
use super::*;
use crate::test_support::{EnvVarGuard, lock_test_env};
use codewhale_config::catalog::bundled_catalog_offerings;
use codewhale_config::catalog::reviewed::bundled_reviewed;
use codewhale_config::descriptors::{bundled_provider_descriptors, compatibility_for_kind};
use codewhale_config::route::offering::bundled_offerings;
use codewhale_config::route::{
    LogicalModelRef, ProviderDescriptor as RouteProviderDescriptor, RouteError, RouteRequest,
    RouteResolver,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

const GOLDEN_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/config/model_resolution.golden.tsv"
);

const REGENERATE_COMMAND: &str = "WRITE_GOLDEN=1 CODEWHALE_SCCACHE=1 python3 scripts/build-lock.py \
/tmp/codewhale-build.lock -- sh scripts/with-hermetic-test-home.sh sh scripts/dev-cargo.sh test \
--locked -p codewhale-runtime --lib config::model_resolution_golden";

const COLUMNS: [&str; 14] = [
    "provider",
    "input",
    "input_kind",
    "runtime_canonical",
    "runtime_normalized",
    "runtime_wire",
    "runtime_requested",
    "tui_default_model",
    "cli_model",
    "route_canonical",
    "route_wire",
    "route_endpoint",
    "route_context",
    "disagree",
];

const IDENTITY_COLUMNS: [(&str, usize); 7] = [
    ("runtime_normalized", 4),
    ("runtime_wire", 5),
    ("runtime_requested", 6),
    ("tui_default_model", 7),
    ("cli_model", 8),
    ("route_canonical", 9),
    ("route_wire", 10),
];

const PROVIDER_ENV_SUFFIXES: [&str; 8] = [
    "_MODEL",
    "_BASE_URL",
    "_API_BASE",
    "_PROVIDER",
    "_MODE",
    "_HTTP_HEADERS",
    "_DEFAULT_TEXT_MODEL",
    "_PROFILE",
];

struct Target {
    label: String,
    kind: ProviderKind,
    catalog_provider_id: String,
    config_identity: String,
    config_table: String,
    custom_base_url: Option<String>,
    default_model: String,
}

impl Target {
    fn builtin(kind: ProviderKind) -> Self {
        Self {
            label: kind.as_str().to_string(),
            kind,
            catalog_provider_id: kind.as_str().to_string(),
            config_identity: kind.as_str().to_string(),
            config_table: compatibility_for_kind(kind).config_key.to_string(),
            custom_base_url: None,
            default_model: RouteProviderDescriptor::for_kind(kind)
                .default_wire_model()
                .as_str()
                .to_string(),
        }
    }

    fn data_only(descriptor: &codewhale_config::descriptors::ProviderDescriptor) -> Self {
        Self {
            label: format!("descriptor:{}", descriptor.id),
            kind: ProviderKind::Custom,
            catalog_provider_id: descriptor.id.clone(),
            config_identity: descriptor.id.clone(),
            config_table: descriptor.id.clone(),
            custom_base_url: Some(descriptor.base_url.clone()),
            default_model: descriptor.default_model.clone(),
        }
    }

    fn config_body(&self, model: &str) -> String {
        let model = toml::Value::String(model.to_string()).to_string();
        let identity = toml::Value::String(self.config_identity.clone()).to_string();
        let table = &self.config_table;
        match &self.custom_base_url {
            None => format!(
                "provider = {identity}\n\n[providers.{table}]\napi_key = \"k\"\nmodel = {model}\n"
            ),
            Some(base_url) => {
                let base_url = toml::Value::String(base_url.clone()).to_string();
                format!(
                    "provider = {identity}\n\n[providers.{table}]\nkind = \"openai-compatible\"\n\
                     base_url = {base_url}\napi_key = \"k\"\nmodel = {model}\n"
                )
            }
        }
    }
}

fn targets() -> Vec<Target> {
    ProviderKind::ALL
        .iter()
        .copied()
        .map(Target::builtin)
        .chain(bundled_provider_descriptors().iter().map(Target::data_only))
        .collect()
}

fn provider_model_rules(provider_id: &str) -> Option<serde_json::Value> {
    let descriptors: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../config/assets/provider_descriptors.json"
    )))
    .expect("provider_descriptors.json parses");
    descriptors["providers"]
        .as_array()
        .expect("provider rows")
        .iter()
        .find(|row| row["id"] == provider_id)
        .map(|row| row["models"].clone())
}

fn offering_ids_by_provider() -> BTreeMap<String, BTreeSet<String>> {
    let mut ids: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let reviewed = bundled_offerings();
    let asset = bundled_catalog_offerings()
        .iter()
        .map(|row| row.to_offering())
        .collect::<Vec<_>>();
    for offering in reviewed.iter().chain(asset.iter()) {
        ids.entry(offering.provider.as_str().to_string())
            .or_default()
            .insert(offering.wire_model_id.as_str().to_string());
    }
    ids
}

fn committed_inputs() -> BTreeMap<String, BTreeSet<String>> {
    let mut inputs: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let text = fs::read_to_string(GOLDEN_PATH).unwrap_or_default();
    for line in text.lines().skip(1) {
        let mut cells = line.split('\t');
        let (Some(provider), Some(input)) = (cells.next(), cells.next()) else {
            continue;
        };
        let input = if input.starts_with('"') {
            serde_json::from_str::<String>(input).expect("quoted golden input")
        } else {
            input.to_string()
        };
        inputs
            .entry(provider.to_string())
            .or_default()
            .insert(input);
    }
    inputs
}

fn inputs_for(
    target: &Target,
    offerings: &BTreeMap<String, BTreeSet<String>>,
    committed: &BTreeMap<String, BTreeSet<String>>,
) -> BTreeMap<String, BTreeSet<&'static str>> {
    let mut inputs: BTreeMap<String, BTreeSet<&'static str>> = BTreeMap::new();
    let mut add = |input: &str, kind: &'static str| {
        inputs.entry(input.to_string()).or_default().insert(kind);
    };
    let reviewed = bundled_reviewed();
    let is_builtin = target.custom_base_url.is_none();

    add(&target.default_model, "default");
    let provider_offerings = offerings.get(&target.catalog_provider_id);
    for id in provider_offerings.into_iter().flatten() {
        add(id, "offering");
    }

    if is_builtin && let Some(rules) = provider_model_rules(&target.catalog_provider_id) {
        for table in ["aliases", "wire_slugs"] {
            for (key, value) in rules[table].as_object().expect("models table") {
                add(key, "alias-key");
                add(value.as_str().expect("models target"), "alias-target");
            }
        }
    }
    if is_builtin {
        for selection in reviewed
            .selections
            .iter()
            .filter(|selection| selection.provider == target.catalog_provider_id)
        {
            add(&selection.id, "alias-target");
            for alias in &selection.aliases {
                add(alias, "alias-key");
            }
        }
    }

    let variant_bases = std::iter::once(target.default_model.clone()).chain(
        provider_offerings
            .into_iter()
            .flatten()
            .find(|id| **id != target.default_model)
            .cloned(),
    );
    for base in variant_bases.collect::<Vec<_>>() {
        add(&base.to_uppercase(), "variant-uppercase");
        add(&format!("models/{base}"), "variant-models-prefix");
        add(
            &format!("{}/{base}", target.catalog_provider_id),
            "variant-provider-prefix",
        );
        add(&format!("  {base}  "), "variant-whitespace");
    }

    add("auto", "sentinel-auto");
    add("totally-unknown-model-x", "unknown");
    if !matches!(
        target.kind,
        ProviderKind::Deepseek | ProviderKind::DeepseekAnthropic
    ) {
        add("deepseek-v4-pro", "foreign-deepseek");
    }
    for input in committed.get(&target.label).into_iter().flatten() {
        inputs
            .entry(input.clone())
            .or_insert_with(|| BTreeSet::from(["carried"]));
    }
    inputs
}

fn optional_cell(value: Option<String>) -> String {
    match value {
        Some(value) if !value.is_empty() => value,
        _ => "-".to_string(),
    }
}

fn route_error_tag(error: &RouteError) -> &'static str {
    match error {
        RouteError::EmptyModel => "-!empty-model",
        RouteError::InvalidProvider(_) => "-!invalid-provider",
        RouteError::AmbiguousModel(_) => "-!ambiguous-model",
        RouteError::ForeignModelForDirectProvider { .. } => "-!foreign-model",
        RouteError::ResponsesProfile { .. } => "-!responses-profile",
        RouteError::UnsupportedModelProtocol { .. } => "-!unsupported-protocol",
    }
}

fn route_cells(target: &Target, input: &str) -> [String; 4] {
    let request = RouteRequest {
        explicit_provider: Some(target.kind),
        model_selector: Some(LogicalModelRef::from(input)),
        base_url_override: target.custom_base_url.clone(),
        ..RouteRequest::default()
    };
    match RouteResolver::bundled().resolve(&request) {
        Ok(candidate) => [
            optional_cell(
                candidate
                    .canonical_model()
                    .map(|id| id.as_str().to_string()),
            ),
            optional_cell(Some(candidate.wire_model_id().as_str().to_string())),
            optional_cell(Some(candidate.endpoint().endpoint_key.clone())),
            optional_cell(
                candidate
                    .limits()
                    .context_tokens
                    .map(|tokens| tokens.to_string()),
            ),
        ],
        Err(error) => std::array::from_fn(|_| route_error_tag(&error).to_string()),
    }
}

fn configured_chain_cells(target: &Target, input: &str, config_path: &Path) -> [String; 2] {
    fs::write(config_path, target.config_body(input)).expect("write golden fixture config");
    let tui = match Config::load(Some(config_path.to_path_buf()), None) {
        Ok(config) => optional_cell(Some(config.default_model())),
        Err(_) => "-!load".to_string(),
    };
    let cli = match codewhale_config::ConfigStore::load(Some(config_path.to_path_buf())) {
        Ok(store) => {
            let resolved = store
                .config
                .resolve_runtime_options(&codewhale_config::CliRuntimeOverrides::default());
            if resolved.model_error.is_some() {
                "-!load".to_string()
            } else {
                optional_cell(Some(resolved.model))
            }
        }
        Err(_) => "-!load".to_string(),
    };
    [tui, cli]
}

fn tsv_cell(value: &str) -> String {
    if value != value.trim() || value.contains(['\t', '\n', '\r']) {
        format!("{value:?}")
    } else {
        value.to_string()
    }
}

fn render_row(cells: &[String]) -> String {
    cells
        .iter()
        .map(|cell| tsv_cell(cell))
        .collect::<Vec<_>>()
        .join("\t")
}

fn golden_table(config_path: &Path) -> String {
    let offerings = offering_ids_by_provider();
    let committed = committed_inputs();
    let mut targets = targets();
    targets.sort_by(|a, b| a.label.cmp(&b.label));

    let mut out = COLUMNS.join("\t");
    out.push('\n');
    for target in &targets {
        for (input, kinds) in inputs_for(target, &offerings, &committed) {
            let mut cells: Vec<String> = Vec::with_capacity(COLUMNS.len());
            cells.push(target.label.clone());
            cells.push(input.clone());
            cells.push(kinds.into_iter().collect::<Vec<_>>().join("+"));
            cells.push(optional_cell(canonical_model_id_for_provider(
                target.kind,
                &input,
            )));
            cells.push(optional_cell(normalize_model_name_for_provider(
                target.kind,
                &input,
            )));
            cells.push(optional_cell(Some(wire_model_for_provider(
                target.kind,
                &input,
            ))));
            cells.push(optional_cell(requested_model_for_provider(
                target.kind,
                &input,
            )));
            cells.extend(configured_chain_cells(target, &input, config_path));
            cells.extend(route_cells(target, &input));
            let canonical = &cells[3];
            let disagree = IDENTITY_COLUMNS
                .iter()
                .filter(|(_, index)| cells[*index] != *canonical)
                .map(|(name, _)| *name)
                .collect::<Vec<_>>()
                .join(",");
            cells.push(if disagree.is_empty() {
                "-".to_string()
            } else {
                disagree
            });
            out.push_str(&render_row(&cells));
            out.push('\n');
        }
    }
    out
}

struct StrippedProviderEnv {
    _guards: Vec<EnvVarGuard>,
}

impl StrippedProviderEnv {
    fn new() -> Self {
        let mut names: BTreeSet<String> = ProviderKind::ALL
            .iter()
            .flat_map(|kind| RouteProviderDescriptor::for_kind(*kind).env_vars().iter())
            .map(|name| (*name).to_string())
            .collect();
        names.extend(
            bundled_provider_descriptors()
                .iter()
                .map(|descriptor| descriptor.api_key_env.clone()),
        );
        names.extend(
            std::env::vars_os()
                .filter_map(|(name, _)| name.into_string().ok())
                .filter(|name| {
                    PROVIDER_ENV_SUFFIXES
                        .iter()
                        .any(|suffix| name.ends_with(suffix))
                }),
        );
        let guards = names
            .into_iter()
            .map(|name| EnvVarGuard::remove(Box::leak(name.into_boxed_str())))
            .collect();
        Self { _guards: guards }
    }
}

#[test]
fn model_resolution_matches_the_committed_golden() {
    let _lock = lock_test_env();
    let _stripped = StrippedProviderEnv::new();
    let root = tempfile::tempdir().expect("golden tempdir");
    let _guard = EnvGuard::new(root.path());
    let _managed = no_managed_config(root.path());
    let config_path = root.path().join(".deepseek").join("config.toml");
    fs::create_dir_all(config_path.parent().expect("config dir")).expect("create config dir");

    let actual = golden_table(&config_path);
    if std::env::var("WRITE_GOLDEN").ok().as_deref() == Some("1") {
        fs::write(GOLDEN_PATH, &actual).expect("write model resolution golden");
        return;
    }
    let expected = fs::read_to_string(GOLDEN_PATH)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    if actual != expected {
        let divergence = crate::test_support::first_divergence(&expected, &actual);
        panic!(
            "model resolution changed from {GOLDEN_PATH} (first divergence: {divergence:?}); \
             if the change is intended, regenerate with: {REGENERATE_COMMAND}"
        );
    }
}
