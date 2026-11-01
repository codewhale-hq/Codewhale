#!/usr/bin/env python3
"""Check that docs/PROVIDERS.md tracks the shipped provider registry.

This is intentionally lightweight. It does not try to generate prose; it checks
the stable identifiers and default strings that are easy for docs to drift from:

- canonical ProviderKind IDs
- provider TOML tables
- descriptor-owned released presentation identities
- shipped-provider table rows
- static ModelRegistry provider rows
- default provider model/base URL constants
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
CONFIG_RS = ROOT / "crates" / "config" / "src" / "lib.rs"
# ProviderKind's enum + identity impl were split out of lib.rs into this module.
PROVIDER_KIND_RS = ROOT / "crates" / "config" / "src" / "provider_kind.rs"
PROVIDER_RS = ROOT / "crates" / "config" / "src" / "provider.rs"
TUI_CONFIG_RS = ROOT / "crates" / "runtime" / "src" / "config.rs"
# Default provider model/base-URL constants were split out of config.rs into
# this leaf module (#3311); read them from there for the default-string check.
PROVIDER_DATA = ROOT / "crates" / "config" / "assets" / "provider_descriptors.json"
AGENT_RS = ROOT / "crates" / "agent" / "src" / "lib.rs"
PROVIDERS_MD = ROOT / "docs" / "PROVIDERS.md"
CONFIGURATION_MD = ROOT / "docs" / "CONFIGURATION.md"
WEB_FACTS_LIB = ROOT / "web" / "scripts" / "facts-lib.mjs"
WEB_FACTS_DRIFT = ROOT / "web" / "lib" / "facts-drift.ts"
WEB_FACTS_GENERATED = ROOT / "web" / "lib" / "facts.generated.ts"
README_MD = ROOT / "README.md"
CONFIG_EXAMPLE_TOML = ROOT / "config.example.toml"
TUI_PROVIDER_READINESS_RS = ROOT / "crates" / "tui" / "src" / "provider_readiness.rs"
TUI_LIB_RS = ROOT / "crates" / "tui" / "src" / "lib.rs"


LEGACY_PROVIDER_TOMBSTONE_IDS = {"antigravity"}
LEGACY_PROVIDER_TOMBSTONE_TABLES = {"antigravity"}
LEGACY_PROVIDER_SELECTION_IDS = {"antigravity", "agy"}

# `custom` is the dynamic OpenAI-compatible meta-provider (#1519): a single
# catch-all `[providers.custom]` table that backs arbitrary user-defined
# endpoints, not a canonical shipped provider with a docs row. It is excluded
# from the provider-table drift check.
META_PROVIDER_TABLES = {"custom"}
SHARED_PROVIDER_TABLES = {
    "siliconflow-CN": "siliconflow_cn",
}
HUGGINGFACE_ALIASES = {"huggingface", "hugging-face", "hugging_face", "hf"}
HUGGINGFACE_API_KEY_ENV_ORDER = ["HUGGINGFACE_API_KEY", "HF_TOKEN"]
HUGGINGFACE_BASE_URL_ENV_ORDER = ["HUGGINGFACE_BASE_URL", "HF_BASE_URL"]
HUGGINGFACE_MODEL_ENV_ORDER = ["HUGGINGFACE_MODEL", "HF_MODEL"]
SENSITIVE_IDENTIFIER_RE = re.compile(r"(?i)(api[_-]?key|token|secret|password|credential)")
SENSITIVE_BEARER_RE = re.compile(r"(?i)(authorization:\s*bearer\s+)\S+")
SENSITIVE_ASSIGNMENT_RE = re.compile(
    r"(?i)\b(api[_-]?key|token|secret|password|credential)(\s*[:=]\s*)\S+"
)


def read(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def display_public_value(value: str) -> str:
    if SENSITIVE_IDENTIFIER_RE.search(value):
        return "<redacted sensitive identifier>"
    return value


def redact_sensitive_text(value: str) -> str:
    value = SENSITIVE_BEARER_RE.sub(r"\1<redacted>", value)
    value = SENSITIVE_ASSIGNMENT_RE.sub(r"\1\2<redacted>", value)
    return SENSITIVE_IDENTIFIER_RE.sub("<redacted sensitive identifier>", value)


def require_index(source: str, needle: str, context: str, start: int = 0) -> int:
    try:
        return source.index(needle, start)
    except ValueError:
        raise ValueError(f"{context}: missing {needle!r}") from None


def markdown_section(source: str, heading: str) -> str:
    start = require_index(source, heading, "docs/PROVIDERS.md")
    next_heading = source.find("\n## ", start + len(heading))
    end = len(source) if next_heading == -1 else next_heading
    return source[start:end]


def extract_match_block(
    source: str, signature: str, context: str, start: int = 0
) -> str:
    start = require_index(source, signature, context, start)
    match_start = require_index(source, "match", f"match block after {signature!r}", start)
    brace_start = require_index(source, "{", f"match block after {signature!r}", match_start)
    depth = 0
    for index in range(brace_start, len(source)):
        char = source[index]
        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                return source[brace_start + 1 : index]
    raise ValueError(f"could not parse match block after {signature!r}")


def provider_data() -> dict:
    data = json.loads(read(PROVIDER_DATA))
    if data.get("schema_version") != 3 or len(data.get("providers", [])) != 52:
        raise ValueError("unsupported or incomplete provider metadata")
    rows = [*data["providers"], data.get("legacy_tui", {})]
    fields = ("id", "kind", "tui_wire_tag", "config_key", "catalog_id", "catalog_source_id")
    if len(rows) != 53 or any(any(not isinstance(row.get(field), str) or not row[field] for field in fields) for row in rows):
        raise ValueError("incomplete released presentation metadata")
    for field in ("id", "tui_wire_tag", "tui_order"):
        if len({row.get(field) for row in rows}) != len(rows):
            raise ValueError(f"duplicate presentation {field}")
    if {row["tui_order"] for row in rows} != set(range(len(rows))):
        raise ValueError("noncontiguous released presentation order")
    if data["legacy_tui"]["kind"] not in {row["kind"] for row in data["providers"]}:
        raise ValueError("legacy presentation has no intrinsic provider")
    return data


def parse_aliases_for_variant(source: str, enum_name: str, variant: str, context: str) -> set[str]:
    # Both selectors delegate aliases to the same descriptor-backed facade.
    for row in provider_data()["providers"]:
        if row["kind"] == variant:
            return {row["id"], *row["aliases"]}
    raise ValueError(f"{context}: missing descriptor for {variant}")


def provider_kind_ids(config_rs: str) -> dict[str, str]:
    rows = provider_data()["providers"]
    ids = {row["kind"]: row["id"] for row in rows if row["kind"] != "Custom"}
    if len({row["kind"] for row in rows}) != len(rows) or len({row["id"] for row in rows}) != len(rows):
        raise ValueError("duplicate built-in descriptor identity")
    return ids


def provider_kind_catalog_ids(provider_kind_rs: str, variant_to_id: dict[str, str]) -> set[str]:
    if not re.search(r"pub const ALL:.*?=\s*crate::descriptors::SELECTABLE_PROVIDER_KINDS;", provider_kind_rs):
        raise ValueError("ProviderKind::ALL must use generated descriptor selection")
    return {row["id"] for row in provider_data()["providers"] if row["selectable"]}


def presentation_provider_ids() -> set[str]:
    data = provider_data()
    return {row["id"] for row in [*data["providers"], data["legacy_tui"]] if row["kind"] != "Custom"}


def provider_tables(config_rs: str) -> set[str]:
    struct_start = require_index(
        config_rs, "pub struct ProvidersToml", "crates/config/src/provider_toml.rs"
    )
    struct_end = require_index(config_rs, "\n}", "ProvidersToml struct", struct_start)
    fields = re.findall(
        r"pub\s+([a-z0-9_]+)\s*:\s*ProviderConfigToml",
        config_rs[struct_start:struct_end],
    )
    if not fields:
        raise ValueError("ProvidersToml returned no provider tables")
    return set(fields)


def shipped_provider_rows(providers_md: str) -> set[str]:
    table = markdown_section(providers_md, "## Shipped Providers")
    return set(re.findall(r"^\|\s*`([^`]+)`\s*\|", table, flags=re.MULTILINE))


def shipped_provider_tables(providers_md: str) -> set[str]:
    table = markdown_section(providers_md, "## Shipped Providers")
    return set(re.findall(r"\|\s*`\[providers\.([a-z0-9_]+)\]`\s*\|", table))


def documented_selectable_provider_ids(providers_md: str) -> set[str]:
    marker = require_index(providers_md, "in that order:", "docs/PROVIDERS.md")
    start = require_index(providers_md, "\n\n", "provider selection list", marker) + 2
    end = require_index(providers_md, "\n\n", "provider selection list", start)
    return set(re.findall(r"`([^`]+)`", providers_md[start:end]))


def report_provider_kind_selector_contract(provider_kind_rs: str) -> list[str]:
    start = require_index(
        provider_kind_rs,
        "pub fn parse(value: &str) -> Option<Self>",
        "ProviderKind::parse",
    )
    end = require_index(
        provider_kind_rs, "pub fn parse_config_identity", "ProviderKind::parse", start
    )
    selector = provider_kind_rs[start:end]
    if "Self::ALL" not in selector and "Self::all()" not in selector:
        return [
            "ProviderKind::parse must gate registry aliases through the selectable "
            "ProviderKind::ALL catalog"
        ]
    return []


def report_tui_catalog_contract(tui_config_rs: str) -> list[str]:
    errors = []
    if re.search(r"(?:enum|impl|type)\s+ApiProvider\b|KIND_LOOKUP|FROM_KIND_LOOKUP", tui_config_rs):
        errors.append("TUI must not define a duplicate provider enum or ordinal bridge")
    # The exact private historical enum in config/tests.rs is the serde
    # counterpart, not a production owner. Check every other Rust consumer.
    counterpart = ROOT / "crates/tui/src/config_stay_tests/config.rs"
    for path in (ROOT / "crates").rglob("*.rs"):
        source = read(path)
        if path != counterpart and re.search(r"\bApiProvider\b", source):
            errors.append(f"retired provider enum consumer: {path.relative_to(ROOT)}")
        if re.search(r"KIND_LOOKUP|FROM_KIND_LOOKUP|ProviderKind::from_kind\(", source):
            errors.append(f"retired provider ordinal bridge: {path.relative_to(ROOT)}")
    for function_name in (
        "active_provider_identity",
        "provider_identities",
        "resolve_provider_selection_identity",
        "verify_provider_identity",
    ):
        if not re.search(rf"\bpub(?:\(crate\))?\s+fn\s+{function_name}\b", tui_config_rs):
            errors.append(f"missing canonical config identity boundary: {function_name}")
    descriptor_source = read(ROOT / "crates/config/src/descriptors.rs")
    if "PROVIDER_COMPATIBILITY" not in descriptor_source or "pub fn compatibility_for_selector" not in descriptor_source:
        errors.append("released presentation identities must use the existing generated descriptor owner")
    if "is_legacy_antigravity_identity(requested)" not in tui_config_rs:
        errors.append("explicit provider selection must refuse retired Antigravity aliases")
    return errors


def report_tombstone_runtime_contract(
    provider_kind_rs: str, tui_provider_readiness_rs: str, tui_lib_rs: str
) -> list[str]:
    """The tombstone must resolve under every legacy spelling and never read
    as a credentialed or advertised slot on a running-product surface."""

    errors: list[str] = []
    start = require_index(
        provider_kind_rs,
        "pub fn parse_config_identity(value: &str) -> Option<Self>",
        "ProviderKind::parse_config_identity",
    )
    end = require_index(
        provider_kind_rs, "pub fn secret_store_slot", "ProviderKind::parse_config_identity", start
    )
    config_identity = provider_kind_rs[start:end]
    if "parse_retired_alias" not in config_identity:
        errors.append(
            "ProviderKind::parse_config_identity must resolve retired registry aliases "
            "(`agy`) so every selection surface can name the tombstone"
        )

    if (
        "provider == ProviderKind::Antigravity"
        not in tui_provider_readiness_rs
    ):
        errors.append(
            "provider_readiness::credential_state_for_provider must classify "
            "ProviderKind::Antigravity as CredentialState::Legacy"
        )

    if "for provider in doctor_api_key_providers()" not in tui_lib_rs or (
        "*provider != codewhale_config::ProviderKind::Antigravity" not in tui_lib_rs
    ):
        errors.append(
            "`codewhale doctor` API Keys rows must iterate doctor_api_key_providers() "
            "with the retired Antigravity slot filtered out"
        )
    return errors


def report_antigravity_public_contract(
    providers_md: str,
    configuration_md: str,
    web_facts_lib: str,
    web_facts_drift: str,
    web_facts_generated: str,
    readme_md: str,
    config_example_toml: str,
) -> list[str]:
    """Keep the retired provider as one safe, non-runnable docs tombstone."""

    errors: list[str] = []
    heading = "### Legacy Antigravity tombstone"
    heading_count = providers_md.count(heading)
    if heading_count != 1:
        errors.append(
            "docs/PROVIDERS.md must contain exactly one legacy Antigravity tombstone "
            f"heading (found {heading_count})"
        )
        tombstone = ""
        outside_tombstone = providers_md
    else:
        start = providers_md.index(heading)
        next_heading = re.search(r"\n#{1,3} ", providers_md[start + len(heading) :])
        end = (
            len(providers_md)
            if next_heading is None
            else start + len(heading) + next_heading.start()
        )
        tombstone = providers_md[start:end]
        outside_tombstone = providers_md[:start] + providers_md[end:]

    normalized_tombstone = " ".join(tombstone.split())
    required_tombstone_copy = [
        "not a Codewhale provider",
        "cannot be selected or run",
        "non-runnable migration tombstone",
        "`codewhale auth clear --provider antigravity`",
        "Codewhale-owned legacy configuration and consent metadata",
        "does not sign out of, revoke, read, or otherwise alter any official Google or Antigravity session",
        "supported `google` provider",
        "`GEMINI_API_KEY`",
    ]
    missing_tombstone_copy = [
        required
        for required in required_tombstone_copy
        if required not in normalized_tombstone
    ]
    if missing_tombstone_copy:
        errors.append(
            "legacy Antigravity tombstone is missing required safety or migration copy "
            f"({len(missing_tombstone_copy)} checks failed)"
        )
    clear_command = "`codewhale auth clear --provider antigravity`"
    legacy_provider_forms = [
        match.lower()
        for match in re.findall(
            r"--provider\s+(antigravity|agy)\b", providers_md, flags=re.IGNORECASE
        )
    ]
    if providers_md.count(clear_command) != 1 or legacy_provider_forms != [
        "antigravity"
    ]:
        errors.append(
            "docs/PROVIDERS.md must contain the Codewhale-owned Antigravity "
            "clear command as its only --provider antigravity/agy form"
        )
    setup_guidance = re.search(
        r"\bagy\b|\boauth\b|\blog(?:in|\s+in)\b|\bsign\s+in\b|"
        r"\bimport\b|\bexternal-consent\b|/provider\s+(?:antigravity|agy)\b|"
        r"CODEWHALE_PROVIDER\s*=\s*(?:antigravity|agy)\b",
        tombstone,
        flags=re.IGNORECASE,
    )
    if setup_guidance:
        errors.append(
            "legacy Antigravity tombstone contains login, OAuth import, consent, "
            "or provider-selection guidance"
        )

    if re.search(r"\b(?:antigravity|agy)\b", outside_tombstone, flags=re.IGNORECASE):
        errors.append(
            "docs/PROVIDERS.md mentions Antigravity/agy outside its legacy tombstone"
        )
    if re.search(r"\b(?:antigravity|agy)\b", configuration_md, flags=re.IGNORECASE):
        errors.append("docs/CONFIGURATION.md advertises retired Antigravity state")
    if re.search(r"\b(?:antigravity|agy)\b", readme_md, flags=re.IGNORECASE):
        errors.append("README.md advertises retired Antigravity state")
    if re.search(r"\b(?:antigravity|agy)\b", config_example_toml, flags=re.IGNORECASE):
        errors.append("config.example.toml advertises retired Antigravity state")
    if "[providers.google]" not in config_example_toml or not re.search(
        r"GEMINI_API_KEY", config_example_toml
    ):
        errors.append(
            "config.example.toml must document the supported `google` Gemini route "
            "with GEMINI_API_KEY"
        )

    forbidden_markers = {
        "Antigravity API-key environment guidance": "ANTIGRAVITY_API_KEY",
        "Antigravity ADC environment guidance": "AGY_ADC_AUTH",
        "Antigravity base-URL environment guidance": "ANTIGRAVITY_BASE_URL",
        "Antigravity model environment guidance": "ANTIGRAVITY_MODEL",
        "private cloud-code endpoint guidance": "cloudcode-pa",
        "private cloud-code protocol guidance": "cloud-code",
        "official CLI credential-store guidance": "state.vscdb",
        "official CLI OAuth-state guidance": "antigravityUnifiedStateSync",
        "runnable legacy provider selection": 'provider = "antigravity"',
        "runnable legacy provider table": "[providers.antigravity]",
    }
    public_sources = {
        "docs/PROVIDERS.md": providers_md,
        "docs/CONFIGURATION.md": configuration_md,
        "web/scripts/facts-lib.mjs": web_facts_lib,
        "web/lib/facts-drift.ts": web_facts_drift,
        "web/lib/facts.generated.ts": web_facts_generated,
        "README.md": readme_md,
        "config.example.toml": config_example_toml,
    }
    for context, source in public_sources.items():
        for description, marker in forbidden_markers.items():
            if marker.lower() in source.lower():
                errors.append(f"{context} contains forbidden {description}")

    projection = read(ROOT / "web/lib/provider-descriptors.mjs")
    if 'row.retired || row.kind === "Antigravity" || row.kind === "Custom"' not in projection:
        errors.append("shared website descriptor projection must exclude retired/Antigravity/custom rows")
    for context, source in [("web/scripts/facts-lib.mjs", web_facts_lib), ("web/lib/facts-drift.ts", web_facts_drift)]:
        if "parseProviderDescriptors" not in source:
            errors.append(f"{context} must use the shared validated descriptor projection")
        if re.search(r"^\s*Antigravity\s*:", source, flags=re.MULTILINE):
            errors.append(f"{context} maps legacy Antigravity to public provider facts")
        if re.search(r"\bagy\b", source, flags=re.IGNORECASE):
            errors.append(f"{context} exposes the legacy agy alias")

    if re.search(
        r"\b(?:antigravity|agy)\b", web_facts_generated, flags=re.IGNORECASE
    ):
        errors.append("web/lib/facts.generated.ts exposes legacy Antigravity/agy")

    return errors


def static_registry_provider_rows(providers_md: str) -> set[str]:
    table = markdown_section(providers_md, "## Static Model Registry")
    return set(re.findall(r"^\|\s*`([^`]+)`\s*\|", table, flags=re.MULTILINE))


def model_registry_providers(agent_rs: str, variant_to_id: dict[str, str]) -> set[str]:
    # ModelRegistry is now a compatibility projection of the reviewed catalog.
    # Reading tests or caller literals cannot reconstruct its shipping roster.
    data = json.loads(read(ROOT / "crates/config/assets/catalog_corrections.json"))
    reviewed = data.get("reviewed")
    if not isinstance(reviewed, dict) or not reviewed.get("revision"):
        raise ValueError("reviewed catalog metadata missing")
    rows = reviewed.get("selections")
    if not isinstance(rows, list) or not rows:
        raise ValueError("reviewed catalog selections missing")
    ids = {row.get("provider") for row in rows if isinstance(row, dict)}
    if None in ids or ids - set(variant_to_id.values()):
        raise ValueError("reviewed catalog uses missing/unknown provider identity")
    if not re.search(r"bundled_reviewed\(\)\s*\.selections", agent_rs):
        raise ValueError("ModelRegistry must project the shared reviewed selection rows")
    return ids


def default_strings(tui_config_rs: str) -> set[str]:
    data = provider_data()
    rows = {row["id"]: row for row in data["providers"]}
    values = dict(data["compatibility_constants"])
    for name, ref in data["constant_refs"].items():
        row = rows[ref["provider"]]
        values[name] = (row["credential_help"] if ref["field"] == "credential_url" else row)[ref["field"]]
    defaults = {value for name, value in values.items()
                if re.fullmatch(r"DEFAULT_[A-Z0-9_]+(?:MODEL|BASE_URL)", name)
                and name != "DEFAULT_DEEPSEEKCN_BASE_URL"
                and not name.startswith("DEFAULT_ANTIGRAVITY_")}
    if not defaults:
        raise ValueError("no default provider metadata found")
    return defaults


def missing_default_strings(providers_md: str, defaults: set[str]) -> list[str]:
    # Inline-code validation should not let fenced TOML/bash examples pair a
    # stray backtick with later prose; strip fenced blocks before scanning.
    inline_source = re.sub(r"```.*?```", "", providers_md, flags=re.DOTALL)
    code_spans = set(re.findall(r"`([^`]+)`", inline_source))
    return sorted(defaults - code_spans)


def report_set(label: str, expected: set[str], actual: set[str]) -> list[str]:
    errors = []
    missing = sorted(expected - actual)
    extra = sorted(actual - expected)
    if missing:
        errors.append(f"{label} missing: {', '.join(missing)}")
    if extra:
        errors.append(f"{label} extra: {', '.join(extra)}")
    return errors


def report_presentation_identity_drift(canonical_ids: set[str], presentation_ids: set[str]) -> list[str]:
    legacy = provider_data()["legacy_tui"]["id"]
    return report_set("released presentation identities", canonical_ids | {legacy}, presentation_ids)


def report_huggingface_coverage(
    config_rs: str, provider_rs: str, tui_config_rs: str, providers_md: str
) -> list[str]:
    errors = []

    config_aliases = parse_aliases_for_variant(
        config_rs, "ProviderKind", "Huggingface", "crates/config/src/lib.rs"
    )
    tui_aliases = parse_aliases_for_variant(
        tui_config_rs, "ProviderIdentity", "Huggingface", "crates/runtime/src/config.rs"
    )
    errors += report_set(
        "ProviderKind Hugging Face aliases",
        HUGGINGFACE_ALIASES,
        config_aliases & HUGGINGFACE_ALIASES,
    )
    errors += report_set(
        "descriptor presentation Hugging Face aliases",
        HUGGINGFACE_ALIASES,
        tui_aliases & HUGGINGFACE_ALIASES,
    )

    inline_source = re.sub(r"```.*?```", "", providers_md, flags=re.DOTALL)
    code_spans = set(re.findall(r"`([^`]+)`", inline_source))
    errors += report_set(
        "documented Hugging Face aliases",
        HUGGINGFACE_ALIASES,
        code_spans & HUGGINGFACE_ALIASES,
    )

    # API-key lookup order is descriptor-owned; actual TUI environment
    # resolution remains independently checked below.
    auth_label = "Hugging Face auth env precedence"
    row = next(row for row in provider_data()["providers"] if row["kind"] == "Huggingface")
    if row["env_vars"] != HUGGINGFACE_API_KEY_ENV_ORDER:
        errors.append("Hugging Face descriptor auth env precedence differs")
    for label, env_order in [
        (auth_label, HUGGINGFACE_API_KEY_ENV_ORDER),
        ("Hugging Face base URL env precedence", HUGGINGFACE_BASE_URL_ENV_ORDER),
        ("Hugging Face model env precedence", HUGGINGFACE_MODEL_ENV_ORDER),
    ]:
        if label != auth_label:
            errors += report_env_lookup_order(
                label, config_rs, env_order, "crates/config/src/lib.rs"
            )
        errors += report_env_lookup_order(
            label, tui_config_rs, env_order, "crates/runtime/src/config.rs"
        )
        errors += report_string_order(label, providers_md, env_order, "docs/PROVIDERS.md")

    return errors


def report_env_lookup_order(
    label: str, source: str, expected_order: list[str], context: str
) -> list[str]:
    lookup_needles = [f'std::env::var("{name}")' for name in expected_order]
    return report_string_order(label, source, lookup_needles, context)


def report_string_order(
    label: str, source: str, expected_order: list[str], context: str
) -> list[str]:
    contains_sensitive_expected_value = any(
        SENSITIVE_IDENTIFIER_RE.search(value) for value in expected_order
    )
    positions = []
    for needle in expected_order:
        index = source.find(needle)
        if index == -1:
            if contains_sensitive_expected_value:
                return [f"{label} missing required entry in {context}"]
            return [f"{label} missing {display_public_value(needle)!r} in {context}"]
        positions.append(index)
    if positions != sorted(positions):
        if contains_sensitive_expected_value:
            return [f"{label} has wrong order in {context}"]
        return [
            f"{label} has wrong order in {context}: expected "
            + " before ".join(display_public_value(value) for value in expected_order)
        ]
    return []


def provider_table_name(provider_id: str) -> str:
    return SHARED_PROVIDER_TABLES.get(provider_id, provider_id.replace("-", "_"))


def main() -> int:
    try:
        config_rs = "\n".join(read(path) for path in sorted(CONFIG_RS.parent.glob("*.rs")) if path.name != "tests.rs")
        provider_kind_rs = read(PROVIDER_KIND_RS)
        tui_config_rs = read(TUI_CONFIG_RS)
        agent_rs = read(AGENT_RS)
        providers_md = read(PROVIDERS_MD)
        configuration_md = read(CONFIGURATION_MD)
        web_facts_lib = read(WEB_FACTS_LIB)
        web_facts_drift = read(WEB_FACTS_DRIFT)
        web_facts_generated = read(WEB_FACTS_GENERATED)
        readme_md = read(README_MD)
        config_example_toml = read(CONFIG_EXAMPLE_TOML)
        tui_provider_readiness_rs = read(TUI_PROVIDER_READINESS_RS)
        tui_lib_rs = read(TUI_LIB_RS)

        variant_to_id = provider_kind_ids(config_rs)
        canonical_ids = set(variant_to_id.values())
        selectable_provider_ids = provider_kind_catalog_ids(
            provider_kind_rs, variant_to_id
        )
        presentation_ids = presentation_provider_ids()
        public_provider_ids = canonical_ids - LEGACY_PROVIDER_TOMBSTONE_IDS
        expected_tables = {
            provider_table_name(provider_id) for provider_id in public_provider_ids
        }
        runtime_tables = expected_tables | LEGACY_PROVIDER_TOMBSTONE_TABLES

        errors: list[str] = []
        errors += report_presentation_identity_drift(canonical_ids, presentation_ids)
        errors += report_provider_kind_selector_contract(provider_kind_rs)
        errors += report_tui_catalog_contract(tui_config_rs)
        errors += report_tombstone_runtime_contract(
            provider_kind_rs, tui_provider_readiness_rs, tui_lib_rs
        )
        errors += report_set(
            "legacy provider identities in ProviderKind::ALL",
            set(),
            selectable_provider_ids & LEGACY_PROVIDER_SELECTION_IDS,
        )
        errors += report_set(
            "documented selectable provider IDs",
            selectable_provider_ids,
            documented_selectable_provider_ids(providers_md),
        )
        errors += report_huggingface_coverage(
            config_rs, read(PROVIDER_RS), tui_config_rs, providers_md
        )
        errors += report_antigravity_public_contract(
            providers_md,
            configuration_md,
            web_facts_lib,
            web_facts_drift,
            web_facts_generated,
            readme_md,
            config_example_toml,
        )
        errors += report_set(
            "shipped provider rows",
            public_provider_ids,
            shipped_provider_rows(providers_md),
        )
        errors += report_set(
            "provider TOML tables",
            runtime_tables,
            provider_tables(config_rs) - META_PROVIDER_TABLES,
        )
        errors += report_set(
            "documented provider TOML tables",
            expected_tables,
            shipped_provider_tables(providers_md),
        )
        errors += report_set(
            "static ModelRegistry rows",
            model_registry_providers(agent_rs, variant_to_id),
            static_registry_provider_rows(providers_md),
        )

        missing_defaults = missing_default_strings(providers_md, default_strings(tui_config_rs))
        if missing_defaults:
            errors.append(
                "docs/PROVIDERS.md does not mention default strings as Markdown code spans: "
                + ", ".join(missing_defaults)
            )
    except ValueError as err:
        errors = [str(err)]

    if errors:
        print("Provider registry drift check failed:", file=sys.stderr)
        for error in errors:
            print(f"- {redact_sensitive_text(error)}", file=sys.stderr)
        return 1

    print("Provider registry drift check passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
