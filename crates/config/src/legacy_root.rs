//! Canonicalize the legacy top-level `base_url` / `api_key` into
//! `[providers.<name>]` tables (#6394).
//!
//! Older releases kept DeepSeek's endpoint and key at the root of
//! `config.toml`, and every reader then decided for itself which routes
//! inherited them. Those readers disagreed. This module is now the only place
//! that interprets the root keys: every parse runs [`apply_to_table`] before
//! the typed structs see the document (which no longer have root fields), and
//! every user-started write runs [`apply_to_document`], so memory and disk
//! follow one rule.
//!
//! Where the top-level `base_url` goes, per scope (the file itself and each
//! `[profiles.<name>]`):
//!
//! 1. `provider = "custom"` with no `[providers.custom]` table: the endpoint,
//!    key and a copy of the model become `[providers.custom]`.
//! 2. An endpoint on another vendor's official host moves to that vendor's
//!    table, so a DeepSeek route never dispatches to it.
//! 3. Anything else belongs to `[providers.deepseek]`, its historical owner.
//!
//! The key goes with DeepSeek (or the literal custom route). It follows the
//! endpoint to another vendor only when the same table explicitly selects that
//! vendor, the host is that vendor's own, and the vendor's table has no key.
//!
//! When the root and the table disagree, memory resolves it (the table's
//! `base_url` wins; the root `api_key` wins, because that is the key the TUI
//! sent) but disk never does: both keys stay where they are until the user
//! runs `codewhale config migrate --prefer ...` or writes that value.

use std::fmt;

use anyhow::{Context as _, Result};

use crate::ProviderKind;

/// The root key names older releases accepted, canonical name first.
const BASE_URL_KEYS: [&str; 2] = ["base_url", "baseUrl"];
const API_KEY_KEYS: [&str; 2] = ["api_key", "apiKey"];

/// Hosts whose root `base_url` belongs to a non-DeepSeek vendor even when the
/// path is not one of that vendor's exact official endpoints. This is the list
/// the DeepSeek route used to refuse at dispatch time.
const FOREIGN_HOST_NEEDLES: &[(&str, ProviderKind)] = &[
    ("integrate.api.nvidia.com", ProviderKind::NvidiaNim),
    ("api.openai.com", ProviderKind::Openai),
    ("api.atlascloud.ai", ProviderKind::Atlascloud),
    ("maas-openapi.wanjiedata.com", ProviderKind::WanjieArk),
    ("volces.com", ProviderKind::Volcengine),
    ("openrouter.ai", ProviderKind::Openrouter),
    ("xiaomimimo.com", ProviderKind::XiaomiMimo),
    ("novita.ai", ProviderKind::Novita),
    ("fireworks.ai", ProviderKind::Fireworks),
    ("siliconflow", ProviderKind::Siliconflow),
    ("arcee.ai", ProviderKind::Arcee),
    ("moonshot.ai", ProviderKind::Moonshot),
    ("api.kimi.com", ProviderKind::Moonshot),
];

/// Which field a note is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyRootField {
    BaseUrl,
    ApiKey,
}

impl LegacyRootField {
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::BaseUrl => "base_url",
            Self::ApiKey => "api_key",
        }
    }
}

/// How a conflicting pair is resolved on disk by `codewhale config migrate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyRootPrefer {
    /// Keep the top-level value; it replaces the table value.
    TopLevel,
    /// Keep the table value; the top-level value is removed.
    Table,
}

/// One thing the canonicalizer did or found. Never carries a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LegacyRootNote {
    /// A top-level key moved into a table.
    Moved {
        scope: Option<String>,
        field: LegacyRootField,
        to: String,
    },
    /// The table already held the same value; the top-level copy was removed.
    Merged {
        scope: Option<String>,
        field: LegacyRootField,
        to: String,
    },
    /// An empty top-level value was removed.
    DroppedEmpty {
        scope: Option<String>,
        field: LegacyRootField,
    },
    /// The top-level key was copied into `[vision_model]`, which inherited it.
    CopiedToVision { scope: Option<String> },
    /// The literal custom route's model was copied into `[providers.custom]`.
    CopiedModel { scope: Option<String> },
    /// `provider` was written because older releases guessed it from the URL.
    GuessedProvider {
        scope: Option<String>,
        provider: &'static str,
    },
    /// The top-level value and the table value differ.
    Conflict {
        scope: Option<String>,
        field: LegacyRootField,
        table: String,
        /// Whether the conflict was resolved (in memory, or by `--prefer`).
        resolved: bool,
    },
}

/// Receipt for one canonicalization pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LegacyRootMigration {
    pub notes: Vec<LegacyRootNote>,
}

impl LegacyRootMigration {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.notes.is_empty()
    }

    /// Whether anything in the file changed (or would change).
    #[must_use]
    pub fn changes_file(&self) -> bool {
        self.notes.iter().any(|note| match note {
            LegacyRootNote::Conflict { resolved, .. } => *resolved,
            _ => true,
        })
    }

    /// Conflicts that are still present in the file.
    pub fn unresolved_conflicts(&self) -> impl Iterator<Item = &LegacyRootNote> {
        self.notes.iter().filter(|note| {
            matches!(
                note,
                LegacyRootNote::Conflict {
                    resolved: false,
                    ..
                }
            )
        })
    }

    /// Whether the file still has legacy top-level keys to move.
    #[must_use]
    pub fn has_pending_moves(&self) -> bool {
        self.notes.iter().any(|note| {
            matches!(
                note,
                LegacyRootNote::Moved { .. }
                    | LegacyRootNote::Merged { .. }
                    | LegacyRootNote::DroppedEmpty { .. }
            )
        })
    }

    /// Whether the top-level (not per-profile) `api_key` moved into
    /// `[providers.<table>]`, where that table had no key of its own.
    #[must_use]
    pub fn moved_root_api_key_to(&self, table: &str) -> bool {
        self.notes.iter().any(|note| {
            matches!(
                note,
                LegacyRootNote::Moved {
                    scope: None,
                    field: LegacyRootField::ApiKey,
                    to,
                } if to.strip_prefix("providers.") == Some(table)
            )
        })
    }

    /// One line per note, suitable for CLI output and doctor.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        self.notes.iter().map(ToString::to_string).collect()
    }

    /// Short one-line summary of what moved.
    #[must_use]
    pub fn summary(&self) -> Option<String> {
        let moved: Vec<String> = self
            .notes
            .iter()
            .filter_map(|note| match note {
                LegacyRootNote::Moved { scope, field, to }
                | LegacyRootNote::Merged { scope, field, to } => {
                    Some(format!("{}{} to [{to}]", scope_prefix(scope), field.key()))
                }
                _ => None,
            })
            .collect();
        (!moved.is_empty()).then(|| format!("moved top-level {}", moved.join(", ")))
    }
}

fn scope_prefix(scope: &Option<String>) -> String {
    scope
        .as_deref()
        .map(|name| format!("profiles.{name}."))
        .unwrap_or_default()
}

impl fmt::Display for LegacyRootNote {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Moved { scope, field, to } => write!(
                f,
                "moved top-level {}{} to [{to}]",
                scope_prefix(scope),
                field.key()
            ),
            Self::Merged { scope, field, to } => write!(
                f,
                "removed top-level {}{}; [{to}] already has the same value",
                scope_prefix(scope),
                field.key()
            ),
            Self::DroppedEmpty { scope, field } => write!(
                f,
                "removed empty top-level {}{}",
                scope_prefix(scope),
                field.key()
            ),
            Self::CopiedToVision { scope } => write!(
                f,
                "copied top-level {}api_key into [{}vision_model], which used it",
                scope_prefix(scope),
                scope_prefix(scope)
            ),
            Self::CopiedModel { scope } => write!(
                f,
                "copied the {}custom route's model into [{}providers.custom]",
                scope_prefix(scope),
                scope_prefix(scope)
            ),
            Self::GuessedProvider { scope, provider } => write!(
                f,
                "set {}provider = \"{provider}\" (older releases inferred it from base_url)",
                scope_prefix(scope)
            ),
            Self::Conflict {
                scope,
                field,
                table,
                resolved,
            } => {
                let in_use = match field {
                    LegacyRootField::BaseUrl => format!("[{table}] {}", field.key()),
                    LegacyRootField::ApiKey => format!("top-level {}", field.key()),
                };
                if *resolved {
                    write!(
                        f,
                        "resolved conflicting top-level {}{} and [{table}] {}",
                        scope_prefix(scope),
                        field.key(),
                        field.key()
                    )
                } else {
                    write!(
                        f,
                        "top-level {}{} differs from [{table}] {}; {in_use} is in use \
                         (run `codewhale config migrate --prefer top-level|table`)",
                        scope_prefix(scope),
                        field.key(),
                        field.key()
                    )
                }
            }
        }
    }
}

/// A primitive edit, shared by the in-memory and on-disk appliers.
#[derive(Debug, Clone, PartialEq)]
enum Op {
    Set(Vec<String>, toml::Value),
    /// Set `to`, then remove `from` only if the set landed.
    Move {
        from: Vec<String>,
        to: Vec<String>,
        value: toml::Value,
    },
    /// A conflicting pair left in place on disk (no-op for both appliers).
    KeepConflict {
        root: Vec<String>,
        table: Vec<String>,
    },
    /// Set only when the destination is absent.
    SetIfAbsent(Vec<String>, toml::Value),
    Remove(Vec<String>),
}

#[derive(Debug, Clone, PartialEq)]
struct PlannedOp {
    op: Op,
    note: Option<LegacyRootNote>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Memory,
    Document(Option<LegacyRootPrefer>),
}

/// The vendor whose official host a root `base_url` names, if it is not
/// DeepSeek's. Loopback hosts and Codewhale's own env-dependent endpoint never
/// count: a local server is not another vendor.
#[must_use]
pub fn legacy_root_owner(base_url: &str) -> Option<ProviderKind> {
    let trimmed = base_url.trim();
    if trimmed.is_empty() || crate::base_url_uses_local_host(trimmed) {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    if crate::device_code::url_scheme_and_host(trimmed).is_ok_and(|(scheme, host, credentials)| {
        scheme == "https" && host == "chatgpt.com" && !credentials
    }) {
        return Some(ProviderKind::OpenaiCodex);
    }
    if lower.contains("integrate.api.nvidia.com") {
        return Some(ProviderKind::NvidiaNim);
    }
    ProviderKind::ALL
        .iter()
        .copied()
        .filter(|kind| {
            !matches!(
                kind,
                ProviderKind::Deepseek | ProviderKind::Custom | ProviderKind::Codewhale
            )
        })
        .find(|kind| crate::provider_base_url_is_official(*kind, trimmed))
        .or_else(|| {
            FOREIGN_HOST_NEEDLES
                .iter()
                .find(|(needle, _)| lower.contains(needle))
                .map(|(_, kind)| *kind)
        })
}

/// Whether `base_url` is on `provider`'s own host family.
fn host_belongs_to(provider: ProviderKind, base_url: &str) -> bool {
    if crate::base_url_uses_local_host(base_url) {
        return false;
    }
    let lower = base_url.trim().to_ascii_lowercase();
    crate::provider_base_url_is_official(provider, base_url)
        || FOREIGN_HOST_NEEDLES
            .iter()
            .any(|(needle, kind)| *kind == provider && lower.contains(needle))
}

fn string_at<'a>(table: &'a toml::Table, key: &str) -> Option<&'a str> {
    table.get(key).and_then(toml::Value::as_str)
}

fn table_at<'a>(table: &'a toml::Table, key: &str) -> Option<&'a toml::Table> {
    table.get(key).and_then(toml::Value::as_table)
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// First present key of an alias pair: `(key name, value)`.
fn root_entry<'a>(
    table: &'a toml::Table,
    keys: &[&'static str],
) -> Option<(&'static str, &'a toml::Value)> {
    keys.iter()
        .find_map(|key| table.get(*key).map(|value| (*key, value)))
}

fn is_blank(value: &toml::Value) -> bool {
    value.as_str().is_some_and(|value| value.trim().is_empty())
}

struct Scope<'a> {
    name: Option<String>,
    /// Path prefix of this scope's table (empty for the file itself).
    prefix: Vec<String>,
    table: &'a toml::Table,
    /// The base file's table when this scope is a profile.
    base: Option<&'a toml::Table>,
}

impl Scope<'_> {
    fn path(&self, parts: &[&str]) -> Vec<String> {
        let mut path = self.prefix.clone();
        path.extend(parts.iter().map(|part| (*part).to_string()));
        path
    }

    fn label(&self, parts: &[&str]) -> String {
        self.path(parts).join(".")
    }

    fn provider(&self) -> Option<&str> {
        nonempty(string_at(self.table, "provider")).or_else(|| {
            self.base
                .and_then(|base| nonempty(string_at(base, "provider")))
        })
    }

    fn own_provider_kind(&self) -> Option<ProviderKind> {
        nonempty(string_at(self.table, "provider")).and_then(ProviderKind::parse_config_identity)
    }

    fn provider_table(&self, key: &str) -> Option<&toml::Table> {
        table_at(self.table, "providers").and_then(|providers| table_at(providers, key))
    }

    fn has_literal_custom_table(&self) -> bool {
        let has = |table: &toml::Table| {
            table_at(table, "providers").is_some_and(|providers| {
                providers
                    .keys()
                    .any(|key| key.trim().eq_ignore_ascii_case("custom"))
            })
        };
        has(self.table) || self.base.is_some_and(has)
    }

    fn selects_literal_custom(&self) -> bool {
        self.provider()
            .is_some_and(|provider| provider.eq_ignore_ascii_case("custom"))
    }
}

fn plan(root: &toml::Table, mode: Mode) -> Vec<PlannedOp> {
    let mut ops = Vec::new();
    plan_scope(
        &Scope {
            name: None,
            prefix: Vec::new(),
            table: root,
            base: None,
        },
        mode,
        &mut ops,
    );
    if let Some(profiles) = table_at(root, "profiles") {
        for (name, profile) in profiles {
            let Some(profile) = profile.as_table() else {
                continue;
            };
            plan_scope(
                &Scope {
                    name: Some(name.clone()),
                    prefix: vec!["profiles".to_string(), name.clone()],
                    table: profile,
                    base: Some(root),
                },
                mode,
                &mut ops,
            );
        }
    }
    ops
}

fn plan_scope(scope: &Scope<'_>, mode: Mode, ops: &mut Vec<PlannedOp>) {
    let base_url = root_entry(scope.table, &BASE_URL_KEYS);
    let api_key = root_entry(scope.table, &API_KEY_KEYS);
    if base_url.is_none() && api_key.is_none() {
        return;
    }
    let name = scope.name.clone();

    let base_url_str = base_url.and_then(|(_, value)| nonempty(value.as_str()));
    let literal_custom = scope.selects_literal_custom() && !scope.has_literal_custom_table();

    // Provider guesses that `api_provider()` used to make from the URL.
    if scope.provider().is_none()
        && let Some(url) = base_url_str
    {
        let lower = url.to_ascii_lowercase();
        let guess = if lower.contains("integrate.api.nvidia.com") {
            Some("nvidia-nim")
        } else if lower.contains("api.deepseeki.com") {
            Some("deepseek-cn")
        } else {
            None
        };
        if let Some(provider) = guess {
            ops.push(PlannedOp {
                op: Op::Set(
                    scope.path(&["provider"]),
                    toml::Value::String(provider.to_string()),
                ),
                note: Some(LegacyRootNote::GuessedProvider {
                    scope: name.clone(),
                    provider,
                }),
            });
        }
    }

    let url_owner = base_url_str.and_then(legacy_root_owner);
    let base_url_dest = if literal_custom {
        "custom".to_string()
    } else if let Some(explicit) = scope
        .own_provider_kind()
        .filter(|kind| base_url_str.is_some_and(|url| host_belongs_to(*kind, url)))
        .filter(|kind| *kind != ProviderKind::Deepseek && *kind != ProviderKind::Custom)
    {
        explicit.provider().provider_config_key().to_string()
    } else if let Some(owner) = url_owner {
        owner.provider().provider_config_key().to_string()
    } else {
        "deepseek".to_string()
    };

    if let Some((key, value)) = base_url {
        plan_move(
            scope,
            mode,
            key,
            value,
            LegacyRootField::BaseUrl,
            &base_url_dest,
            ops,
        );
    }

    if let Some((key, value)) = api_key {
        let key_dest = if literal_custom {
            "custom".to_string()
        } else if let Some(explicit) = scope.own_provider_kind().filter(|kind| {
            *kind != ProviderKind::Deepseek
                && *kind != ProviderKind::Custom
                && base_url_str.is_some_and(|url| host_belongs_to(*kind, url))
                && nonempty(
                    scope
                        .provider_table(kind.provider().provider_config_key())
                        .and_then(|table| string_at(table, "api_key")),
                )
                .is_none()
        }) {
            explicit.provider().provider_config_key().to_string()
        } else {
            "deepseek".to_string()
        };

        // `[vision_model]` without a key of its own used the top-level key.
        if !is_blank(value)
            && let Some(vision) = table_at(scope.table, "vision_model")
            && vision.get("api_key").is_none()
        {
            ops.push(PlannedOp {
                op: Op::Set(scope.path(&["vision_model", "api_key"]), value.clone()),
                note: Some(LegacyRootNote::CopiedToVision {
                    scope: name.clone(),
                }),
            });
        }

        plan_move(
            scope,
            mode,
            key,
            value,
            LegacyRootField::ApiKey,
            &key_dest,
            ops,
        );
    }

    if literal_custom
        && base_url_str.is_some()
        && let Some(model) = ["default_text_model", "model"]
            .iter()
            .find_map(|key| nonempty(string_at(scope.table, key)))
            .or_else(|| {
                scope.base.and_then(|base| {
                    ["default_text_model", "model"]
                        .iter()
                        .find_map(|key| nonempty(string_at(base, key)))
                })
            })
        && scope
            .provider_table("custom")
            .and_then(|table| table.get("model"))
            .is_none()
    {
        ops.push(PlannedOp {
            op: Op::SetIfAbsent(
                scope.path(&["providers", "custom", "model"]),
                toml::Value::String(model.to_string()),
            ),
            note: Some(LegacyRootNote::CopiedModel {
                scope: name.clone(),
            }),
        });
    }

    // A `[providers.custom]` table is an OpenAI-compatible custom route; the
    // top-level shape it replaces always was one.
    let moved_key = api_key.is_some_and(|(_, value)| !is_blank(value));
    if literal_custom && (base_url_str.is_some() || moved_key) {
        ops.push(PlannedOp {
            op: Op::SetIfAbsent(
                scope.path(&["providers", "custom", "kind"]),
                toml::Value::String("openai-compatible".to_string()),
            ),
            note: None,
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn plan_move(
    scope: &Scope<'_>,
    mode: Mode,
    key: &str,
    value: &toml::Value,
    field: LegacyRootField,
    dest: &str,
    ops: &mut Vec<PlannedOp>,
) {
    let name = scope.name.clone();
    let root_path = scope.path(&[key]);
    let dest_path = scope.path(&["providers", dest, field.key()]);
    let dest_label = scope.label(&["providers", dest]);
    if is_blank(value) {
        ops.push(PlannedOp {
            op: Op::Remove(root_path),
            note: Some(LegacyRootNote::DroppedEmpty { scope: name, field }),
        });
        return;
    }
    let existing = scope
        .provider_table(dest)
        .and_then(|table| table.get(field.key()))
        .filter(|existing| !is_blank(existing));
    let Some(existing) = existing else {
        ops.push(PlannedOp {
            op: Op::Move {
                from: root_path,
                to: dest_path,
                value: value.clone(),
            },
            note: Some(LegacyRootNote::Moved {
                scope: name,
                field,
                to: dest_label,
            }),
        });
        return;
    };
    let same = match (existing.as_str(), value.as_str()) {
        (Some(a), Some(b)) => a.trim() == b.trim(),
        _ => existing == value,
    };
    if same {
        ops.push(PlannedOp {
            op: Op::Remove(root_path),
            note: Some(LegacyRootNote::Merged {
                scope: name,
                field,
                to: dest_label,
            }),
        });
        return;
    }
    // A real conflict. Memory applies the precedence the runtime always had;
    // disk changes only on an explicit `--prefer`.
    let top_level_wins = match mode {
        Mode::Memory => Some(field == LegacyRootField::ApiKey),
        Mode::Document(Some(LegacyRootPrefer::TopLevel)) => Some(true),
        Mode::Document(Some(LegacyRootPrefer::Table)) => Some(false),
        Mode::Document(None) => None,
    };
    let op = match top_level_wins {
        Some(true) => Op::Move {
            from: root_path,
            to: dest_path,
            value: value.clone(),
        },
        Some(false) => Op::Remove(root_path),
        None => Op::KeepConflict {
            root: root_path,
            table: dest_path,
        },
    };
    ops.push(PlannedOp {
        op,
        note: Some(LegacyRootNote::Conflict {
            scope: name,
            field,
            table: dest_label,
            resolved: matches!(mode, Mode::Document(Some(_))),
        }),
    });
}

/// Canonicalize a parsed document in memory. Every parse of `config.toml`
/// (and of profile, managed and imported configs) runs this before
/// deserializing, so no reader ever sees a top-level `base_url` or `api_key`.
pub fn apply_to_table(root: &mut toml::Table) -> LegacyRootMigration {
    apply_planned_to_table(root, plan(root, Mode::Memory))
}

fn apply_planned_to_table(root: &mut toml::Table, planned: Vec<PlannedOp>) -> LegacyRootMigration {
    let mut receipt = LegacyRootMigration::default();
    for PlannedOp { op, note } in planned {
        if apply_op_to_table(root, op)
            && let Some(note) = note
        {
            receipt.notes.push(note);
        }
    }
    receipt
}

/// Canonicalize `contents` as text, for callers that then deserialize it.
///
/// Going through `toml::Value` would turn datetimes in unknown keys into
/// strings, so the edit is made on the document and re-rendered instead.
/// Returns `contents` unchanged (borrowed) when there is nothing to move.
pub fn canonicalize_text(
    contents: &str,
) -> Result<(std::borrow::Cow<'_, str>, LegacyRootMigration), toml::de::Error> {
    let might_have_keys = BASE_URL_KEYS
        .iter()
        .chain(API_KEY_KEYS.iter())
        .any(|key| contents.contains(key));
    if !might_have_keys {
        return Ok((
            std::borrow::Cow::Borrowed(contents),
            LegacyRootMigration::default(),
        ));
    }
    let table = toml::from_str::<toml::Table>(contents)?;
    if !has_legacy_root_keys(&table) {
        return Ok((
            std::borrow::Cow::Borrowed(contents),
            LegacyRootMigration::default(),
        ));
    }
    let planned = plan(&table, Mode::Memory);
    match contents.parse::<toml_edit::DocumentMut>() {
        Ok(mut doc) => {
            let receipt = apply_planned_to_document(&mut doc, planned);
            Ok((std::borrow::Cow::Owned(doc.to_string()), receipt))
        }
        Err(_) => {
            let mut table = table;
            let receipt = apply_planned_to_table(&mut table, planned);
            let text = toml::to_string(&table).unwrap_or_else(|_| contents.to_string());
            Ok((std::borrow::Cow::Owned(text), receipt))
        }
    }
}

/// What [`apply_to_document`] would do, without doing it.
#[must_use]
pub fn preview_document(
    doc: &toml_edit::DocumentMut,
    prefer: Option<LegacyRootPrefer>,
) -> LegacyRootMigration {
    let mut cloned = doc.clone();
    apply_to_document(&mut cloned, prefer)
}

/// Canonicalize a document on disk, keeping comments and layout. Conflicting
/// pairs are left untouched unless `prefer` says which one to keep.
pub fn apply_to_document(
    doc: &mut toml_edit::DocumentMut,
    prefer: Option<LegacyRootPrefer>,
) -> LegacyRootMigration {
    let Some(table) = document_table(doc) else {
        return LegacyRootMigration::default();
    };
    apply_planned_to_document(doc, plan(&table, Mode::Document(prefer)))
}

pub fn try_apply_to_document(
    doc: &mut toml_edit::DocumentMut,
    prefer: Option<LegacyRootPrefer>,
) -> Result<LegacyRootMigration> {
    let Some(table) = document_table(doc) else {
        return Ok(LegacyRootMigration::default());
    };
    let mut staged = doc.clone();
    let mut receipt = LegacyRootMigration::default();
    for PlannedOp { op, note } in plan(&table, Mode::Document(prefer)) {
        if apply_op_to_document(&mut staged, op)?
            && let Some(note) = note
        {
            receipt.notes.push(note);
        }
    }
    *doc = staged;
    Ok(receipt)
}

fn apply_planned_to_document(
    doc: &mut toml_edit::DocumentMut,
    planned: Vec<PlannedOp>,
) -> LegacyRootMigration {
    let mut receipt = LegacyRootMigration::default();
    for PlannedOp { op, note } in planned {
        match apply_op_to_document(doc, op) {
            Ok(true) => {
                if let Some(note) = note {
                    receipt.notes.push(note);
                }
            }
            Ok(false) => {}
            Err(err) => {
                tracing::warn!("skipping legacy root migration op: {err:#}");
            }
        }
    }
    receipt
}

fn document_table(doc: &toml_edit::DocumentMut) -> Option<toml::Table> {
    toml::from_str::<toml::Table>(&doc.to_string()).ok()
}

/// `raw` with its legacy top-level keys moved (conflicts left in place), or
/// `None` when there is nothing to move or `raw` does not parse.
#[must_use]
pub fn migrated_document_text(raw: &str) -> Option<String> {
    let mut doc = raw.parse::<toml_edit::DocumentMut>().ok()?;
    apply_to_document(&mut doc, None)
        .changes_file()
        .then(|| doc.to_string())
}

/// Whether a raw document still holds legacy top-level keys anywhere.
#[must_use]
pub fn has_legacy_root_keys(root: &toml::Table) -> bool {
    let scope_has = |table: &toml::Table| {
        BASE_URL_KEYS
            .iter()
            .chain(API_KEY_KEYS.iter())
            .any(|key| table.contains_key(*key))
    };
    scope_has(root)
        || table_at(root, "profiles").is_some_and(|profiles| {
            profiles
                .values()
                .filter_map(toml::Value::as_table)
                .any(scope_has)
        })
}

fn apply_op_to_table(root: &mut toml::Table, op: Op) -> bool {
    match op {
        Op::Set(path, value) => set_in_table(root, &path, value, true),
        Op::SetIfAbsent(path, value) => set_in_table(root, &path, value, false),
        Op::KeepConflict { .. } => true,
        Op::Move { from, to, value } => {
            if set_in_table(root, &to, value, true) {
                apply_op_to_table(root, Op::Remove(from))
            } else {
                false
            }
        }
        Op::Remove(path) => {
            let Some((last, parents)) = path.split_last() else {
                return false;
            };
            let mut current = root;
            for part in parents {
                match current.get_mut(part).and_then(toml::Value::as_table_mut) {
                    Some(next) => current = next,
                    None => return false,
                }
            }
            current.remove(last).is_some()
        }
    }
}

fn set_in_table(
    root: &mut toml::Table,
    path: &[String],
    value: toml::Value,
    overwrite: bool,
) -> bool {
    let Some((last, parents)) = path.split_last() else {
        return false;
    };
    let mut current = root;
    for part in parents {
        let entry = current
            .entry(part.clone())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        match entry.as_table_mut() {
            Some(next) => current = next,
            None => return false,
        }
    }
    if overwrite || !current.contains_key(last) {
        current.insert(last.clone(), value);
        true
    } else {
        false
    }
}

fn apply_op_to_document(doc: &mut toml_edit::DocumentMut, op: Op) -> Result<bool> {
    let (path, value) = match op {
        Op::Move { from, to, value } => {
            let carried = comment_travelling_with(doc, &from);
            let written = document_value(doc, &from)
                .or_else(|| toml_value_to_edit(&value))
                .with_context(|| format!("cannot represent `{}` as a TOML value", to.join(".")))?;
            let segments: Vec<&str> = to.iter().map(String::as_str).collect();
            crate::set_config_document_value(doc, &segments, written).with_context(|| {
                format!("cannot move `{}` to `{}`", from.join("."), to.join("."))
            })?;
            apply_op_to_document(doc, Op::Remove(from))?;
            if let Some(comment) = carried {
                set_key_comment(doc, &to, &comment);
            }
            return Ok(true);
        }
        Op::Set(path, value) => (path, value),
        Op::SetIfAbsent(path, value) if !document_has(doc, &path) => (path, value),
        Op::SetIfAbsent(..) => return Ok(false),
        Op::KeepConflict { .. } => return Ok(true),
        Op::Remove(path) => {
            let segments: Vec<&str> = path.iter().map(String::as_str).collect();
            crate::unset_config_document_value(doc, &segments)
                .with_context(|| format!("cannot remove `{}`", path.join(".")))?;
            return Ok(true);
        }
    };
    let segments: Vec<&str> = path.iter().map(String::as_str).collect();
    let value = toml_value_to_edit(&value)
        .with_context(|| format!("cannot represent `{}` as a TOML value", path.join(".")))?;
    crate::set_config_document_value(doc, &segments, value)
        .with_context(|| format!("cannot set `{}`", path.join(".")))?;
    Ok(true)
}

/// Conflicting pairs `(root path, table path)` a document still holds.
fn conflict_sites(root: &toml::Table) -> Vec<(Vec<String>, Vec<String>)> {
    plan(root, Mode::Document(None))
        .into_iter()
        .filter_map(|planned| match planned.op {
            Op::KeepConflict { root, table } => Some((root, table)),
            _ => None,
        })
        .collect()
}

fn value_at<'a>(root: &'a toml::Table, path: &[String]) -> Option<&'a toml::Value> {
    let (last, parents) = path.split_last()?;
    let mut current = root;
    for part in parents {
        current = current.get(part)?.as_table()?;
    }
    current.get(last)
}

fn set_document_value(doc: &mut toml_edit::DocumentMut, path: &[String], value: &toml::Value) {
    let segments: Vec<&str> = path.iter().map(String::as_str).collect();
    if let Some(value) = toml_value_to_edit(value) {
        let _ = crate::set_config_document_value(doc, &segments, value);
    }
}

/// Put back conflicting pairs that a typed save dropped.
///
/// A typed save writes the in-memory view, where the conflict was already
/// resolved and the top-level key is gone. Unless the user changed that exact
/// value during the session, both keys go back exactly as they were in
/// `original_raw`. Returns whether the document changed.
pub fn restore_conflicts(doc: &mut toml_edit::DocumentMut, original_raw: &str) -> bool {
    let Ok(original) = toml::from_str::<toml::Table>(original_raw) else {
        return false;
    };
    let sites = conflict_sites(&original);
    if sites.is_empty() {
        return false;
    }
    let mut canonical = original.clone();
    apply_to_table(&mut canonical);
    let Some(current) = document_table(doc) else {
        return false;
    };
    let mut changed = false;
    for (root_path, table_path) in sites {
        if value_at(&current, &table_path) != value_at(&canonical, &table_path) {
            // The user wrote this value during the session: their write wins
            // and the top-level key stays gone.
            continue;
        }
        if let (Some(table_value), Some(root_value)) = (
            value_at(&original, &table_path),
            value_at(&original, &root_path),
        ) {
            set_document_value(doc, &table_path, table_value);
            set_document_value(doc, &root_path, root_value);
            changed = true;
        }
    }
    changed
}

/// Table values of the conflicting pairs in `doc`, taken before a targeted
/// write so [`settle_conflicts_after_write`] can tell what the user changed.
pub(crate) type ConflictSnapshot = Vec<(Vec<String>, Vec<String>, Option<toml::Value>)>;

pub(crate) fn conflict_snapshot(doc: &toml_edit::DocumentMut) -> ConflictSnapshot {
    let Some(table) = document_table(doc) else {
        return Vec::new();
    };
    conflict_sites(&table)
        .into_iter()
        .map(|(root, path)| {
            let value = value_at(&table, &path).cloned();
            (root, path, value)
        })
        .collect()
}

/// After a targeted write: when the write changed (or removed) the table side
/// of a conflicting pair, that explicit choice ends the conflict and the
/// top-level key is removed.
pub(crate) fn settle_conflicts_after_write(
    doc: &mut toml_edit::DocumentMut,
    snapshot: ConflictSnapshot,
) {
    if snapshot.is_empty() {
        return;
    }
    let Some(current) = document_table(doc) else {
        return;
    };
    for (root_path, table_path, before) in snapshot {
        if value_at(&current, &table_path) != before.as_ref()
            && let Err(err) = apply_op_to_document(doc, Op::Remove(root_path))
        {
            tracing::warn!("failed to remove settled legacy conflict key: {err:#}");
        }
    }
}

static PENDING_NOTICES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Queue the one-line notice for a write that moved legacy top-level keys.
pub(crate) fn queue_notice(receipt: &LegacyRootMigration, backup: &std::path::Path) {
    let Some(summary) = receipt.summary() else {
        return;
    };
    let line = format!("{summary}; backup at {}", backup.display());
    if let Ok(mut pending) = PENDING_NOTICES.lock()
        && !pending.contains(&line)
    {
        pending.push(line);
    }
}

/// Drain the notices queued by writes that moved legacy top-level keys. Each
/// notice is returned exactly once.
#[must_use]
pub fn take_notices() -> Vec<String> {
    PENDING_NOTICES
        .lock()
        .map(|mut pending| std::mem::take(&mut *pending))
        .unwrap_or_default()
}

fn table_like_at<'a>(
    doc: &'a toml_edit::DocumentMut,
    parents: &[String],
) -> Option<&'a dyn toml_edit::TableLike> {
    let mut current: &dyn toml_edit::TableLike = doc.as_table();
    for part in parents {
        current = current.get(part)?.as_table_like()?;
    }
    Some(current)
}

/// The comment written directly above the key at `path`, when removing the
/// key would otherwise drop it: the next key already has a comment of its
/// own, or there is no next key. (When the next key has none, removal hands
/// the comment to it, which keeps a file header at the top.)
fn comment_travelling_with(doc: &toml_edit::DocumentMut, path: &[String]) -> Option<String> {
    let (key, parents) = path.split_last()?;
    let table = table_like_at(doc, parents)?;
    let prefix = table.key(key)?.leaf_decor().prefix()?.as_str()?.to_string();
    if !prefix.contains('#') {
        return None;
    }
    let mut found = false;
    let next_prefix_empty = table
        .iter()
        .find_map(|(candidate, _)| {
            if found {
                Some(candidate.to_owned())
            } else {
                found = candidate == key.as_str();
                None
            }
        })
        .and_then(|next| {
            table.key(&next).map(|next| {
                next.leaf_decor()
                    .prefix()
                    .and_then(|prefix| prefix.as_str())
                    .is_none_or(str::is_empty)
            })
        })
        .unwrap_or(false);
    (!next_prefix_empty).then(|| prefix.trim_start_matches(['\n', '\r']).to_string())
}

fn set_key_comment(doc: &mut toml_edit::DocumentMut, path: &[String], comment: &str) {
    let Some((key, parents)) = path.split_last() else {
        return;
    };
    let mut current: &mut dyn toml_edit::TableLike = doc.as_table_mut();
    for part in parents {
        match current
            .get_mut(part)
            .and_then(toml_edit::Item::as_table_like_mut)
        {
            Some(next) => current = next,
            None => return,
        }
    }
    if let Some(mut key) = current.key_mut(key) {
        key.leaf_decor_mut().set_prefix(comment);
    }
}

fn document_value(doc: &toml_edit::DocumentMut, path: &[String]) -> Option<toml_edit::Value> {
    let (key, parents) = path.split_last()?;
    let mut value = table_like_at(doc, parents)?.get(key)?.as_value()?.clone();
    // The destination key supplies its own leading spacing.
    value.decor_mut().set_prefix(" ");
    Some(value)
}

fn document_has(doc: &toml_edit::DocumentMut, path: &[String]) -> bool {
    let mut item: &toml_edit::Item = doc.as_item();
    for part in path {
        match item.get(part) {
            Some(next) => item = next,
            None => return false,
        }
    }
    true
}

fn toml_value_to_edit(value: &toml::Value) -> Option<toml_edit::Value> {
    match value {
        toml::Value::String(text) => Some(toml_edit::Value::from(text.as_str())),
        other => other.to_string().parse::<toml_edit::Value>().ok(),
    }
}

#[cfg(test)]
mod tests;
