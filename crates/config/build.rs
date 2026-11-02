//! Compile immutable provider metadata from its one committed data owner.

use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::PathBuf,
};

use serde_json::Value;

fn text<'a>(row: &'a Value, key: &str) -> &'a str {
    row[key]
        .as_str()
        .unwrap_or_else(|| panic!("missing string {key}"))
}

fn quoted(value: &str) -> String {
    format!("{value:?}")
}

fn optional(row: &Value, key: &str) -> String {
    match row.get(key) {
        Some(Value::String(value)) => format!("Some({})", quoted(value)),
        Some(Value::Null) => "None".into(),
        _ => panic!("missing or malformed optional string {key}"),
    }
}

fn strings(row: &Value, key: &str) -> String {
    let values = row[key]
        .as_array()
        .unwrap_or_else(|| panic!("missing array {key}"));
    format!(
        "&[{}]",
        values
            .iter()
            .map(|value| quoted(value.as_str().expect("string array")))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn credential(row: &Value) -> String {
    let acquisition = match text(row, "acquisition") {
        "api_key" => "ApiKey",
        "api_key_or_oauth" => "ApiKeyOrOAuth",
        "local_optional" => "LocalOptional",
        "oauth" => "OAuth",
        "configuration" => "Configuration",
        other => panic!("unknown credential classification {other}"),
    };
    format!(
        "crate::provider::CredentialHelp {{ acquisition: crate::provider::CredentialAcquisition::{acquisition}, credential_url: {}, docs_url: {}, guidance: {} }}",
        optional(row, "credential_url"),
        optional(row, "docs_url"),
        quoted(text(row, "guidance"))
    )
}

fn wire(row: &Value) -> &'static str {
    match text(row, "wire_policy") {
        "chat_completions" => {
            "crate::provider::WirePolicy::Fixed(crate::provider::WireFormat::ChatCompletions)"
        }
        "responses" => "crate::provider::WirePolicy::Fixed(crate::provider::WireFormat::Responses)",
        "anthropic_messages" => {
            "crate::provider::WirePolicy::Fixed(crate::provider::WireFormat::AnthropicMessages)"
        }
        "model_aware" => "crate::provider::WirePolicy::ModelAware",
        other => panic!("unknown wire classification {other}"),
    }
}

fn main() {
    println!("cargo:rerun-if-changed=assets/provider_descriptors.json");
    let file: Value = serde_json::from_str(
        &fs::read_to_string("assets/provider_descriptors.json").expect("provider data"),
    )
    .expect("provider data JSON");
    assert_eq!(
        file["schema_version"].as_u64(),
        Some(3),
        "unsupported provider schema"
    );
    let rows = file["providers"].as_array().expect("built-in rows");
    let mut ids = BTreeSet::new();
    let mut kinds = BTreeSet::new();
    let mut output = String::from("// Generated from provider_descriptors.json. Do not edit.\n");
    let mut selected = Vec::new();
    output.push_str("pub(crate) static BUILTIN_DESCRIPTORS: &[BuiltinProviderDescriptor] = &[\n");
    for row in rows {
        let id = text(row, "id");
        let kind = text(row, "kind");
        assert!(!id.is_empty() && !kind.is_empty(), "empty identity");
        assert!(ids.insert(id), "duplicate provider id {id}");
        assert!(kinds.insert(kind), "duplicate provider kind {kind}");
        assert!(
            kind.chars().all(|c| c.is_ascii_alphanumeric()),
            "invalid Rust kind"
        );
        let selectable = row["selectable"].as_bool().expect("selectable status");
        let retired = row["retired"].as_bool().expect("retired status");
        assert!(
            !retired || !selectable,
            "retired provider cannot be selectable"
        );
        if kind == "Antigravity" {
            assert!(retired && !selectable, "Antigravity remains retired");
        }
        if selectable {
            selected.push((
                row["selection_order"].as_u64().expect("selection order"),
                format!("crate::ProviderKind::{kind}"),
            ));
        } else {
            assert!(row["selection_order"].is_null(), "non-selectable order");
        }
        output.push_str(&format!("BuiltinProviderDescriptor {{ kind: crate::ProviderKind::{kind}, id: {}, label: {}, base_url: {}, default_model: {}, env_vars: {}, aliases: {}, config_key: {}, secret_store_slot: {}, family: {}, selectable: {selectable}, retired: {retired}, wire_policy: {}, credential_help: {} }},\n",
            quoted(id),quoted(text(row,"label")),quoted(text(row,"base_url")),quoted(text(row,"default_model")),strings(row,"env_vars"),strings(row,"aliases"),quoted(text(row,"config_key")),quoted(text(row,"secret_store_slot")),quoted(text(row,"family")),wire(row),credential(&row["credential_help"])));
    }
    output.push_str("];\n");
    // Generated exhaustive projection makes a new/missing enum variant a
    // compiler error, rather than a delayed registry panic.
    output.push_str("pub(crate) const fn builtin_provider_descriptor(kind: crate::ProviderKind) -> &'static BuiltinProviderDescriptor { match kind {\n");
    for (index, row) in rows.iter().enumerate() {
        output.push_str(&format!(
            "crate::ProviderKind::{} => &BUILTIN_DESCRIPTORS[{index}],\n",
            text(row, "kind")
        ));
    }
    output.push_str("} }\n");
    selected.sort_by_key(|(order, _)| *order);
    for (index, (order, _)) in selected.iter().enumerate() {
        assert_eq!(
            *order,
            u64::try_from(index).expect("selection index"),
            "selection order must be unique and contiguous"
        );
    }
    let selected: Vec<_> = selected.into_iter().map(|(_, kind)| kind).collect();
    output.push_str(&format!(
        "pub(crate) const SELECTABLE_PROVIDER_KINDS: [crate::ProviderKind; {}] = [{}];\n",
        selected.len(),
        selected.join(", ")
    ));
    for row in file["descriptors"].as_array().expect("compatible rows") {
        assert!(
            ids.insert(text(row, "id")),
            "descriptor shadows built-in identity"
        );
        for field in ["id", "label", "base_url", "api_key_env", "default_model"] {
            assert!(!text(row, field).is_empty(), "empty compatible-host field");
        }
        assert!(
            matches!(
                text(row, "wire"),
                "openai-compatible" | "anthropic-messages"
            ),
            "unknown compatible wire"
        );
        assert!(
            matches!(
                text(row, "discovery"),
                "models_endpoint" | "public_models_endpoint" | "none"
            ),
            "unknown discovery"
        );
        if row.get("aliases").is_some() {
            let _ = strings(row, "aliases");
        }
        for field in ["docs_url", "credential_url", "guidance"] {
            if row.get(field).is_some() {
                let _ = optional(row, field);
            }
        }
    }
    let legacy = &file["legacy_tui"];
    output.push_str(&format!("/// Exact retained TUI-only compatibility metadata.\npub const LEGACY_DEEPSEEK_CN: LegacyProviderDescriptor = LegacyProviderDescriptor {{ id: {}, label: {}, base_url: {}, default_model: {}, config_key: {}, secret_store_slot: {} }};\n",
        quoted(text(legacy,"id")),quoted(text(legacy,"label")),quoted(text(legacy,"base_url")),quoted(text(legacy,"default_model")),quoted(text(legacy,"config_key")),quoted(text(legacy,"secret_store_slot"))));
    // Existing descriptor data owns the released presentation tags and typed
    // table projection. A missing field or duplicate tag refuses the build.
    let mut compatibility: Vec<_> = rows.iter().chain(std::iter::once(legacy)).collect();
    compatibility.sort_by_key(|row| row["tui_order"].as_u64().expect("compatibility order"));
    let mut tags = BTreeSet::new();
    let mut compatibility_ids = BTreeSet::new();
    output.push_str("static PROVIDER_COMPATIBILITY: &[ProviderCompatibility] = &[\n");
    for (index, row) in compatibility.iter().enumerate() {
        assert_eq!(
            row["tui_order"].as_u64(),
            Some(index as u64),
            "compatibility order must be contiguous"
        );
        let id = text(row, "id");
        let tag = text(row, "tui_wire_tag");
        assert!(
            !tag.is_empty() && tags.insert(tag),
            "duplicate/empty compatibility tag"
        );
        assert!(
            compatibility_ids.insert(id),
            "duplicate compatibility identity"
        );
        for field in ["catalog_id", "catalog_source_id"] {
            assert!(
                rows.iter()
                    .any(|primary| text(primary, "id") == text(row, field))
                    || text(row, field) == id,
                "unknown catalog projection"
            );
        }
        let field = text(row, "table_field");
        assert!(
            field.chars().next().is_some_and(|c| c.is_ascii_lowercase())
                && field
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
            "invalid Rust table field"
        );
        if let Some(key) = row.get("base_url_config_key") {
            let key = key.as_str().expect("endpoint table key");
            assert!(
                rows.iter().any(|row| text(row, "config_key") == key),
                "unknown endpoint config table"
            );
        }
        output.push_str(&format!("ProviderCompatibility {{ kind: crate::ProviderKind::{}, id: {}, tui_wire_tag: {}, config_key: {}, base_url_config_key: {}, catalog_id: {}, catalog_source_id: {}, subagent_aliases: {}, selector_aliases: {}, label: {}, base_url: {}, default_model: {} }},\n", text(row,"kind"),quoted(id),quoted(tag),quoted(text(row,"config_key")),quoted(row.get("base_url_config_key").map_or_else(|| text(row,"config_key"), |value| value.as_str().expect("endpoint table key"))),quoted(text(row,"catalog_id")),quoted(text(row,"catalog_source_id")),strings(row,"subagent_aliases"),strings(row,"selector_aliases"),quoted(text(row,"label")),quoted(text(row,"base_url")),quoted(text(row,"default_model"))));
    }
    output.push_str("];\n");
    // Expand against the consumer's released typed configuration struct. The
    // field inventory remains generated; custom/legacy provenance is admitted
    // by that consumer before this pure projection is called.
    output.push_str("#[macro_export]\nmacro_rules! provider_config_table {\n");
    for (mode, borrow) in [("read", "&"), ("write", "&mut ")] {
        output.push_str(&format!(
            "(@{mode} $providers:expr, $id:expr) => {{ match $id {{\n"
        ));
        for row in &compatibility {
            if text(row, "kind") != "Custom" {
                output.push_str(&format!(
                    "{} => Some({borrow}$providers.{}),\n",
                    quoted(text(row, "id")),
                    text(row, "table_field")
                ));
            }
        }
        output.push_str("_ => None } };\n");
    }
    output.push_str("}\n");
    let mut public_orders = BTreeSet::new();
    for row in rows {
        if row["retired"] == false && text(row, "kind") != "Custom" {
            let order = row["web"]["order"]
                .as_u64()
                .expect("public presentation order");
            assert!(
                public_orders.insert(order),
                "duplicate public presentation order"
            );
            assert!(
                row["web"].get("variant").is_none(),
                "retired enum presentation identity"
            );
        }
    }
    assert!(
        public_orders
            .iter()
            .enumerate()
            .all(|(index, order)| *order == index as u64),
        "public order must be contiguous"
    );
    for (key, row) in file["route_credential_help"]
        .as_object()
        .expect("route credential help")
        .iter()
        .collect::<BTreeMap<_, _>>()
    {
        let name = key.replace('-', "_").to_ascii_uppercase();
        assert!(
            name.chars().all(|c| c.is_ascii_uppercase() || c == '_'),
            "invalid route help key"
        );
        output.push_str(&format!(
            "pub(crate) const {name}_CREDENTIAL_HELP: crate::provider::CredentialHelp = {};\n",
            credential(row)
        ));
    }
    let mut defaults = String::from("// Generated data projections. Do not edit.\n");
    let refs = file["constant_refs"]
        .as_object()
        .expect("constant references");
    let constants = file["compatibility_constants"]
        .as_object()
        .expect("compatibility constants");
    for (name, value) in refs.iter().collect::<BTreeMap<_, _>>() {
        assert!(!constants.contains_key(name), "duplicate constant");
        assert!(
            name.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'),
            "invalid constant name"
        );
        let row = rows
            .iter()
            .find(|row| row["id"] == value["provider"])
            .expect("constant row");
        let field = text(value, "field");
        assert!(
            matches!(field, "base_url" | "default_model" | "credential_url"),
            "unknown constant field"
        );
        let source = if field == "credential_url" {
            &row["credential_help"][field]
        } else {
            &row[field]
        };
        defaults.push_str(&format!(
            "/// Projection of the committed provider descriptor.\npub const {name}: &str = {};\n",
            quoted(source.as_str().expect("constant field string"))
        ));
    }
    for (name, value) in constants.iter().collect::<BTreeMap<_, _>>() {
        assert!(
            name.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'),
            "invalid constant name"
        );
        defaults.push_str(&format!(
            "/// Retained data-owned compatibility seed.\npub const {name}: &str = {};\n",
            quoted(value.as_str().expect("compatibility string"))
        ));
    }
    let directory = PathBuf::from(env::var_os("OUT_DIR").expect("build output"));
    fs::write(directory.join("provider_descriptors.rs"), output).expect("write descriptors");
    fs::write(directory.join("provider_defaults.rs"), defaults).expect("write defaults");

    println!("cargo:rerun-if-changed=assets/catalog_corrections.json");
    let catalog: Value = serde_json::from_str(
        &fs::read_to_string("assets/catalog_corrections.json").expect("catalog data"),
    )
    .expect("catalog JSON");
    let reviewed = &catalog["reviewed"];
    assert!(!text(reviewed, "revision").is_empty(), "reviewed revision");
    let mut catalog_constants =
        String::from("// Generated from catalog_corrections.json.reviewed. Do not edit.\n");
    for (name, value) in reviewed["constants"]
        .as_object()
        .expect("model constants")
        .iter()
        .collect::<BTreeMap<_, _>>()
    {
        assert!(
            name.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'),
            "invalid catalog constant"
        );
        catalog_constants.push_str(&format!(
            "pub const {name}: &str = {};\n",
            quoted(value.as_str().expect("model constant string"))
        ));
    }
    for (name, reference) in reviewed["numeric_refs"]
        .as_object()
        .expect("numeric contract references")
    {
        assert!(
            name.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'),
            "invalid numeric contract name"
        );
        let field = text(reference, "field");
        assert!(
            matches!(
                field,
                "context_window" | "max_output" | "generation_default"
            ),
            "unknown numeric contract field"
        );
        let value = reviewed["intrinsic"][text(reference, "model")][field]
            .as_u64()
            .expect("numeric intrinsic fact");
        assert!(
            value > 0 && value <= u64::from(u32::MAX),
            "numeric intrinsic bound"
        );
        catalog_constants.push_str(&format!("pub const {name}: u32 = {value};\n"));
    }
    for (name, value) in reviewed["groups"]
        .as_object()
        .expect("model rosters")
        .iter()
        .collect::<BTreeMap<_, _>>()
    {
        assert!(
            name.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'),
            "invalid catalog group"
        );
        let values = value
            .as_array()
            .expect("model roster array")
            .iter()
            .map(|v| quoted(v.as_str().expect("model roster string")))
            .collect::<Vec<_>>()
            .join(", ");
        catalog_constants.push_str(&format!("pub const {name}: &[&str] = &[{values}];\n"));
    }
    for (name, provider, endpoint) in [
        (
            "OPENCODE_ZEN_RESPONSES_MODELS",
            "opencode-zen",
            Some("responses"),
        ),
        (
            "OPENCODE_ZEN_MESSAGES_MODELS",
            "opencode-zen",
            Some("messages"),
        ),
        ("OPENCODE_ZEN_CHAT_MODELS", "opencode-zen", Some("chat")),
        ("OPENCODE_GO_MODELS", "opencode-go", None),
        ("MODELSTUDIO_TEXT_MODELS", "modelstudio-token-plan", None),
        ("CODEWHALE_FALLBACK_MODELS", "codewhale", None),
    ] {
        let values = reviewed["transports"]
            .as_array()
            .expect("transport rows")
            .iter()
            .filter(|row| {
                row["provider"] == provider
                    && endpoint.is_none_or(|endpoint| row["endpoint_key"] == endpoint)
            })
            .map(|row| quoted(text(row, "id")))
            .collect::<Vec<_>>()
            .join(", ");
        catalog_constants.push_str(&format!("pub const {name}: &[&str] = &[{values}];\n"));
    }
    // Alias/completion entries refer to existing defaults or model groups;
    // the source owner never repeats a descriptor-owned default value.
    let mut values = BTreeMap::new();
    for (name, value) in constants {
        values.insert(name.as_str(), value.as_str().expect("compatibility value"));
    }
    for (name, reference) in refs {
        let provider = rows
            .iter()
            .find(|row| row["id"] == reference["provider"])
            .expect("constant provider");
        let field = text(reference, "field");
        let value = if field == "credential_url" {
            &provider["credential_help"][field]
        } else {
            &provider[field]
        };
        values.insert(name.as_str(), value.as_str().expect("constant value"));
    }
    for (name, value) in reviewed["constants"]
        .as_object()
        .expect("catalog constants")
    {
        assert!(
            values
                .insert(name, value.as_str().expect("catalog value"))
                .is_none(),
            "duplicate data constant"
        );
    }
    let resolve = |value: &str| -> String {
        match value.strip_prefix('$') {
            Some(name) => values
                .get(name)
                .unwrap_or_else(|| panic!("missing constant {name}"))
                .to_string(),
            None => value.to_string(),
        }
    };
    let mut groups: BTreeMap<String, Vec<String>> = reviewed["groups"]
        .as_object()
        .expect("groups")
        .iter()
        .map(|(name, value)| {
            (
                name.clone(),
                value
                    .as_array()
                    .expect("group")
                    .iter()
                    .map(|v| resolve(v.as_str().expect("group entry")))
                    .collect(),
            )
        })
        .collect();
    for (name, provider, endpoint) in [
        (
            "OPENCODE_ZEN_RESPONSES_MODELS",
            "opencode-zen",
            Some("responses"),
        ),
        (
            "OPENCODE_ZEN_MESSAGES_MODELS",
            "opencode-zen",
            Some("messages"),
        ),
        ("OPENCODE_ZEN_CHAT_MODELS", "opencode-zen", Some("chat")),
        ("OPENCODE_GO_MODELS", "opencode-go", None),
        ("MODELSTUDIO_TEXT_MODELS", "modelstudio-token-plan", None),
        ("CODEWHALE_FALLBACK_MODELS", "codewhale", None),
    ] {
        groups.insert(
            name.into(),
            reviewed["transports"]
                .as_array()
                .expect("transports")
                .iter()
                .filter(|row| {
                    row["provider"] == provider
                        && endpoint.is_none_or(|endpoint| row["endpoint_key"] == endpoint)
                })
                .map(|row| text(row, "id").to_string())
                .collect(),
        );
    }
    catalog_constants.push_str("#[must_use]\npub fn compatibility_alias(group: &str, model: &str) -> Option<&'static str> { match (group, model) {\n");
    for (group, aliases) in reviewed["compatibility_aliases"]
        .as_object()
        .expect("aliases")
    {
        let mut selectors = BTreeSet::new();
        for (alias, target) in aliases.as_object().expect("alias map") {
            let alias = resolve(alias);
            let target = resolve(target.as_str().expect("alias target"));
            assert!(
                selectors.insert(alias.clone()),
                "duplicate resolved selector {alias}"
            );
            catalog_constants.push_str(&format!(
                "({}, {}) => Some({}),\n",
                quoted(group),
                quoted(&alias),
                quoted(&target)
            ));
        }
    }
    catalog_constants.push_str("_ => None, } }\n#[must_use]\npub fn completion_names(provider: &str) -> &'static [&'static str] { match provider {\n");
    for (provider, roster) in reviewed["completion_rosters"]
        .as_object()
        .expect("completion rosters")
    {
        assert!(
            rows.iter().any(|row| row["id"] == provider.as_str()) || provider == text(legacy, "id"),
            "unknown completion provider"
        );
        let mut list = Vec::new();
        for entry in roster.as_array().expect("completion roster") {
            let entry = entry.as_str().expect("completion entry");
            if let Some(group) = entry.strip_prefix('@') {
                list.extend(
                    groups
                        .get(group)
                        .unwrap_or_else(|| panic!("missing group {group}"))
                        .iter()
                        .cloned(),
                );
            } else {
                list.push(resolve(entry));
            }
        }
        // Zen's logical default can be an alias of a documented row. Preserve
        // its original stable, case-insensitive first-occurrence projection.
        if provider == "opencode-zen" {
            let mut seen = BTreeSet::new();
            list.retain(|id| seen.insert(id.to_ascii_lowercase()));
        }
        assert!(
            !matches!(provider.as_str(), "antigravity" | "custom") || list.is_empty(),
            "retired/custom completion rows"
        );
        catalog_constants.push_str(&format!(
            "{} => &[{}],\n",
            quoted(provider),
            list.iter()
                .map(|v| quoted(v))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    catalog_constants.push_str("_ => &[], } }\n");
    fs::write(directory.join("catalog_constants.rs"), catalog_constants)
        .expect("write catalog constants");
}
