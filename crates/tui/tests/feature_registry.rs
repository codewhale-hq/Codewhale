//! Cross-checks `docs/features.toml` (the hand-edited feature registry)
//! against the code, and fails on drift.
//!
//! Checked here: every `[features]` flag in `features.rs` is owned by exactly
//! one row, a flag row's status follows the flag's stage and default, and
//! every `docs`/`owner` path exists. Not checked yet: slash-command ownership
//! and generated docs (later slices of the registry design).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use codewhale_runtime::features::{FEATURES, FeatureSpec, Stage};
use serde::Deserialize;

const STATUSES: &[&str] = &["stable", "experimental", "flagged", "planned"];
const SURFACES: &[&str] = &["tui", "desktop", "web", "api"];
const SURFACE_STATUSES: &[&str] = &["stable", "preview", "partial", "none", "planned"];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    feature: Vec<Row>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Row {
    id: String,
    name: String,
    summary: String,
    status: String,
    #[serde(default)]
    surfaces: BTreeMap<String, String>,
    since: Option<String>,
    flag: Option<String>,
    docs: Option<String>,
    owner: Option<String>,
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[cfg(test)]
fn read(root: &Path, rel: &str) -> String {
    std::fs::read_to_string(root.join(rel)).unwrap_or_else(|err| panic!("read {rel}: {err}"))
}

/// Versions with a `## [x.y.z]` heading in the changelogs.
fn released_versions(root: &Path) -> BTreeSet<String> {
    ["CHANGELOG.md", "docs/CHANGELOG_ARCHIVE.md"]
        .iter()
        .flat_map(|file| {
            read(root, file)
                .lines()
                .filter_map(|line| line.strip_prefix("## ["))
                .filter_map(|rest| rest.split(']').next())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The registry status a flag implies: off by default is `flagged`; on by
/// default is `stable` for Stable flags and `experimental` otherwise.
fn status_for_flag(spec: &FeatureSpec) -> &'static str {
    match (spec.default_enabled, spec.stage) {
        (false, _) => "flagged",
        (true, Stage::Stable) => "stable",
        (true, Stage::Experimental | Stage::Beta) => "experimental",
    }
}

fn is_kebab(id: &str) -> bool {
    !id.is_empty()
        && id.starts_with(|c: char| c.is_ascii_lowercase())
        && !id.ends_with('-')
        && !id.contains("--")
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn check(root: &Path, registry: &Registry) -> Vec<String> {
    let mut errors = Vec::new();
    let versions = released_versions(root);
    let mut ids = BTreeSet::new();
    let mut flag_owners: BTreeMap<&str, Vec<&str>> = BTreeMap::new();

    for row in &registry.feature {
        let id = row.id.as_str();
        let mut err = |msg: String| errors.push(format!("{id}: {msg}"));

        if !is_kebab(id) {
            err("id must be kebab-case".into());
        }
        if !ids.insert(id) {
            err("duplicate id".into());
        }
        if row.name.trim().is_empty() {
            err("name is empty".into());
        }
        let summary_len = row.summary.chars().count();
        if row.summary.trim().is_empty() || summary_len > 120 || row.summary.contains('\n') {
            err(format!(
                "summary must be one line of 1-120 characters (has {summary_len})"
            ));
        }
        if !STATUSES.contains(&row.status.as_str()) {
            err(format!(
                "status {:?} is not one of {STATUSES:?}",
                row.status
            ));
        }
        for (surface, value) in &row.surfaces {
            if !SURFACES.contains(&surface.as_str()) {
                err(format!(
                    "unknown surface {surface:?}; expected one of {SURFACES:?}"
                ));
            }
            if !SURFACE_STATUSES.contains(&value.as_str()) {
                err(format!(
                    "surface {surface} = {value:?} is not one of {SURFACE_STATUSES:?}"
                ));
            }
        }
        if row.status == "planned" && (row.since.is_some() || row.flag.is_some()) {
            err("a planned row cannot have `since` or `flag`".into());
        }
        if let Some(since) = &row.since
            && since != "unreleased"
            && !versions.contains(since)
        {
            err(format!(
                "since = {since:?} is neither \"unreleased\" nor a CHANGELOG heading"
            ));
        }
        if let Some(flag) = &row.flag {
            flag_owners.entry(flag.as_str()).or_default().push(id);
            match FEATURES.iter().find(|spec| spec.key == flag) {
                None => err(format!(
                    "flag {flag:?} is not a key in features.rs FEATURES"
                )),
                Some(spec) => {
                    let want = status_for_flag(spec);
                    if row.status != want {
                        err(format!(
                            "status {:?} disagrees with flag {flag} (stage {}, default {}): expected {want:?}",
                            row.status,
                            spec.stage,
                            if spec.default_enabled { "on" } else { "off" },
                        ));
                    }
                }
            }
        }
        if let Some(docs) = &row.docs {
            let file = docs.split('#').next().unwrap_or_default();
            if !root.join(file).exists() {
                err(format!("docs path {file} does not exist"));
            }
        }
        match &row.owner {
            Some(owner) if !root.join(owner).exists() => {
                err(format!("owner path {owner} does not exist"));
            }
            None if row.status != "planned" => err("a shipped row needs an `owner`".into()),
            _ => {}
        }
    }

    for spec in FEATURES {
        match flag_owners.get(spec.key).map(Vec::as_slice) {
            None | Some([]) => errors.push(format!(
                "flag {} in features.rs has no row in docs/features.toml",
                spec.key
            )),
            Some([_]) => {}
            Some(many) => errors.push(format!(
                "flag {} is claimed by several rows: {}",
                spec.key,
                many.join(", ")
            )),
        }
    }
    errors
}

#[test]
fn feature_registry_matches_code() {
    let root = repo_root();
    let registry: Registry = toml::from_str(&read(&root, "docs/features.toml"))
        .unwrap_or_else(|err| panic!("docs/features.toml does not parse: {err}"));
    assert!(
        !registry.feature.is_empty(),
        "docs/features.toml has no rows"
    );

    let errors = check(&root, &registry);
    assert!(
        errors.is_empty(),
        "docs/features.toml has drifted from the code ({} problem(s)); fix the row or the code:\n  {}",
        errors.len(),
        errors.join("\n  ")
    );
}

#[test]
fn feature_registry_checks_catch_drift() {
    let root = repo_root();
    let registry: Registry = toml::from_str(
        r#"
        [[feature]]
        id = "Bad_Id"
        name = "Wrong status"
        summary = "The vision flag is off by default, so stable is wrong."
        status = "stable"
        surfaces = { phone = "stable" }
        since = "99.0.0"
        flag = "vision_model"
        docs = "docs/NOPE.md#anchor"
        owner = "crates/nope"

        [[feature]]
        id = "twice"
        name = "Duplicate flag"
        summary = "Claims the same flag again."
        status = "flagged"
        flag = "vision_model"
        owner = "crates/tui"
        "#,
    )
    .expect("fixture parses");

    let errors = check(&root, &registry).join("\n");
    for expected in [
        "Bad_Id: id must be kebab-case",
        "unknown surface \"phone\"",
        "since = \"99.0.0\"",
        "expected \"flagged\"",
        "docs path docs/NOPE.md does not exist",
        "owner path crates/nope does not exist",
        "flag vision_model is claimed by several rows",
        "flag shell_tool in features.rs has no row",
    ] {
        assert!(
            errors.contains(expected),
            "missing {expected:?} in:\n{errors}"
        );
    }
}
