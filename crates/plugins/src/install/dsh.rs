//! DSH import through the pinned upstream nonexecuting reviewer. All product
//! preview/install callers share this preparation and existing atomic installer.
//! Portable Rust MCP/skills remain core-owned. Closed relative Native modules
//! mount through the one host Loader. Bare exports come only from exact
//! admitted package manifests; mixed missing/unbridged rows refuse explicitly,
//! never a partial compatibility claim. Native install still lands disabled/untrusted and requires review.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};

use anyhow::Result;
use regex::Regex;
use serde::Serialize;
use serde_json::{Value as Json, json};

pub mod presets;

pub const CONVERTER_VERSION: &str = "0.10.1";
const MAX_FILES: usize = 4096;
const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_DOCUMENT: usize = 1024 * 1024;
const MAX_PATCH_FILES: usize = 64;
const MAX_DEPTH: usize = 32;
const MAX_SERVERS: usize = 64;
const DSH_MCP_CLIENT: &str = "@deepseek-ai/dsh-mcp-client";
const DSH_SKILL_FILESYSTEM: &str = "@deepseek-ai/dsh-skill-filesystem";

fn refuse<T>(message: impl Into<String>) -> Result<T> {
    Err(anyhow::Error::msg(message.into()))
}

fn require(condition: bool, message: &str) -> Result<()> {
    if condition { Ok(()) } else { refuse(message) }
}

/// A refusal names at most this many rows and counts the rest.
const MAX_REFUSAL_ROWS: usize = 5;
/// Longest package-authored fragment quoted in a refusal, in characters.
pub const MAX_REFUSAL_TEXT: usize = 120;

/// Package-authored text for a one-line refusal: bounded, with control and
/// bidirectional-format characters shown as escapes. A refusal is plain text
/// (error line, Runtime API), so this is not the review's Markdown escape.
pub fn refusal_text(text: &str) -> String {
    let mut shown = String::new();
    for ch in text.chars().take(MAX_REFUSAL_TEXT) {
        match ch {
            '\'' | '"' => shown.push(ch),
            _ => shown.extend(ch.escape_debug()),
        }
    }
    if text.chars().nth(MAX_REFUSAL_TEXT).is_some() {
        shown.push('…');
    }
    shown
}

/// The first rows of a refusal, then how many more were left out.
pub fn refusal_list(mut rows: impl Iterator<Item = String>) -> String {
    let mut shown: Vec<String> = rows.by_ref().take(MAX_REFUSAL_ROWS).collect();
    let more = rows.count();
    if more > 0 {
        shown.push(format!("and {more} more"));
    }
    shown.join("; ")
}

fn pattern(source: &str) -> Regex {
    Regex::new(source).expect("static pattern")
}

// ─────────────────────────────────────────────────────────────────────────────
// Closed data model
// ─────────────────────────────────────────────────────────────────────────────

/// Parsed configuration data. `Js` is an unevaluated `!!js` scalar; it is
/// never executed. Maps keep source order.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Js(String),
    Seq(Vec<Value>),
    Map(Vec<(String, Value)>),
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Map(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        match self {
            Value::Map(entries) => entries.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn set(&mut self, key: &str, value: Value) {
        if let Value::Map(entries) = self {
            match entries.iter_mut().find(|(k, _)| k == key) {
                Some(slot) => slot.1 = value,
                None => entries.push((key.to_string(), value)),
            }
        }
    }

    fn str(&self) -> Option<&str> {
        match self {
            Value::Str(text) => Some(text),
            _ => None,
        }
    }

    fn keys(&self) -> Vec<&str> {
        match self {
            Value::Map(entries) => entries.iter().map(|(k, _)| k.as_str()).collect(),
            _ => Vec::new(),
        }
    }

    fn is_true(&self) -> bool {
        matches!(self, Value::Bool(true))
    }

    fn free_of_js(&self) -> bool {
        match self {
            Value::Js(_) => false,
            Value::Seq(items) => items.iter().all(Value::free_of_js),
            Value::Map(entries) => entries.iter().all(|(_, v)| v.free_of_js()),
            _ => true,
        }
    }

    fn to_json(&self) -> Json {
        match self {
            Value::Null | Value::Js(_) => Json::Null,
            Value::Bool(value) => Json::Bool(*value),
            Value::Int(value) => json!(value),
            Value::Float(value) => {
                serde_json::Number::from_f64(*value).map_or(Json::Null, Json::Number)
            }
            Value::Str(value) => Json::String(value.clone()),
            Value::Seq(items) => Json::Array(items.iter().map(Value::to_json).collect()),
            Value::Map(entries) => Json::Object(
                entries
                    .iter()
                    .map(|(k, v)| (k.clone(), v.to_json()))
                    .collect(),
            ),
        }
    }
}

/// Keys must be strings, unique, and limited to `allowed` when given.
fn mapping<'a>(value: &'a Value, allowed: Option<&[&str]>) -> Result<&'a [(String, Value)]> {
    let Value::Map(entries) = value else {
        return refuse("Expected a configuration object.");
    };
    if let Some(allowed) = allowed {
        require(
            entries
                .iter()
                .all(|(key, _)| allowed.contains(&key.as_str())),
            "Unsupported fields; select only documented portable declarations.",
        )?;
    }
    Ok(entries)
}

const PARSE_REFUSAL: &str =
    "Cannot parse portable data; use plain YAML or JSON for DSH bundle files.";

/// Closed YAML parsing: no aliases, no explicit tags except `!!js` on
/// scalars (when allowed), no duplicate or non-string keys, at most one
/// document and 32 levels. Parser errors are not echoed: they can contain
/// source lines and credentials.
pub fn parse_yaml(text: &str, allow_js: bool) -> Result<Value> {
    use yaml_rust2::parser::{Event, Parser};
    use yaml_rust2::scanner::TScalarStyle;

    enum Frame {
        Seq(Vec<Value>),
        Map(Vec<(String, Value)>, Option<String>),
    }

    fn place(stack: &mut [Frame], root: &mut Option<Value>, value: Value) -> Result<()> {
        match stack.last_mut() {
            None => {
                *root = Some(value);
                Ok(())
            }
            Some(Frame::Seq(items)) => {
                items.push(value);
                Ok(())
            }
            Some(Frame::Map(entries, pending)) => match pending.take() {
                None => {
                    let Value::Str(key) = value else {
                        return refuse("Object keys must be strings.");
                    };
                    require(
                        !entries.iter().any(|(existing, _)| *existing == key),
                        "Duplicate or non-string object key.",
                    )?;
                    require(
                        key != "__jsExpr",
                        "DSH executable expressions require a manual port.",
                    )?;
                    *pending = Some(key);
                    Ok(())
                }
                Some(key) => {
                    entries.push((key, value));
                    Ok(())
                }
            },
        }
    }

    fn plain_scalar(text: &str) -> Value {
        match text {
            "" | "~" | "null" | "Null" | "NULL" => return Value::Null,
            "true" | "True" | "TRUE" => return Value::Bool(true),
            "false" | "False" | "FALSE" => return Value::Bool(false),
            ".inf" | ".Inf" | ".INF" | "+.inf" | "+.Inf" | "+.INF" => {
                return Value::Float(f64::INFINITY);
            }
            "-.inf" | "-.Inf" | "-.INF" => return Value::Float(f64::NEG_INFINITY),
            ".nan" | ".NaN" | ".NAN" => return Value::Float(f64::NAN),
            _ => {}
        }
        let integer = |digits: &str, radix| i64::from_str_radix(digits, radix).ok();
        if let Some(value) = text
            .strip_prefix("0x")
            .and_then(|digits| integer(digits, 16))
            .or_else(|| {
                text.strip_prefix("0o")
                    .and_then(|digits| integer(digits, 8))
            })
        {
            return Value::Int(value);
        }
        let bytes = text.strip_prefix(['-', '+']).unwrap_or(text);
        if !bytes.is_empty()
            && bytes.bytes().all(|b| b.is_ascii_digit())
            && let Ok(value) = text.parse::<i64>()
        {
            return Value::Int(value);
        }
        let float = pattern(r"^[-+]?(\.[0-9]+|[0-9]+(\.[0-9]*)?)([eE][-+]?[0-9]+)?$");
        if float.is_match(text)
            && let Ok(value) = text.parse::<f64>()
        {
            return Value::Float(value);
        }
        Value::Str(text.to_string())
    }

    let is_js_tag = |tag: &yaml_rust2::parser::Tag| {
        tag.suffix == "js" && (tag.handle == "!!" || tag.handle == "tag:yaml.org,2002:")
    };

    let mut parser = Parser::new_from_str(text);
    let mut stack: Vec<Frame> = Vec::new();
    let mut root: Option<Value> = None;
    let mut documents = 0usize;
    loop {
        let (event, _) = parser
            .next_token()
            .map_err(|_| anyhow::Error::msg(PARSE_REFUSAL))?;
        match event {
            Event::StreamEnd => break,
            Event::StreamStart | Event::Nothing | Event::DocumentEnd => {}
            Event::DocumentStart => {
                documents += 1;
                require(documents <= 1, PARSE_REFUSAL)?;
            }
            Event::Alias(_) => return refuse("YAML aliases are unsupported."),
            Event::Scalar(text, style, _anchor, tag) => {
                let value = match tag {
                    Some(tag) if allow_js && is_js_tag(&tag) => Value::Js(text),
                    Some(_) => {
                        return refuse(
                            "YAML aliases and explicit tags (including !!js) are unsupported.",
                        );
                    }
                    None if matches!(style, TScalarStyle::Plain) => plain_scalar(&text),
                    None => Value::Str(text),
                };
                place(&mut stack, &mut root, value)?;
            }
            Event::SequenceStart(_, tag) | Event::MappingStart(_, tag) if tag.is_some() => {
                return refuse("YAML aliases and explicit tags (including !!js) are unsupported.");
            }
            Event::SequenceStart(..) => {
                require(
                    stack.len() < MAX_DEPTH,
                    "Configuration nesting exceeds 32 levels.",
                )?;
                stack.push(Frame::Seq(Vec::new()));
            }
            Event::MappingStart(..) => {
                require(
                    stack.len() < MAX_DEPTH,
                    "Configuration nesting exceeds 32 levels.",
                )?;
                stack.push(Frame::Map(Vec::new(), None));
            }
            Event::SequenceEnd => {
                let Some(Frame::Seq(items)) = stack.pop() else {
                    return refuse(PARSE_REFUSAL);
                };
                place(&mut stack, &mut root, Value::Seq(items))?;
            }
            Event::MappingEnd => {
                let Some(Frame::Map(entries, None)) = stack.pop() else {
                    return refuse(PARSE_REFUSAL);
                };
                place(&mut stack, &mut root, Value::Map(entries))?;
            }
        }
    }
    Ok(root.unwrap_or(Value::Null))
}

/// Strict JSON (no duplicate keys, finite numbers) through the same closed
/// model as the YAML layers.
pub fn parse_json(text: &str) -> Result<Value> {
    serde_json::from_str::<serde::de::IgnoredAny>(text)
        .map_err(|_| anyhow::Error::msg(PARSE_REFUSAL))?;
    parse_yaml(text, false)
}

// ─────────────────────────────────────────────────────────────────────────────
// Contained reads
// ─────────────────────────────────────────────────────────────────────────────

/// A package directory, canonicalized once; every path below it is walked
/// component by component so a link anywhere inside the package refuses.
pub struct Package {
    root: PathBuf,
}

impl Package {
    pub fn open(path: &Path) -> Result<Self> {
        let root = path.canonicalize().map_err(|_| {
            anyhow::Error::msg(
                "Select a DSH bundle package directory (a directory containing package.json).",
            )
        })?;
        require(
            root.is_dir(),
            "Select a DSH bundle package directory (a directory containing package.json).",
        )?;
        Ok(Self { root })
    }

    /// Join a contained relative path, refusing escapes and links.
    fn contained(&self, relative: &Path) -> Result<PathBuf> {
        let mut current = self.root.clone();
        for component in relative.components() {
            match component {
                Component::CurDir => continue,
                Component::Normal(part) => current.push(part),
                _ => return refuse("Paths must stay inside the selected package."),
            }
            if let Ok(metadata) = fs::symlink_metadata(&current) {
                require(
                    !codewhale_util::path_identity::metadata_is_link_or_reparse(&metadata),
                    "Source paths must not contain links or reparse points.",
                )?;
            }
        }
        Ok(current)
    }

    fn relative_of<'a>(&self, path: &'a Path) -> &'a Path {
        path.strip_prefix(&self.root).unwrap_or(path)
    }
}

/// Read a regular, singly linked file no larger than `limit`.
fn read_file(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path).map_err(|_| {
        anyhow::Error::msg("File operation failed; source files must be accessible regular paths.")
    })?;
    require(
        metadata.is_file()
            && !codewhale_util::path_identity::metadata_is_link_or_reparse(&metadata),
        "Only regular, non-linked source files are supported.",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        require(
            metadata.nlink() == 1,
            "Only regular, non-linked source files are supported.",
        )?;
    }
    require(
        usize::try_from(metadata.len()).is_ok_and(|len| len <= limit),
        "Source file exceeds the conversion size limit.",
    )?;
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path).map_err(|_| {
        anyhow::Error::msg("File operation failed; source files must be accessible regular paths.")
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let opened = file.metadata()?;
        require(
            (opened.dev(), opened.ino()) == (metadata.dev(), metadata.ino()),
            "Source changed during conversion.",
        )?;
    }
    let mut content = Vec::new();
    file.by_ref()
        .take(u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut content)?;
    require(
        content.len() <= limit,
        "Source file exceeds the conversion size limit.",
    )?;
    Ok(content)
}

fn utf8(content: Vec<u8>) -> Result<String> {
    String::from_utf8(content)
        .map_err(|_| anyhow::Error::msg("Configuration and skill entrypoints must be UTF-8."))
}

use codewhale_skill_install::install::sha256_hex;

/// Relative path spellings a package may use for its own files.
fn plain_relative(text: &str) -> bool {
    !text.is_empty()
        // A rooted path without a drive (`/tmp/a.yml`) is not `is_absolute` on
        // Windows, but it is not relative to the bundle either.
        && !text.starts_with('/')
        && !Path::new(text).is_absolute()
        && !Path::new(text)
            .components()
            .any(|c| matches!(c, Component::ParentDir))
        && !text
            .chars()
            .any(|c| c == '\\' || c == ':' || (c as u32) < 0x20)
}

// ─────────────────────────────────────────────────────────────────────────────
// Receipts
// ─────────────────────────────────────────────────────────────────────────────

/// One structured per-row or per-patch outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DshOutcome {
    pub row: Option<String>,
    pub package: Option<String>,
    pub kind: String,
    pub outcome: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch: Option<usize>,
}

impl DshOutcome {
    /// A skipped row or operation is a manual port the reviewer must see.
    pub fn needs_manual_port(&self) -> bool {
        self.outcome == "skipped"
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DshLayer {
    pub path: String,
    pub sha256: String,
    pub bytes: usize,
}

/// What a conversion produced, for preview and install receipts.
#[derive(Debug, Clone, Serialize)]
pub struct DshConversion {
    pub plugin_name: String,
    pub source_package: Option<String>,
    pub source_version: Option<String>,
    pub manifest_sha256: String,
    pub layers: Vec<DshLayer>,
    pub outcomes: Vec<DshOutcome>,
    pub diagnostics: Vec<String>,
    pub skills: Vec<String>,
    pub remote_servers: Vec<String>,
    pub local_servers: Vec<String>,
    pub network_hosts: Vec<String>,
    pub requires_node: bool,
    /// Ordinary reviewed Native entry; remains experimental and disabled at install.
    pub requires_native: bool,
    pub native_rows: Vec<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Bundle loading and patch evaluation
// ─────────────────────────────────────────────────────────────────────────────

pub struct LoadedBundle {
    pub composition: Json,
    pub manifest: Value,
    pub manifest_sha256: String,
    pub entries: Vec<Value>,
    pub notes: Vec<String>,
    pub layers: Vec<DshLayer>,
    pub outcomes: Vec<DshOutcome>,
}

pub fn load_bundle(package: &Package) -> Result<LoadedBundle> {
    let manifest_bytes = read_file(&package.contained(Path::new("package.json"))?, MAX_DOCUMENT)
        .map_err(|_| {
            anyhow::Error::msg("Not a DSH bundle package: package.json is missing or unreadable.")
        })?;
    let manifest_sha256 = sha256_hex(&manifest_bytes);
    let manifest = parse_json(&utf8(manifest_bytes)?)?;
    mapping(&manifest, None)?;
    let bundle = manifest.get("dsh").and_then(|dsh| dsh.get("bundle"));
    require(
        matches!(bundle, Some(Value::Map(_))),
        "Not a DSH bundle package: package.json lacks `dsh.bundle.patch`.",
    )?;
    let mut notes = Vec::new();
    if manifest
        .get("dsh")
        .and_then(|dsh| dsh.get("client"))
        .is_some_and(|client| !matches!(client, Value::Null))
    {
        notes.push(
            "package declares `dsh.client`; its UI half was not converted and stays off until Codewhale has a renderer that can keep plugin UI off the network (WebKit restrictions do not stop WebRTC)"
                .to_string(),
        );
    }
    let declared: Vec<&Value> = match bundle.and_then(|bundle| bundle.get("patch")) {
        Some(single @ Value::Str(_)) => vec![single],
        Some(Value::Seq(items)) if !items.is_empty() => items.iter().collect(),
        _ => {
            return refuse(
                "`dsh.bundle.patch` must name a patch file or a non-empty ordered list of patch files.",
            );
        }
    };
    require(
        declared.len() <= MAX_PATCH_FILES,
        "At most 64 `dsh.bundle.patch` files are supported.",
    )?;
    let mut layers = Vec::new();
    let mut raw_layers = Vec::new();
    let mut seen = Vec::<PathBuf>::new();
    let mut total = 0usize;
    for entry in declared {
        let Some(relative) = entry.str().filter(|text| !text.is_empty()) else {
            return refuse("Every `dsh.bundle.patch` entry must be a non-empty relative path.");
        };
        require(
            plain_relative(relative),
            "Every `dsh.bundle.patch` entry must be a relative path inside the bundle directory.",
        )?;
        let path = package.contained(Path::new(relative))?;
        require(
            path.is_file(),
            "Every `dsh.bundle.patch` entry must resolve to a file inside the bundle directory.",
        )?;
        require(
            !seen.contains(&path),
            "Each `dsh.bundle.patch` file may be listed once; a duplicate would apply its layer twice.",
        )?;
        seen.push(path.clone());
        require(
            total < MAX_DOCUMENT,
            "Selected patch files exceed the 1 MiB aggregate patch limit.",
        )?;
        let content = read_file(&path, MAX_DOCUMENT - total)?;
        total += content.len();
        let sha256 = sha256_hex(&content);
        let bytes = content.len();
        let source = utf8(content)?;
        raw_layers.push(json!({"path": relative_slash(&path, &package.root), "sha256": sha256, "source": source}));
        layers.push(DshLayer {
            path: relative.to_string(),
            sha256,
            bytes,
        });
    }
    let composition = json!({"version":1,"layers":raw_layers,"modules":[],"files":{}});
    let reviewed = crate::composition_review::review(&composition).map_err(anyhow::Error::msg)?;
    let entries = reviewed
        .get("entries")
        .and_then(Json::as_array)
        .ok_or_else(|| anyhow::Error::msg("composition reviewer omitted its effective entries"))?
        .iter()
        .map(reviewed_value)
        .collect::<Result<Vec<_>>>()?;
    // Upstream warnings contain row identities only, never configuration values.
    notes.extend(
        reviewed
            .get("warnings")
            .and_then(Json::as_array)
            .into_iter()
            .flatten()
            .filter_map(Json::as_str)
            .map(str::to_string),
    );
    let outcomes = reviewed
        .get("skipped")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .map(|row| {
            let review_path = row
                .get("layer")
                .and_then(Json::as_str)
                .ok_or_else(|| anyhow::Error::msg("reviewer omitted patch layer"))?;
            let layer = layers
                .iter()
                .find(|layer| layer.path.trim_start_matches("./") == review_path)
                .ok_or_else(|| anyhow::Error::msg("reviewer returned unknown patch layer"))?;
            Ok(DshOutcome {
                row: row.get("row").and_then(Json::as_str).map(str::to_string),
                package: row
                    .get("package")
                    .and_then(Json::as_str)
                    .map(str::to_string),
                kind: "patch".into(),
                outcome: "skipped".into(),
                reason: row
                    .get("reason")
                    .and_then(Json::as_str)
                    .ok_or_else(|| anyhow::Error::msg("reviewer omitted patch reason"))?
                    .into(),
                layer: Some(layer.path.clone()),
                patch: Some(usize::try_from(
                    row.get("patch")
                        .and_then(Json::as_u64)
                        .filter(|n| *n > 0)
                        .ok_or_else(|| anyhow::Error::msg("reviewer omitted patch number"))?,
                )?),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(LoadedBundle {
        composition,
        manifest,
        manifest_sha256,
        entries,
        notes,
        layers,
        outcomes,
    })
}

/// Upstream expression markers remain expressions, including nested values.
fn reviewed_value(value: &Json) -> Result<Value> {
    Ok(match value {
        Json::Null => Value::Null,
        Json::Bool(value) => Value::Bool(*value),
        Json::Number(value) => {
            if let Some(value) = value.as_i64() {
                Value::Int(value)
            } else {
                Value::Float(
                    value
                        .as_f64()
                        .ok_or_else(|| anyhow::Error::msg("nonfinite reviewed value"))?,
                )
            }
        }
        Json::String(value) => Value::Str(value.clone()),
        Json::Array(values) => {
            Value::Seq(values.iter().map(reviewed_value).collect::<Result<_>>()?)
        }
        Json::Object(values)
            if values.len() == 1 && values.get("__jsExpr").is_some_and(Json::is_string) =>
        {
            Value::Js(
                values["__jsExpr"]
                    .as_str()
                    .expect("checked marker")
                    .to_string(),
            )
        }
        Json::Object(values) => Value::Map(
            values
                .iter()
                .map(|(key, value)| Ok((key.clone(), reviewed_value(value)?)))
                .collect::<Result<_>>()?,
        ),
    })
}

/// Rows are addressed by index path: `[i]` is a top-level entry, `[i, j]` the
/// `j`th child in entry `i`'s group `config`.
#[cfg(any(test, feature = "test-support"))]
pub fn row_at<'a>(entries: &'a mut [Value], path: &[usize]) -> &'a mut Value {
    let (first, rest) = path.split_first().expect("non-empty row path");
    let mut row = &mut entries[*first];
    for index in rest {
        let Some(Value::Seq(children)) = row.get_mut("config") else {
            unreachable!("row paths only descend through group configs");
        };
        row = &mut children[*index];
    }
    row
}

/// `applyEntryPatches` parity over an empty entry list: `insert` appends rows
/// (or appends into a group entry's config); keyed patches replace fields on
/// an earlier inserted row. Skipped patches are recorded with their layer.
#[cfg(any(test, feature = "test-support"))]
pub fn evaluate_patches(
    patches: Vec<Value>,
    notes: &mut Vec<String>,
    locations: &[(String, usize)],
) -> Result<(Vec<Value>, Vec<DshOutcome>)> {
    fn index_rows(
        rows: &[Value],
        base: &[usize],
        offset: usize,
        index: &mut BTreeMap<String, Vec<usize>>,
    ) {
        for (position, row) in rows.iter().enumerate() {
            let mut path = base.to_vec();
            path.push(offset + position);
            if let Some(identifier) = row.get("id").and_then(Value::str) {
                index.insert(identifier.to_string(), path.clone());
            }
            if (row.get("group").is_some_and(Value::is_true)
                || row.get("name").and_then(Value::str).is_some_and(|name| {
                    matches!(name, "cordis:group" | "@deepseek-ai/cordis-plugin-group")
                }))
                && let Some(Value::Seq(children)) = row.get("config")
            {
                index_rows(children, &path, 0, index);
            }
        }
    }

    let mut entries: Vec<Value> = Vec::new();
    let mut index: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut outcomes = Vec::new();
    for (order, patch) in patches.into_iter().enumerate() {
        require(
            matches!(patch, Value::Map(_)),
            "Each DSH patch must be an object.",
        )?;
        let mut skip = |reason: String| {
            notes.push(format!("patch {}: {reason}; skipped", order + 1));
            let (layer, number) = locations[order].clone();
            outcomes.push(DshOutcome {
                row: patch.get("id").and_then(Value::str).map(str::to_string),
                package: patch.get("name").and_then(Value::str).map(str::to_string),
                kind: "patch".to_string(),
                outcome: "skipped".to_string(),
                reason,
                layer: Some(layer),
                patch: Some(number),
            });
        };
        let identifier = patch.get("id").cloned();
        if let Some(insert) = patch.get("insert") {
            let Value::Seq(rows) = insert else {
                return refuse("A DSH patch `insert` must be a list of entries.");
            };
            require(
                rows.iter().all(|row| matches!(row, Value::Map(_))),
                "A DSH patch `insert` must be a list of entries.",
            )?;
            let rows = rows.clone();
            match identifier {
                None | Some(Value::Null) => {
                    let offset = entries.len();
                    entries.extend(rows.iter().cloned());
                    index_rows(&rows, &[], offset, &mut index);
                }
                Some(identifier) => {
                    let target = identifier.str().and_then(|id| index.get(id)).cloned();
                    let Some(path) = target.filter(|path| {
                        row_at(&mut entries, path)
                            .get("group")
                            .is_some_and(Value::is_true)
                    }) else {
                        let shown = identifier.str().unwrap_or("?").to_string();
                        skip(format!("insert target `{shown}` is missing or not a group"));
                        continue;
                    };
                    let row = row_at(&mut entries, &path);
                    if !matches!(row.get("config"), Some(Value::Seq(_))) {
                        row.set("config", Value::Seq(Vec::new()));
                    }
                    let Some(Value::Seq(children)) = row.get_mut("config") else {
                        unreachable!("config was just set to a list");
                    };
                    let offset = children.len();
                    children.extend(rows.iter().cloned());
                    index_rows(&rows, &path, offset, &mut index);
                }
            }
            continue;
        }
        let Some(identifier) = identifier.as_ref().and_then(Value::str).map(str::to_string) else {
            skip("non-insert patch without an `id`".to_string());
            continue;
        };
        let Some(path) = index.get(&identifier).cloned() else {
            skip(format!(
                "entry `{identifier}` was not inserted by an earlier layer"
            ));
            continue;
        };
        let target = row_at(&mut entries, &path);
        if let Some(name) = patch.get("name")
            && !matches!(name, Value::Null)
            && Some(name) != target.get("name")
        {
            skip(format!("`name` does not match entry `{identifier}`"));
            continue;
        }
        let Value::Map(fields) = &patch else {
            unreachable!()
        };
        let replaces_children = fields
            .iter()
            .any(|(key, _)| key == "config" || key == "group");
        for (key, value) in fields {
            if key != "id" && key != "name" {
                target.set(key, value.clone());
            }
        }
        if replaces_children {
            // Rows under a replaced config are detached from the profile, as
            // their dict identities are in DSH; later patches cannot reach them.
            index.retain(|_, row_path| {
                !(row_path.len() > path.len() && row_path.starts_with(&path))
            });
        }
    }
    Ok((entries, outcomes))
}

// ─────────────────────────────────────────────────────────────────────────────
// MCP conversion
// ─────────────────────────────────────────────────────────────────────────────

fn js_literal(text: &str, label: &str) -> Result<String> {
    let text = text.trim();
    let chars: Vec<char> = text.chars().collect();
    if chars.len() >= 2
        && (chars[0] == '"' || chars[0] == '\'')
        && chars[chars.len() - 1] == chars[0]
        && !chars[1..chars.len() - 1].contains(&chars[0])
    {
        let body: String = chars[1..chars.len() - 1].iter().collect();
        require(
            !body.contains('\\') && !body.contains('\n'),
            &format!("{label} contains JavaScript escapes; author the literal explicitly."),
        )?;
        return Ok(body);
    }
    if chars.len() >= 2 && chars[0] == '`' && chars[chars.len() - 1] == '`' {
        let body: String = chars[1..chars.len() - 1].iter().collect();
        require(
            !body.contains("${") && !body.contains('\\') && !body.contains('`'),
            &format!(
                "{label} interpolates a value that conversion never evaluates; author the literal explicitly."
            ),
        )?;
        return Ok(body);
    }
    refuse(format!(
        "{label} is not a quoted or template literal; author the value explicitly."
    ))
}

/// Lower only the `!!js` idioms that need no ambient state.
pub fn lower_js(text: &str, label: &str) -> Result<String> {
    let text = text.trim();
    if text == "process.execPath" {
        return Ok("node".to_string());
    }
    if text.contains("process.env") {
        return refuse(format!(
            "{label} reads an environment value, and conversion never evaluates this machine's environment; author the literal explicitly"
        ));
    }
    if text.starts_with('`') || text.starts_with('"') || text.starts_with('\'') {
        return js_literal(text, label);
    }
    refuse(format!(
        "{label} uses a `!!js` expression with no portable lowering; author the value explicitly."
    ))
}

fn timeout_seconds(value: &Value) -> Result<i64> {
    match value {
        Value::Int(ms) if (1000..=3_600_000).contains(ms) && ms % 1000 == 0 => Ok(ms / 1000),
        _ => refuse(
            "Timeouts must be whole seconds expressed in milliseconds (1000–3600000); port other values manually.",
        ),
    }
}

fn server_options(config: &Value) -> Result<serde_json::Map<String, Json>> {
    let mut extension = serde_json::Map::new();
    require(
        config
            .get("failOnStartupError")
            .is_none_or(|v| *v == Value::Bool(false)),
        "DSH startup-failure policy requires a manual port.",
    )?;
    if let Some(value) = config.get("toolCallTimeoutMs") {
        extension.insert(
            "execute_timeout".to_string(),
            json!(timeout_seconds(value)?),
        );
    }
    Ok(extension)
}

/// Parse a literal HTTP(S) endpoint the way the native reviewer will see it,
/// returning the host as it enters the capability set.
pub fn endpoint_host(url: &str) -> Result<String> {
    require(
        !url.chars()
            .any(|c| c.is_whitespace() || c == '\\' || c == '{' || c == '}'),
        "MCP URL must be a literal endpoint without interpolation.",
    )?;
    require(
        url.is_ascii() && url.len() <= 4096,
        "Use an ASCII MCP hostname and URL of at most 4096 characters.",
    )?;
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| anyhow::Error::msg("Invalid MCP endpoint URL."))?;
    let scheme = scheme.to_ascii_lowercase();
    require(
        !rest.contains('?') && !rest.contains('#'),
        "MCP URLs must not contain credentials, query strings or fragments.",
    )?;
    let authority = rest.split('/').next().unwrap_or_default();
    require(
        !authority.contains('@'),
        "MCP URLs must not contain credentials, query strings or fragments.",
    )?;
    let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
        let (host, tail) = bracketed
            .split_once(']')
            .ok_or_else(|| anyhow::Error::msg("MCP URL needs a valid host and port."))?;
        (host.to_string(), tail.strip_prefix(':'))
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => (host.to_string(), Some(port)),
            None => (authority.to_string(), None),
        }
    };
    let host = host.to_ascii_lowercase();
    require(!host.is_empty(), "MCP URL needs a valid host and port.")?;
    if let Some(port) = port {
        require(
            port.parse::<u16>().is_ok_and(|port| port != 0),
            "MCP URL needs a valid host and port.",
        )?;
    }
    let loopback = matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1");
    require(
        scheme == "https" || (scheme == "http" && loopback),
        "MCP endpoints need HTTPS (or explicit loopback HTTP).",
    )?;
    let last_label = host.rsplit('.').next().unwrap_or_default();
    let numeric = host.contains(':') || pattern(r"^(?:[0-9]+|0x[0-9a-f]+)$").is_match(last_label);
    if numeric {
        let address: std::net::IpAddr = host
            .parse()
            .map_err(|_| anyhow::Error::msg("Use a canonical numeric MCP address."))?;
        require(
            address.to_string() == host,
            "Use a canonical numeric MCP address.",
        )?;
        if address.is_ipv6() {
            return Ok(format!("[{host}]"));
        }
    }
    Ok(host)
}

fn remote_server(config: &Value) -> Result<(Json, String)> {
    mapping(config, None)?;
    require(
        config.get("transport").and_then(Value::str) == Some("streamable-http"),
        "Unsupported MCP transport.",
    )?;
    mapping(
        config,
        Some(&[
            "serverName",
            "transport",
            "url",
            "headers",
            "toolCallTimeoutMs",
            "failOnStartupError",
        ]),
    )?;
    let extension = server_options(config)?;
    let Some(url) = config.get("url").and_then(Value::str) else {
        return refuse("MCP URL must be a literal endpoint without interpolation.");
    };
    let host = endpoint_host(url)?;
    if let Some(headers) = config.get("headers") {
        require(
            mapping(headers, None)?.is_empty(),
            "Literal headers, DSH expressions and file interpolation cannot be converted; author native env_headers manually.",
        )?;
    }
    Ok((
        json!({"type": "streamable-http", "url": url, "extensions": {"net.codewhale": extension}}),
        host,
    ))
}

fn stdio_server(config: &Value, name: &str, root: &Path) -> Result<Json> {
    mapping(
        config,
        Some(&[
            "serverName",
            "transport",
            "command",
            "args",
            "env",
            "cwd",
            "toolCallTimeoutMs",
            "failOnStartupError",
        ]),
    )?;
    if let Some(env) = config.get("env") {
        require(
            mapping(env, None)?.is_empty(),
            "DSH stdio env values and expressions require a manual native port.",
        )?;
    }
    let entry = match (
        config.get("command").and_then(Value::str),
        config.get("args"),
    ) {
        (Some("node"), Some(Value::Seq(args))) if args.len() == 1 => args[0].str(),
        _ => None,
    };
    let Some(entry) = entry else {
        return refuse(
            "Only node with one packaged .mjs, .js or .cjs entry is supported; no launcher flags, package managers or shell commands.",
        );
    };
    require(
        pattern(r"^(?:\./)?[A-Za-z0-9_][A-Za-z0-9_./-]*\.(?:mjs|js|cjs)$").is_match(entry)
            && !entry.split('/').any(|part| part == ".."),
        "Node entry must be a contained relative .mjs, .js or .cjs file; compile other entry formats before packaging.",
    )?;
    require(
        config
            .get("cwd")
            .is_none_or(|cwd| matches!(cwd.str(), Some("" | "."))),
        "Other cwd values require a manual port.",
    )?;
    require(
        root.join(entry).is_file(),
        "Packaged Node entry does not exist.",
    )?;
    Ok(json!({
        "type": "stdio",
        "command": "node",
        "args": [entry],
        "cwd": format!("mcp/{name}"),
        "env": {},
        "extensions": {"net.codewhale": server_options(config)?},
    }))
}

// ─────────────────────────────────────────────────────────────────────────────
// Row conversion
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct Components {
    pub servers: Vec<(String, Json)>,
    pub hosts: Vec<String>,
    /// Local server name → packaged source root inside the package.
    pub roots: Vec<(String, PathBuf)>,
    pub skill_dirs: Vec<PathBuf>,
    pub notes: Vec<String>,
    pub outcomes: Vec<DshOutcome>,
}

impl Components {
    fn record(&mut self, row: &Value, kind: &str, outcome: &str, reason: &str) {
        let identifier = row.get("id").and_then(Value::str).map(str::to_string);
        let label = identifier
            .as_ref()
            .map_or_else(|| "an unlabeled row".to_string(), |id| format!("`{id}`"));
        self.notes.push(format!(
            "{label} [{kind}] {outcome}{}",
            if reason.is_empty() {
                String::new()
            } else {
                format!(": {reason}")
            }
        ));
        self.outcomes.push(DshOutcome {
            row: identifier,
            package: row.get("name").and_then(Value::str).map(str::to_string),
            kind: kind.to_string(),
            outcome: outcome.to_string(),
            reason: reason.to_string(),
            layer: None,
            patch: None,
        });
    }

    fn has_server(&self, name: &str) -> bool {
        self.servers.iter().any(|(existing, _)| existing == name)
    }

    fn mcp_row(&mut self, package: &Package, row: &Value, disabled: bool) -> Result<()> {
        let mut config = row.get("config").cloned().unwrap_or(Value::Null);
        mapping(&config, None)?;
        let name = config
            .get("serverName")
            .and_then(Value::str)
            .map(str::to_string);
        let Some(name) =
            name.filter(|name| pattern(r"^[A-Za-z0-9][A-Za-z0-9_-]{0,31}$").is_match(name))
        else {
            return refuse(
                "dsh-mcp-client config needs a literal `serverName` of 1–32 letters/digits/_/-",
            );
        };
        require(
            !self.has_server(&name),
            &format!("Duplicate MCP server name `{name}`; nothing was written for it."),
        )?;
        for field in ["command", "cwd", "url", "serverName"] {
            if let Some(Value::Js(expression)) = config.get(field) {
                let lowered = lower_js(expression, &format!("`{field}` in `{name}`"))?;
                config.set(field, Value::Str(lowered));
            }
        }
        if let Some(Value::Seq(args)) = config.get("args").cloned() {
            let lowered = args
                .into_iter()
                .map(|arg| match arg {
                    Value::Js(expression) => {
                        lower_js(&expression, &format!("`args` in `{name}`")).map(Value::Str)
                    }
                    other => Ok(other),
                })
                .collect::<Result<Vec<_>>>()?;
            config.set("args", Value::Seq(lowered));
        }
        let local = config.get("transport").and_then(Value::str) == Some("stdio");
        let mut root = None;
        if local
            && let Some(Value::Seq(args)) = config.get("args")
            && let [Value::Str(arg)] = args.as_slice()
        {
            let entry = pattern(r"^(?:\./)?[A-Za-z0-9_][A-Za-z0-9_./-]*\.(?:mjs|js|cjs)$");
            if !entry.is_match(arg) {
                return refuse(format!(
                    "`args` in `{name}` names a host path outside the selected package; import never copies an ambient path. Package the server inside the bundle, then import again"
                ));
            }
            if !arg.split('/').any(|part| part == "..") {
                let cwd = config.get("cwd").and_then(Value::str).unwrap_or("");
                if matches!(cwd, "" | ".") {
                    if package.contained(Path::new(arg))?.is_file() {
                        root = Some(package.root.clone());
                    }
                } else if plain_relative(cwd) {
                    let candidate = package.contained(Path::new(cwd))?;
                    if package.contained(&Path::new(cwd).join(arg))?.is_file() {
                        root = Some(candidate);
                        config.set("cwd", Value::Str(".".to_string()));
                    }
                }
            }
        }
        require(
            config.free_of_js(),
            &format!("an unevaluated `!!js` remains in `{name}`; author that field explicitly"),
        )?;
        let mut converted = if local {
            let Some(root) = root else {
                return refuse(format!(
                    "`{name}` is a local server whose packaged Node entry is not inside the selected package"
                ));
            };
            let server = stdio_server(&config, &name, &root)?;
            let shown = package.relative_of(&root).display().to_string();
            self.notes.push(format!(
                "`{name}`: stdio source root resolved inside the selected package at `{}`",
                if shown.is_empty() {
                    ".".to_string()
                } else {
                    shown
                }
            ));
            self.roots.push((name.clone(), root));
            server
        } else {
            let (server, host) = remote_server(&config)?;
            self.hosts.push(host);
            server
        };
        if disabled {
            converted["extensions"]["net.codewhale"]["disabled"] = json!(true);
        }
        self.servers.push((name, converted));
        Ok(())
    }

    pub fn walk(
        &mut self,
        package: &Package,
        rows: &[Value],
        disabled_ancestor: Option<&str>,
    ) -> Result<()> {
        for row in rows {
            require(
                matches!(row, Value::Map(_)),
                "Each DSH group child must be an entry object.",
            )?;
            let identifier = row.get("id").and_then(Value::str);
            let label =
                identifier.map_or_else(|| "an unlabeled row".to_string(), |id| format!("`{id}`"));
            let name = row.get("name").and_then(Value::str);
            let group = row.get("group").is_some_and(Value::is_true);
            let convertible = matches!(name, Some(DSH_MCP_CLIENT | DSH_SKILL_FILESYSTEM));
            if group || convertible {
                require(
                    row.keys()
                        .iter()
                        .all(|key| matches!(*key, "id" | "name" | "config" | "group" | "disabled")),
                    &format!(
                        "{label} has unsupported entry policy or dependency fields; port its activation and authority semantics manually before conversion."
                    ),
                )?;
                require(
                    row.get("group")
                        .is_none_or(|flag| matches!(flag, Value::Bool(_))),
                    &format!("{label} has a non-boolean group flag; resolve it explicitly."),
                )?;
            }
            if group {
                require(
                    matches!(row.get("config"), Some(Value::Seq(_))),
                    &format!("{label} needs a list of group entries."),
                )?;
            }
            let disabled = match row.get("disabled") {
                None => false,
                Some(Value::Bool(flag)) => *flag,
                Some(_) => {
                    if group || convertible {
                        return refuse(format!(
                            "{label} gates activation with a conditional or non-boolean `disabled` value that cannot be resolved offline; import refuses to assume the row is enabled. Pre-resolve the gate in a reviewed copy of the patch layer, then import that copy."
                        ));
                    }
                    self.record(
                        row,
                        "foreign",
                        "skipped",
                        "no portable representation; its conditional `disabled` gate was not evaluated",
                    );
                    continue;
                }
            };
            if group {
                let Some(Value::Seq(children)) = row.get("config") else {
                    unreachable!()
                };
                let inherited = match disabled_ancestor {
                    Some(ancestor) => Some(ancestor.to_string()),
                    None if disabled => Some(label.clone()),
                    None => None,
                };
                self.walk(package, children, inherited.as_deref())?;
                continue;
            }
            let reason = if disabled {
                "the source row sets `disabled: true`".to_string()
            } else if let Some(ancestor) = disabled_ancestor {
                format!("the row is disabled by ancestor {ancestor}")
            } else {
                String::new()
            };
            let effective = disabled || disabled_ancestor.is_some();
            match name {
                Some(DSH_MCP_CLIENT) => {
                    if let Some(server) = row
                        .get("config")
                        .and_then(|c| c.get("serverName"))
                        .and_then(Value::str)
                    {
                        require(
                            !self.has_server(server),
                            "Duplicate MCP server name; no entries were written.",
                        )?;
                    }
                    match self.mcp_row(package, row, effective) {
                        Ok(()) => self.record(
                            row,
                            "mcp",
                            if effective {
                                "converted-disabled"
                            } else {
                                "converted"
                            },
                            &reason,
                        ),
                        Err(error) => self.record(row, "mcp", "skipped", &error.to_string()),
                    }
                }
                Some(DSH_SKILL_FILESYSTEM) => {
                    if effective {
                        self.record(
                            row,
                            "skill",
                            "skipped-disabled",
                            &format!(
                                "{reason}; native skills have no disabled state, so the intent is preserved by omission"
                            ),
                        );
                        continue;
                    }
                    let dirs = match row.get("config").and_then(|c| c.get("customSkillDirs")) {
                        Some(Value::Seq(dirs)) if !dirs.is_empty() => dirs.clone(),
                        _ => {
                            self.record(
                                row,
                                "skill",
                                "skipped",
                                "skill row has no `customSkillDirs` to import",
                            );
                            continue;
                        }
                    };
                    let mut imported = 0;
                    for entry in dirs {
                        let Value::Str(entry) = entry else {
                            self.notes.push(format!(
                                "{label}: a `customSkillDirs` entry is not a literal path; skipped"
                            ));
                            continue;
                        };
                        if !plain_relative(&entry) {
                            self.notes.push(format!(
                                "{label}: `customSkillDirs` entry `{entry}` is outside the bundle; skipped"
                            ));
                            continue;
                        }
                        let resolved = package.contained(Path::new(&entry))?;
                        if !resolved.is_dir() {
                            self.notes.push(format!(
                                "{label}: `customSkillDirs` entry `{entry}` does not exist in the bundle; skipped"
                            ));
                            continue;
                        }
                        self.skill_dirs.push(resolved);
                        imported += 1;
                    }
                    if imported > 0 {
                        self.record(
                            row,
                            "skill",
                            "converted",
                            &format!(
                                "imported {imported} `customSkillDirs` entries inside the package"
                            ),
                        );
                    } else {
                        self.record(
                            row,
                            "skill",
                            "skipped",
                            "no `customSkillDirs` entry inside the package could be imported",
                        );
                    }
                }
                _ => {
                    let shown = name.unwrap_or("unlabeled");
                    self.record(
                        row,
                        "foreign",
                        "skipped",
                        &format!(
                            "only dsh-mcp-client and dsh-skill-filesystem rows convert; `{shown}` has no portable representation"
                        ),
                    );
                }
            }
        }
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Files
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Default)]
struct OutputFiles {
    files: BTreeMap<String, Vec<u8>>,
    bytes: usize,
}

impl OutputFiles {
    fn add(&mut self, relative: String, content: Vec<u8>) -> Result<()> {
        require(
            !self.files.contains_key(&relative),
            "Two converted files collide; nothing was written.",
        )?;
        require(
            self.files.len() < MAX_FILES,
            "Selected components exceed the file budget.",
        )?;
        self.bytes += content.len();
        require(
            self.bytes <= MAX_BYTES,
            "Output exceeds the 4096-file / 64 MiB bundle budget.",
        )?;
        self.files.insert(relative, content);
        Ok(())
    }

    fn remaining(&self) -> usize {
        MAX_BYTES.saturating_sub(self.bytes)
    }
}

/// Walk a contained directory without following links, bounded.
pub fn walk_files(root: &Path, mut visit: impl FnMut(&Path, bool) -> Result<()>) -> Result<()> {
    let mut pending = vec![root.to_path_buf()];
    let mut visited = 0usize;
    while let Some(directory) = pending.pop() {
        let mut children: Vec<_> = fs::read_dir(&directory)?.collect::<std::io::Result<_>>()?;
        children.sort_by_key(fs::DirEntry::file_name);
        for child in children {
            visited += 1;
            require(
                visited <= MAX_FILES,
                "The selected source contains too many filesystem entries.",
            )?;
            let path = child.path();
            let metadata = fs::symlink_metadata(&path)?;
            require(
                !codewhale_util::path_identity::metadata_is_link_or_reparse(&metadata),
                "Source paths must not contain links or reparse points.",
            )?;
            visit(&path, metadata.is_dir())?;
            if metadata.is_dir() {
                pending.push(path);
            }
        }
    }
    Ok(())
}

fn relative_slash(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Convert one skill directory or Markdown file into native skill files.
fn skill_files(source: &Path, output: &mut OutputFiles) -> Result<String> {
    let is_dir = source.is_dir();
    let entry = if is_dir {
        source.join("SKILL.md")
    } else {
        source.to_path_buf()
    };
    require(
        entry.file_name().is_some_and(|name| name == "SKILL.md")
            || entry.extension().is_some_and(|ext| ext == "md"),
        "Select a skill directory or Markdown skill file.",
    )?;
    let text = utf8(read_file(&entry, MAX_DOCUMENT)?)?;
    let delimiter = pattern(r"(?m)^---\s*$");
    let parts: Vec<&str> = delimiter.splitn(&text, 3).collect();
    require(
        parts.len() == 3 && parts[0].trim().is_empty(),
        "Skills need YAML frontmatter with name and description.",
    )?;
    let meta = parse_yaml(parts[1], false)?;
    mapping(
        &meta,
        Some(&[
            "name",
            "description",
            "license",
            "compatibility",
            "metadata",
            "disable-model-invocation",
            "user-invocable",
        ]),
    )?;
    let Some(name) = meta
        .get("name")
        .and_then(Value::str)
        .filter(|name| name.len() <= 64 && pattern(r"^[a-z0-9]+(?:-[a-z0-9]+)*$").is_match(name))
    else {
        return refuse("Skill name must be a kebab-case identifier of at most 64 characters.");
    };
    let Some(description) = meta
        .get("description")
        .and_then(Value::str)
        .filter(|d| !d.trim().is_empty())
    else {
        return refuse("Skills need a non-empty description.");
    };
    require(
        !description.contains("---"),
        "Skill description contains a delimiter the native reader cannot preserve.",
    )?;
    require(
        meta.get("user-invocable")
            .is_none_or(|v| *v == Value::Bool(true)),
        "user-invocable:false has no equivalent in the native skill adapter; port it manually.",
    )?;
    let explicit = match meta.get("disable-model-invocation") {
        None => false,
        Some(Value::Bool(flag)) => *flag,
        Some(_) => return refuse("disable-model-invocation must be a boolean."),
    };
    let mut front = format!("---\nname: {name}\ndescription: |-\n");
    front.push_str(
        &description
            .lines()
            .map(|line| format!("  {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    front.push('\n');
    if explicit {
        front.push_str("invocation: explicit-only\n");
    }
    output.add(
        format!("skills/{name}/SKILL.md"),
        format!("{front}---{}", parts[2]).into_bytes(),
    )?;
    let extra: serde_json::Map<String, Json> = ["license", "compatibility", "metadata"]
        .into_iter()
        .filter_map(|key| {
            meta.get(key)
                .map(|value| (key.to_string(), value.to_json()))
        })
        .collect();
    if !extra.is_empty() {
        output.add(
            format!("skills/{name}/SOURCE_SKILL_METADATA.json"),
            format!("{}\n", serde_json::to_string_pretty(&extra)?).into_bytes(),
        )?;
    }
    if is_dir {
        walk_files(source, |path, is_dir| {
            let file_name = path.file_name().unwrap_or_default().to_string_lossy();
            require(
                !matches!(file_name.as_ref(), ".git" | ".env" | ".installed-from"),
                "Remove local secrets, repository metadata or install receipts from the selected skill.",
            )?;
            if is_dir || path == entry {
                return Ok(());
            }
            let content = read_file(path, output.remaining())?;
            output.add(
                format!("skills/{name}/{}", relative_slash(path, source)),
                content,
            )
        })?;
    }
    Ok(name.to_string())
}

/// Copy a packaged MCP source directory as data; never resolve dependencies.
fn stdio_files(root: &Path, name: &str, output: &mut OutputFiles) -> Result<()> {
    walk_files(root, |path, is_dir| {
        let lower = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase();
        let suffix = path
            .extension()
            .map(|ext| ext.to_string_lossy().to_ascii_lowercase());
        require(
            !lower.starts_with('.')
                && !matches!(
                    lower.as_str(),
                    "credentials" | "credentials.json" | "secrets.json" | "id_rsa" | "id_ed25519"
                )
                && !matches!(suffix.as_deref(), Some("pem" | "key" | "p12" | "pfx")),
            "Package MCP source without hidden files, repository metadata or credential files; nothing was copied.",
        )?;
        if is_dir {
            return Ok(());
        }
        let content = read_file(path, output.remaining())?;
        output.add(
            format!("mcp/{name}/{}", relative_slash(path, root)),
            content,
        )
    })
}

/// A native plugin name from the package name: its last path segment,
/// lowercased, with other characters replaced by single hyphens.
fn host_hook_bridge(name: &str) -> bool {
    crate::activation::dsh_row_bridge(name)
}

/// Closed relative modules and exact contained ESM package exports. Missing
/// foreign rows remain manual ports for a portable-only import; a live Native
/// graph refuses unresolved rows and never uses an ambient Node parent-walk.
fn native_composition_files(
    package: &Package,
    entries: &[Value],
    mut document: Json,
    output: &mut OutputFiles,
    client_outcomes: &mut Vec<DshOutcome>,
) -> Result<Vec<String>> {
    fn modules(rows: &[Value], names: &mut Vec<String>) -> Result<()> {
        for row in rows {
            if row.get("group").is_some_and(Value::is_true)
                || row.get("name").and_then(Value::str).is_some_and(|name| {
                    matches!(name, "cordis:group" | "@deepseek-ai/cordis-plugin-group")
                })
            {
                if let Some(Value::Seq(children)) = row.get("config") {
                    modules(children, names)?;
                }
                continue;
            }
            let name = row
                .get("name")
                .and_then(Value::str)
                .ok_or_else(|| anyhow::Error::msg("reviewed row has no module name"))?;
            if !names.iter().any(|old| old == name) {
                names.push(name.into());
            }
        }
        Ok(())
    }
    let mut names = Vec::new();
    modules(entries, &mut names)?;
    if names.is_empty() {
        return Ok(names);
    }
    // Portable-only import keeps absent foreign rows visible as manual ports.
    // Once one real Native closure is present, every row must be admitted:
    // refusing a mixed partial graph is stronger than silently pruning it.
    let mut paths = BTreeMap::new();
    let mut unresolved = Vec::new();
    for name in &names {
        if name == presets::AGENT_PRESETS || host_hook_bridge(name) {
            continue;
        }
        match presets::contained_module(package, name) {
            Ok(path) if crate::runtime::native_entry_problem(&path, path.is_file()).is_none() => {
                paths.insert(name.clone(), path);
            }
            _ => unresolved.push(name),
        }
    }
    if paths.is_empty()
        && !names.iter().any(|name| {
            name == presets::AGENT_PRESETS
                || (host_hook_bridge(name)
                    && !matches!(name.as_str(), DSH_MCP_CLIENT | DSH_SKILL_FILESYSTEM))
        })
    {
        return Ok(Vec::new());
    }
    if !unresolved.is_empty() {
        return refuse(format!(
            "Native composition contains an unsupported or missing row with no exact admitted module ({}); no partial graph was installed.",
            refusal_list(unresolved.iter().map(|name| refusal_text(name)))
        ));
    }
    let mut files = serde_json::Map::new();
    walk_files(&package.root, |path, is_dir| {
        let lower = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase();
        let suffix = path
            .extension()
            .map(|ext| ext.to_string_lossy().to_ascii_lowercase());
        require(
            (!lower.starts_with('.') || lower == ".agent-presets")
                && !matches!(
                    lower.as_str(),
                    "credentials" | "credentials.json" | "secrets.json" | "id_rsa" | "id_ed25519"
                )
                && !matches!(suffix.as_deref(), Some("pem" | "key" | "p12" | "pfx")),
            "Package a closed Native source without hidden files, repository metadata or credential files.",
        )?;
        if is_dir {
            return Ok(());
        }
        let relative = relative_slash(path, &package.root);
        let bytes = read_file(
            &package.contained(Path::new(&relative))?,
            output.remaining(),
        )?;
        files.insert(relative.clone(), json!(sha256_hex(&bytes)));
        output.add(format!("source/{relative}"), bytes)
    })?;
    let mut selected_manifests = std::collections::BTreeSet::new();
    let mut client_paths: Vec<_> = paths
        .iter()
        .map(|(name, path)| (name.clone(), path.clone()))
        .collect();
    for name in &names {
        if name == presets::AGENT_PRESETS || host_hook_bridge(name) {
            let manifest = format!("source/node_modules/{name}/package.json");
            if output.files.contains_key(&manifest) {
                client_paths.push((
                    name.clone(),
                    package.root.join(format!("node_modules/{name}/index.js")),
                ));
            }
        }
    }
    for (name, path) in client_paths {
        let mut directory = path.parent();
        while let Some(parent) = directory.filter(|parent| parent.starts_with(&package.root)) {
            let manifest_path = relative_slash(&parent.join("package.json"), &package.root);
            let key = format!("source/{manifest_path}");
            if let Some(bytes) = output.files.get(&key) {
                if selected_manifests.insert(manifest_path) {
                    require(
                        bytes.len() <= MAX_DOCUMENT,
                        "Package manifest exceeds the conversion size limit.",
                    )?;
                    let text = std::str::from_utf8(bytes).map_err(|_| {
                        anyhow::Error::msg("Configuration and skill entrypoints must be UTF-8.")
                    })?;
                    let manifest = parse_json(text)?;
                    if manifest
                        .get("dsh")
                        .and_then(|dsh| dsh.get("client"))
                        .is_some_and(|client| !matches!(client, Value::Null))
                    {
                        client_outcomes.push(DshOutcome {
                            row: Some(refusal_text(&name)),
                            package: Some(refusal_text(
                                manifest.get("name").and_then(Value::str).unwrap_or(&name),
                            )),
                            kind: "client".into(),
                            outcome: "skipped".into(),
                            reason: "Plugin-provided UI is not imported or run by Codewhale."
                                .into(),
                            layer: None,
                            patch: None,
                        });
                    }
                }
                break;
            }
            directory = parent.parent();
        }
    }
    let mut selected = Vec::new();
    for name in &names {
        if name == presets::AGENT_PRESETS || host_hook_bridge(name) {
            continue;
        }
        let path = paths
            .get(name)
            .ok_or_else(|| anyhow::Error::msg("Native module lost its admitted path"))?;
        let relative = relative_slash(path, &package.root);
        let digest = files
            .get(&relative)
            .ok_or_else(|| anyhow::Error::msg("Native module was absent from source closure"))?;
        selected.push(json!({"name":name,"path":relative,"sha256":digest}));
    }
    document["modules"] = json!(selected);
    document["files"] = json!(files);
    if presets::has_roster(entries) {
        presets::prepare(package, &document, output)?;
        return Ok(names);
    }
    output.add(
        "native/composition.json".into(),
        format!("{}\n", serde_json::to_string_pretty(&document)?).into_bytes(),
    )?;
    output.add("native/index.mjs".into(),b"import { mountReviewedComposition } from '@codewhale/dsh-composition';\nimport spec from './composition.json' with { type: 'json' };\nexport async function apply(ctx) { await mountReviewedComposition(ctx, new URL('../source/', import.meta.url).href, spec); }\n".to_vec())?;
    Ok(names)
}

pub fn derived_plugin_name(package_name: &str) -> Option<String> {
    let segment = package_name
        .rsplit('/')
        .next()
        .unwrap_or(package_name)
        .to_ascii_lowercase();
    let mut name = String::new();
    for c in segment.chars() {
        let c = if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' {
            c
        } else {
            '-'
        };
        if c == '-' && name.ends_with('-') {
            continue;
        }
        name.push(c);
    }
    let name = name.trim_matches(|c| c == '-' || c == '.').to_string();
    valid_plugin_name(&name).then_some(name)
}

fn valid_plugin_name(name: &str) -> bool {
    pattern(r"^[a-z0-9](?:[a-z0-9.-]{0,62}[a-z0-9])?$").is_match(name)
        && !name.contains("..")
        && !name.contains("--")
}

// ─────────────────────────────────────────────────────────────────────────────
// Entry point
// ─────────────────────────────────────────────────────────────────────────────

/// Convert the DSH bundle package at `package` into a fresh native bundle at
/// `output`. `output` must not exist; nothing is written unless every
/// component validated, and a failed write removes what it created.
pub fn convert_package(package: &Path, output: &Path) -> Result<DshConversion> {
    static CONVERSION: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _conversion = CONVERSION
        .lock()
        .map_err(|_| anyhow::Error::msg("DSH preparation is unavailable"))?;
    let package = Package::open(package)?;
    require(
        !output.exists(),
        "Output already exists; choose a fresh directory. Nothing was overwritten.",
    )?;
    if let Some(parent) = output.parent().and_then(|p| p.canonicalize().ok()) {
        require(
            !parent.starts_with(&package.root),
            "Output must be outside the selected bundle.",
        )?;
    }
    let loaded = load_bundle(&package)?;
    let mut output_files = OutputFiles::default();
    let mut client_outcomes = Vec::new();
    let native_rows = native_composition_files(
        &package,
        &loaded.entries,
        loaded.composition,
        &mut output_files,
        &mut client_outcomes,
    )?;
    let requires_native = !native_rows.is_empty();
    let mut notes = loaded.notes;
    let mut components = Components::default();
    if !requires_native {
        components.walk(&package, &loaded.entries, None)?;
    }
    require(
        components.servers.len() <= MAX_SERVERS,
        "At most 64 MCP servers can be converted at once.",
    )?;
    let mut outcomes = loaded.outcomes;
    outcomes.append(&mut components.outcomes);
    outcomes.append(&mut client_outcomes);
    notes.append(&mut components.notes);

    let package_name = loaded
        .manifest
        .get("name")
        .and_then(Value::str)
        .map(str::to_string);
    let package_version = loaded
        .manifest
        .get("version")
        .and_then(Value::str)
        .filter(|v| !v.trim().is_empty())
        .map(str::to_string);
    let Some(plugin_name) = package_name.as_deref().and_then(derived_plugin_name) else {
        return refuse(
            "The package name cannot form a native plugin name (1–64 lowercase letters/digits with single dots or hyphens).",
        );
    };

    let mut skill_sources = Vec::new();
    for directory in &components.skill_dirs {
        let mut children: Vec<_> = fs::read_dir(directory)?.collect::<std::io::Result<_>>()?;
        children.sort_by_key(fs::DirEntry::file_name);
        for child in children {
            let path = child.path();
            if child.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            if path.join("SKILL.md").is_file() || path.extension().is_some_and(|ext| ext == "md") {
                skill_sources.push(path);
            }
        }
    }
    let mut skills = Vec::new();
    for source in skill_sources.iter().filter(|_| !requires_native) {
        let name = skill_files(source, &mut output_files)?;
        require(
            !skills.contains(&name),
            "Duplicate skill name; no files were written.",
        )?;
        skills.push(name);
    }
    if !requires_native {
        for (name, root) in &components.roots {
            stdio_files(root, name, &mut output_files)?;
        }
    } else {
        // Live rows register only under their selected Native entry. The
        // declarative copy would otherwise leak across all caller presets.
        components.servers.clear();
    }

    if let Some(bytes) = output_files.files.get("native/presets.json") {
        let catalog: Json = serde_json::from_slice(bytes)?;
        for row in catalog
            .get("presets")
            .and_then(Json::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(reason) = row.get("broken").and_then(Json::as_str) {
                outcomes.push(DshOutcome {
                    row: row.get("id").and_then(Json::as_str).map(str::to_string),
                    package: Some(presets::AGENT_PRESETS.into()),
                    kind: "native-preset".into(),
                    outcome: "skipped".into(),
                    reason: reason.into(),
                    layer: None,
                    patch: None,
                });
            }
        }
    }

    for outcome in &mut outcomes {
        if outcome.kind == "foreign"
            && outcome
                .package
                .as_ref()
                .is_some_and(|name| native_rows.contains(name))
        {
            outcome.kind = "native".to_string();
            outcome.outcome = "converted".to_string();
            outcome.reason =
                "reviewed Native composition; core authority and experimental host required"
                    .to_string();
        }
    }
    if output_files.files.is_empty() && components.servers.is_empty() {
        let skipped = refusal_list(
            outcomes
                .iter()
                .filter(|outcome| outcome.needs_manual_port())
                .map(|outcome| {
                    format!(
                        "{} ({}): {}",
                        refusal_text(outcome.row.as_deref().unwrap_or("unlabeled")),
                        refusal_text(outcome.package.as_deref().unwrap_or("unlabeled")),
                        refusal_text(&outcome.reason)
                    )
                }),
        );
        return refuse(if skipped.is_empty() {
            "No portable components in this package: nothing to import.".to_string()
        } else {
            format!(
                "No portable components in this package: nothing to import. Skipped rows, each needing a manual port: {skipped}"
            )
        });
    }

    let mut hosts = components.hosts.clone();
    hosts.sort();
    hosts.dedup();
    // Only portable stdio roots copied for a Node command require Node. Native
    // composition runs on the selected host runtime and must remain usable on
    // a reviewed Bun-only installation; individual MCP launches keep their own
    // exact command/dependency admission.
    let requires_node = !components.roots.is_empty();
    let mut manifest = json!({
        "$schema": "https://agent-plugins.org/schemas/plugin.json",
        "name": plugin_name,
    });
    for field in ["version", "description"] {
        if let Some(value) = loaded
            .manifest
            .get(field)
            .and_then(Value::str)
            .filter(|v| !v.trim().is_empty())
        {
            manifest[field] = json!(value);
        }
    }
    let mut extension = serde_json::Map::new();
    if requires_native {
        let paths: Vec<_> = output_files
            .files
            .keys()
            .filter(|p| p.starts_with("native/presets/") && p.ends_with(".mjs"))
            .cloned()
            .collect();
        extension.insert(
            "native".into(),
            if paths.is_empty() {
                json!({"path":"native/index.mjs"})
            } else {
                json!({"paths":paths})
            },
        );
    }
    if !hosts.is_empty() {
        extension.insert("capabilities".into(), json!({"network_hosts": hosts}));
    }
    if requires_node {
        extension.insert("when".into(), json!({"binaries": ["node"]}));
    }
    if !extension.is_empty() {
        manifest["extensions"] = json!({"net.codewhale": extension});
    }
    notes.insert(
        0,
        format!(
            "source package: {}{}",
            package_name.as_deref().unwrap_or("unnamed"),
            package_version
                .as_deref()
                .map(|v| format!("@{v}"))
                .unwrap_or_default()
        ),
    );
    output_files.add(
        "plugin.json".into(),
        format!("{}\n", serde_json::to_string_pretty(&manifest)?).into_bytes(),
    )?;
    let (local, remote): (Vec<_>, Vec<_>) = components
        .servers
        .iter()
        .partition(|(_, server)| server["type"] == "stdio");
    if !components.servers.is_empty() {
        let servers: serde_json::Map<String, Json> = components.servers.iter().cloned().collect();
        output_files.add(
            "mcp.json".into(),
            format!(
                "{}\n",
                serde_json::to_string_pretty(&json!({"mcpServers": servers}))?
            )
            .into_bytes(),
        )?;
    }

    let conversion = DshConversion {
        plugin_name,
        source_package: package_name,
        source_version: package_version,
        manifest_sha256: loaded.manifest_sha256,
        layers: loaded.layers,
        outcomes,
        diagnostics: notes,
        skills,
        remote_servers: remote.iter().map(|(name, _)| name.clone()).collect(),
        local_servers: local.iter().map(|(name, _)| name.clone()).collect(),
        network_hosts: hosts,
        requires_node,
        requires_native,
        native_rows,
    };
    output_files.add(
        "CONVERSION.md".into(),
        conversion_markdown(&conversion).into_bytes(),
    )?;
    let receipt = json!({
        "schema": "codewhale.plugin-conversion.v1",
        "converter_version": CONVERTER_VERSION,
        "converter": "codewhale-native",
        "source": {
            "dialect": "dsh",
            "package": conversion.source_package,
            "version": conversion.source_version,
            "manifest_sha256": conversion.manifest_sha256,
            "patch_layers": conversion.layers,
        },
        "counts": {"skills": conversion.skills.len(), "mcp_servers": components.servers.len(), "native_rows": conversion.native_rows.len()},
        "requires_native": conversion.requires_native,
        "outcomes": conversion.outcomes,
        "diagnostics": conversion.diagnostics,
        "required_manual_ports": conversion.outcomes.iter().filter(|o| o.needs_manual_port()).collect::<Vec<_>>(),
    });
    output_files.add(
        "CONVERSION.json".into(),
        format!("{}\n", serde_json::to_string_pretty(&receipt)?).into_bytes(),
    )?;

    write_output(output, &output_files)?;
    Ok(conversion)
}

fn conversion_markdown(conversion: &DshConversion) -> String {
    let mut lines = vec![
        "# Conversion receipt".to_string(),
        String::new(),
        format!(
            "Converter version {CONVERTER_VERSION} (native Codewhale import). Source dialect: dsh."
        ),
        format!(
            "Source package: {}{}",
            conversion.source_package.as_deref().unwrap_or("unnamed"),
            conversion
                .source_version
                .as_deref()
                .map(|v| format!("@{v}"))
                .unwrap_or_default()
        ),
        format!("Source manifest sha256: {}", conversion.manifest_sha256),
        "Selected patch layers, applied in declaration order:".to_string(),
    ];
    lines.extend(conversion.layers.iter().map(|layer| {
        format!(
            "- {} (sha256 {}, {} bytes)",
            layer.path, layer.sha256, layer.bytes
        )
    }));
    lines.push(String::new());
    lines.push(format!(
        "Converted {} Skills, {} remote and {} local MCP declarations; {} reviewed Native composition rows.",
        conversion.skills.len(),
        conversion.remote_servers.len(),
        conversion.local_servers.len(),conversion.native_rows.len()
    ));
    lines.push(String::new());
    if !conversion.outcomes.is_empty() {
        lines.push("## Component outcomes".to_string());
        lines.push(String::new());
        lines.push(
            "Unsupported rows need a manual port; disabled rows remain intentionally inactive."
                .to_string(),
        );
        lines.push(String::new());
        for outcome in &conversion.outcomes {
            lines.push(format!(
                "- {} ({}) {}: {}{}",
                outcome.row.as_deref().unwrap_or("unlabeled"),
                outcome.package.as_deref().unwrap_or("unlabeled"),
                outcome.kind,
                outcome.outcome,
                if outcome.reason.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", outcome.reason)
                }
            ));
        }
        lines.push(String::new());
    }
    if !conversion.diagnostics.is_empty() {
        lines.push("Bundle diagnostics:".to_string());
        lines.extend(
            conversion
                .diagnostics
                .iter()
                .map(|note| format!("- {note}")),
        );
        lines.push(String::new());
    }
    lines.extend(
        [
            "No selected-package code, package manager, install hook, network request or credential lookup ran; the pinned pure review evaluator parsed configuration.",
            "No environment variable values were resolved. Only files inside the selected package were read;",
            "host paths are never inferred from expressions or copied.",
            "Companion skill files and packaged Node source were copied as data.",
            "Local MCP runs with host-user process authority, not an OS sandbox; stdio does not confine its network or files.",
            "The bundle is installed disabled and untrusted: review it with /plugin validate <name> and the exact trust token before enabling.",
            "Remote MCP output uses Streamable HTTP only. Conversion does not prove server connectivity or foreign runtime compatibility.",
        ]
        .map(str::to_string),
    );
    format!("{}\n", lines.join("\n"))
}

fn write_output(output: &Path, files: &OutputFiles) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        fs::DirBuilder::new().mode(0o700).create(output)?;
    }
    #[cfg(not(unix))]
    fs::create_dir(output)?;
    let result = (|| -> Result<()> {
        for (relative, content) in &files.files {
            let destination = output.join(relative);
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination)?;
            std::io::Write::write_all(&mut file, content)?;
        }
        Ok(())
    })();
    if result.is_err() {
        // The directory was created by this call, so removing it removes
        // only what this conversion wrote.
        let _ = fs::remove_dir_all(output);
    }
    result.map_err(|_| {
        anyhow::Error::msg("Writing the converted bundle failed; nothing was installed.")
    })
}

/// Convert into a private scratch directory and return it with the receipt.
/// The directory is removed when the returned guard drops.
pub fn convert_to_scratch(package: &Path) -> Result<(tempfile::TempDir, PathBuf, DshConversion)> {
    let scratch = tempfile::Builder::new()
        .prefix("codewhale-dsh-import-")
        .tempdir()?;
    let bundle = scratch.path().join("bundle");
    let conversion = convert_package(package, &bundle)?;
    Ok((scratch, bundle, conversion))
}

/// Whether `path` looks like a DSH bundle package (for routing a plain local
/// install to the importer instead of the native bundle reader).
pub fn is_dsh_package(path: &Path) -> bool {
    let Ok(package) = Package::open(path) else {
        return false;
    };
    let Ok(path) = package.contained(Path::new("package.json")) else {
        return false;
    };
    read_file(&path, MAX_DOCUMENT)
        .ok()
        .and_then(|bytes| utf8(bytes).ok())
        .and_then(|text| parse_json(&text).ok())
        .is_some_and(|manifest| {
            manifest
                .get("dsh")
                .and_then(|dsh| dsh.get("bundle"))
                .is_some()
        })
}
