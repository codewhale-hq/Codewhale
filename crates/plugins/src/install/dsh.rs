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
    pub client_closures: Vec<DshClientClosure>,
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
    pub platforms: Option<Vec<String>>,
}

fn host_node_platform() -> Option<&'static str> {
    match std::env::consts::OS {
        "macos" => Some("darwin"),
        "windows" => Some("win32"),
        "linux" => Some("linux"),
        _ => None,
    }
}

fn node_platform(name: &str) -> Option<&'static str> {
    match name {
        "darwin" | "macos" => Some("darwin"),
        "win32" | "windows" => Some("win32"),
        "linux" => Some("linux"),
        _ => None,
    }
}

fn codewhale_os_name(node_platform: &str) -> &'static str {
    match node_platform {
        "darwin" => "macos",
        "win32" => "windows",
        _ => "linux",
    }
}

fn target_dsh_version() -> Result<semver::Version> {
    let receipt: Json = serde_json::from_slice(crate::activation::DSH_COMPATIBILITY)
        .map_err(|_| anyhow::Error::msg("The pinned DSH compatibility receipt is unreadable."))?;
    let version = receipt
        .get("target_dsh_version")
        .and_then(Json::as_str)
        .ok_or_else(|| {
            anyhow::Error::msg("The pinned DSH compatibility receipt lacks `target_dsh_version`.")
        })?;
    semver::Version::parse(version).map_err(|_| {
        anyhow::Error::msg(
            "The pinned DSH compatibility receipt has an invalid `target_dsh_version`.",
        )
    })
}

pub(crate) fn supported_dsh_versions() -> Result<Vec<semver::Version>> {
    let primary = target_dsh_version()?;
    let mut versions = vec![primary];
    for extra in ["0.2.1-rc.5", "0.2.2-rc.1"] {
        if let Ok(parsed) = semver::Version::parse(extra)
            && !versions.contains(&parsed)
        {
            versions.push(parsed);
        }
    }
    Ok(versions)
}

#[derive(Clone, Copy)]
enum SemverBoundOp {
    Eq,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone)]
struct SemverBound {
    op: SemverBoundOp,
    version: semver::Version,
}

impl SemverBound {
    fn matches(&self, candidate: &semver::Version) -> bool {
        let ord = candidate.cmp_precedence(&self.version);
        match self.op {
            SemverBoundOp::Eq => ord.is_eq(),
            SemverBoundOp::Lt => ord.is_lt(),
            SemverBoundOp::Le => ord.is_le(),
            SemverBoundOp::Gt => ord.is_gt(),
            SemverBoundOp::Ge => ord.is_ge(),
        }
    }
}

enum ParsedPartialVersion {
    Wildcard,
    Major(u64),
    MajorMinor(u64, u64),
    Exact(semver::Version),
}

fn zero_prerelease(major: u64, minor: u64, patch: u64) -> semver::Version {
    semver::Version {
        major,
        minor,
        patch,
        pre: semver::Prerelease::new("0").expect("static prerelease"),
        build: semver::BuildMetadata::EMPTY,
    }
}

fn parse_partial_version(raw: &str) -> Result<ParsedPartialVersion, String> {
    let trimmed = raw.trim();
    let trimmed = trimmed
        .strip_prefix('v')
        .or_else(|| trimmed.strip_prefix('V'))
        .unwrap_or(trimmed);
    if trimmed.is_empty() {
        return Err("empty version".into());
    }
    if matches!(trimmed, "*" | "x" | "X") {
        return Ok(ParsedPartialVersion::Wildcard);
    }
    let core = trimmed.split(['-', '+']).next().unwrap_or(trimmed);
    let segments: Vec<&str> = core.split('.').collect();
    let is_wild = |s: &str| matches!(s, "*" | "x" | "X");
    match segments.as_slice() {
        [maj] if is_wild(maj) => Ok(ParsedPartialVersion::Wildcard),
        [maj] if !trimmed.contains(['-', '+']) => {
            let major = maj
                .parse::<u64>()
                .map_err(|_| "invalid major".to_string())?;
            Ok(ParsedPartialVersion::Major(major))
        }
        [maj, min] if is_wild(maj) && is_wild(min) && !trimmed.contains(['-', '+']) => {
            Ok(ParsedPartialVersion::Wildcard)
        }
        [maj, min] if is_wild(min) && !trimmed.contains(['-', '+']) => {
            let major = maj
                .parse::<u64>()
                .map_err(|_| "invalid major".to_string())?;
            Ok(ParsedPartialVersion::Major(major))
        }
        [maj, min] if !trimmed.contains(['-', '+']) => {
            let major = maj
                .parse::<u64>()
                .map_err(|_| "invalid major".to_string())?;
            let minor = min
                .parse::<u64>()
                .map_err(|_| "invalid minor".to_string())?;
            Ok(ParsedPartialVersion::MajorMinor(major, minor))
        }
        [maj, min, pat] if is_wild(pat) && !trimmed.contains(['-', '+']) => {
            if is_wild(maj) && is_wild(min) {
                return Ok(ParsedPartialVersion::Wildcard);
            }
            let major = maj
                .parse::<u64>()
                .map_err(|_| "invalid major".to_string())?;
            if is_wild(min) {
                return Ok(ParsedPartialVersion::Major(major));
            }
            let minor = min
                .parse::<u64>()
                .map_err(|_| "invalid minor".to_string())?;
            Ok(ParsedPartialVersion::MajorMinor(major, minor))
        }
        _ => semver::Version::parse(trimmed)
            .map(ParsedPartialVersion::Exact)
            .map_err(|e| e.to_string()),
    }
}

fn parse_comparator_token(token: &str, out: &mut Vec<SemverBound>) -> Result<(), String> {
    let token = token.trim();
    if token.is_empty() {
        return Err("empty comparator".into());
    }
    if let Some(rest) = token.strip_prefix('^') {
        match parse_partial_version(rest)? {
            ParsedPartialVersion::Wildcard => {}
            ParsedPartialVersion::Major(major) => {
                out.push(SemverBound {
                    op: SemverBoundOp::Ge,
                    version: zero_prerelease(major, 0, 0),
                });
                out.push(SemverBound {
                    op: SemverBoundOp::Lt,
                    version: zero_prerelease(major.saturating_add(1), 0, 0),
                });
            }
            ParsedPartialVersion::MajorMinor(major, minor) => {
                out.push(SemverBound {
                    op: SemverBoundOp::Ge,
                    version: zero_prerelease(major, minor, 0),
                });
                let upper = if major > 0 {
                    zero_prerelease(major.saturating_add(1), 0, 0)
                } else {
                    zero_prerelease(0, minor.saturating_add(1), 0)
                };
                out.push(SemverBound {
                    op: SemverBoundOp::Lt,
                    version: upper,
                });
            }
            ParsedPartialVersion::Exact(version) => {
                let upper = if version.major > 0 {
                    zero_prerelease(version.major.saturating_add(1), 0, 0)
                } else if version.minor > 0 {
                    zero_prerelease(0, version.minor.saturating_add(1), 0)
                } else {
                    zero_prerelease(0, 0, version.patch.saturating_add(1))
                };
                out.push(SemverBound {
                    op: SemverBoundOp::Ge,
                    version,
                });
                out.push(SemverBound {
                    op: SemverBoundOp::Lt,
                    version: upper,
                });
            }
        }
        return Ok(());
    }
    if let Some(rest) = token.strip_prefix('~') {
        match parse_partial_version(rest)? {
            ParsedPartialVersion::Wildcard => {}
            ParsedPartialVersion::Major(major) => {
                out.push(SemverBound {
                    op: SemverBoundOp::Ge,
                    version: zero_prerelease(major, 0, 0),
                });
                out.push(SemverBound {
                    op: SemverBoundOp::Lt,
                    version: zero_prerelease(major.saturating_add(1), 0, 0),
                });
            }
            ParsedPartialVersion::MajorMinor(major, minor) => {
                out.push(SemverBound {
                    op: SemverBoundOp::Ge,
                    version: zero_prerelease(major, minor, 0),
                });
                out.push(SemverBound {
                    op: SemverBoundOp::Lt,
                    version: zero_prerelease(major, minor.saturating_add(1), 0),
                });
            }
            ParsedPartialVersion::Exact(version) => {
                let upper = zero_prerelease(version.major, version.minor.saturating_add(1), 0);
                out.push(SemverBound {
                    op: SemverBoundOp::Ge,
                    version,
                });
                out.push(SemverBound {
                    op: SemverBoundOp::Lt,
                    version: upper,
                });
            }
        }
        return Ok(());
    }
    if let Some(rest) = token.strip_prefix(">=") {
        match parse_partial_version(rest)? {
            ParsedPartialVersion::Wildcard => {}
            ParsedPartialVersion::Major(major) => out.push(SemverBound {
                op: SemverBoundOp::Ge,
                version: zero_prerelease(major, 0, 0),
            }),
            ParsedPartialVersion::MajorMinor(major, minor) => out.push(SemverBound {
                op: SemverBoundOp::Ge,
                version: zero_prerelease(major, minor, 0),
            }),
            ParsedPartialVersion::Exact(version) => out.push(SemverBound {
                op: SemverBoundOp::Ge,
                version,
            }),
        }
        return Ok(());
    }
    if let Some(rest) = token.strip_prefix("<=") {
        match parse_partial_version(rest)? {
            ParsedPartialVersion::Wildcard => {}
            ParsedPartialVersion::Major(major) => out.push(SemverBound {
                op: SemverBoundOp::Lt,
                version: zero_prerelease(major.saturating_add(1), 0, 0),
            }),
            ParsedPartialVersion::MajorMinor(major, minor) => out.push(SemverBound {
                op: SemverBoundOp::Lt,
                version: zero_prerelease(major, minor.saturating_add(1), 0),
            }),
            ParsedPartialVersion::Exact(version) => out.push(SemverBound {
                op: SemverBoundOp::Le,
                version,
            }),
        }
        return Ok(());
    }
    if let Some(rest) = token.strip_prefix('>') {
        match parse_partial_version(rest)? {
            ParsedPartialVersion::Wildcard => {
                return Err("impossible >* comparator".into());
            }
            ParsedPartialVersion::Major(major) => out.push(SemverBound {
                op: SemverBoundOp::Ge,
                version: zero_prerelease(major.saturating_add(1), 0, 0),
            }),
            ParsedPartialVersion::MajorMinor(major, minor) => out.push(SemverBound {
                op: SemverBoundOp::Ge,
                version: zero_prerelease(major, minor.saturating_add(1), 0),
            }),
            ParsedPartialVersion::Exact(version) => out.push(SemverBound {
                op: SemverBoundOp::Gt,
                version,
            }),
        }
        return Ok(());
    }
    if let Some(rest) = token.strip_prefix('<') {
        match parse_partial_version(rest)? {
            ParsedPartialVersion::Wildcard => {
                return Err("impossible <* comparator".into());
            }
            ParsedPartialVersion::Major(major) => out.push(SemverBound {
                op: SemverBoundOp::Lt,
                version: zero_prerelease(major, 0, 0),
            }),
            ParsedPartialVersion::MajorMinor(major, minor) => out.push(SemverBound {
                op: SemverBoundOp::Lt,
                version: zero_prerelease(major, minor, 0),
            }),
            ParsedPartialVersion::Exact(version) => out.push(SemverBound {
                op: SemverBoundOp::Lt,
                version,
            }),
        }
        return Ok(());
    }
    let rest = token.strip_prefix('=').unwrap_or(token);
    match parse_partial_version(rest)? {
        ParsedPartialVersion::Wildcard => {}
        ParsedPartialVersion::Major(major) => {
            out.push(SemverBound {
                op: SemverBoundOp::Ge,
                version: zero_prerelease(major, 0, 0),
            });
            out.push(SemverBound {
                op: SemverBoundOp::Lt,
                version: zero_prerelease(major.saturating_add(1), 0, 0),
            });
        }
        ParsedPartialVersion::MajorMinor(major, minor) => {
            out.push(SemverBound {
                op: SemverBoundOp::Ge,
                version: zero_prerelease(major, minor, 0),
            });
            out.push(SemverBound {
                op: SemverBoundOp::Lt,
                version: zero_prerelease(major, minor.saturating_add(1), 0),
            });
        }
        ParsedPartialVersion::Exact(version) => out.push(SemverBound {
            op: SemverBoundOp::Eq,
            version,
        }),
    }
    Ok(())
}

fn parse_npm_range(range: &str) -> Result<Vec<Vec<SemverBound>>, String> {
    let trimmed = range.trim();
    if trimmed.is_empty() || trimmed.starts_with("workspace:") {
        return Err("invalid range".into());
    }
    let mut clauses = Vec::new();
    for raw_clause in trimmed.split("||") {
        let clause = raw_clause.trim();
        if clause.is_empty() {
            clauses.push(Vec::new());
            continue;
        }
        let raw_tokens: Vec<&str> = clause.split_whitespace().collect();
        let mut merged_tokens = Vec::<String>::new();
        let mut idx = 0;
        while idx < raw_tokens.len() {
            let tok = raw_tokens[idx];
            if tok == "-" && !merged_tokens.is_empty() && idx + 1 < raw_tokens.len() {
                let low = merged_tokens.pop().expect("non-empty");
                let high = raw_tokens[idx + 1];
                merged_tokens.push(format!(">={low}"));
                merged_tokens.push(format!("<={high}"));
                idx += 2;
                continue;
            }
            if matches!(tok, ">=" | "<=" | ">" | "<" | "=" | "^" | "~")
                && idx + 1 < raw_tokens.len()
            {
                merged_tokens.push(format!("{tok}{}", raw_tokens[idx + 1]));
                idx += 2;
                continue;
            }
            merged_tokens.push(tok.to_string());
            idx += 1;
        }
        let mut bounds = Vec::new();
        for tok in &merged_tokens {
            parse_comparator_token(tok, &mut bounds)?;
        }
        clauses.push(bounds);
    }
    if clauses.is_empty() {
        return Err("empty range".into());
    }
    Ok(clauses)
}

pub(crate) fn match_supported_dsh_range(range: &str) -> Result<Option<semver::Version>, String> {
    let clauses = parse_npm_range(range)?;
    let supported = supported_dsh_versions().map_err(|e| e.to_string())?;
    for candidate in supported {
        if clauses
            .iter()
            .any(|bounds| bounds.iter().all(|bound| bound.matches(&candidate)))
        {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

struct AdmittedCompatibility {
    dsh_range: Option<String>,
    matched_dsh_version: Option<semver::Version>,
    platforms: Option<Vec<String>>,
}

fn admit_host_compatibility(manifest: &Value) -> Result<AdmittedCompatibility> {
    let target = target_dsh_version()?;
    let (dsh_range, matched_dsh_version) = match manifest.get("engines") {
        Some(Value::Map(_)) => {
            if let Some(declared) = manifest
                .get("engines")
                .and_then(|engines| engines.get("dsh"))
            {
                let Some(range) = declared.str() else {
                    return refuse(
                        "incompatible-version: `engines.dsh` must be a string semver range; author the supported DSH range or remove the field.",
                    );
                };
                match match_supported_dsh_range(range) {
                    Ok(Some(matched)) => (Some(range), Some(matched)),
                    Ok(None) => {
                        return refuse(format!(
                            "incompatible-version: this bundle requires DSH `{}`, but this Codewhale host targets DSH {target} (0.2.1-rc.5 / 0.2.2-rc.1); import a bundle built for that host or update the host.",
                            refusal_text(range)
                        ));
                    }
                    Err(_) => {
                        return refuse(format!(
                            "incompatible-version: `engines.dsh` value `{}` is not a valid semver range.",
                            refusal_text(range)
                        ));
                    }
                }
            } else {
                (None, None)
            }
        }
        Some(Value::Null) | None => (None, None),
        Some(_) => {
            return refuse(
                "incompatible-version: `engines` in package.json must be an object when present.",
            );
        }
    };
    if dsh_range.is_none() {
        match manifest.get("peerDependencies") {
            Some(Value::Map(peers)) => {
                for (pkg, spec_val) in peers {
                    if pkg != "@deepseek-ai/dsh" && !pkg.starts_with("@deepseek-ai/dsh-") {
                        continue;
                    }
                    let Some(raw_spec) = spec_val.str().map(str::trim).filter(|s| !s.is_empty())
                    else {
                        return refuse(format!(
                            "incompatible-version: `peerDependencies.{}` must be a non-empty string semver range.",
                            refusal_text(pkg)
                        ));
                    };
                    let check_range = if let Some(rest) = raw_spec.strip_prefix("workspace:") {
                        let rest = rest.trim();
                        if matches!(rest, "*" | "^" | "~") {
                            continue;
                        }
                        rest
                    } else {
                        raw_spec
                    };
                    match match_supported_dsh_range(check_range) {
                        Ok(Some(_)) => {}
                        Ok(None) => {
                            return refuse(format!(
                                "incompatible-version: `peerDependencies.{}` requires DSH `{}`, but this Codewhale host targets DSH {target} (0.2.1-rc.5 / 0.2.2-rc.1); import a bundle built for that host or update the host.",
                                refusal_text(pkg),
                                refusal_text(raw_spec)
                            ));
                        }
                        Err(_) => {
                            return refuse(format!(
                                "incompatible-version: `peerDependencies.{}` value `{}` is not a valid semver range.",
                                refusal_text(pkg),
                                refusal_text(raw_spec)
                            ));
                        }
                    }
                }
            }
            Some(Value::Null) | None => {}
            Some(_) => {
                return refuse(
                    "incompatible-version: `peerDependencies` in package.json must be an object when present.",
                );
            }
        }
    }
    let platforms = if let Some(declared) = manifest
        .get("dsh")
        .and_then(|dsh| dsh.get("bundle"))
        .and_then(|bundle| bundle.get("platforms"))
    {
        let Value::Seq(items) = declared else {
            return refuse(
                "unsupported-platform: `dsh.bundle.platforms` must be a non-empty list of Node platform names (darwin/macos, win32/windows, linux).",
            );
        };
        require(
            !items.is_empty(),
            "unsupported-platform: `dsh.bundle.platforms` must be a non-empty list of Node platform names (darwin/macos, win32/windows, linux).",
        )?;
        let mut platforms = Vec::new();
        for item in items {
            let Some(name) = item.str() else {
                return refuse(
                    "unsupported-platform: every `dsh.bundle.platforms` entry must be a Node platform name (darwin/macos, win32/windows, linux).",
                );
            };
            let Some(platform) = node_platform(name) else {
                return refuse(format!(
                    "unsupported-platform: `{}` is not a supported Node platform name; use darwin/macos, win32/windows or linux.",
                    refusal_text(name)
                ));
            };
            platforms.push(platform);
        }
        let host = host_node_platform().ok_or_else(|| {
            anyhow::Error::msg(format!(
                "unsupported-platform: Codewhale cannot verify host platform `{}` against the bundle's declared platforms.",
                std::env::consts::OS
            ))
        })?;
        require(
            platforms.contains(&host),
            &format!(
                "unsupported-platform: this bundle declares only {}; this host is {host}.",
                platforms.join(", ")
            ),
        )?;
        let mut mapped: Vec<String> = platforms
            .into_iter()
            .map(|p| codewhale_os_name(p).to_string())
            .collect();
        mapped.sort();
        mapped.dedup();
        Some(mapped)
    } else {
        None
    };
    Ok(AdmittedCompatibility {
        dsh_range: dsh_range.map(ToString::to_string),
        matched_dsh_version,
        platforms,
    })
}

fn declares_client(manifest: &Value) -> bool {
    manifest
        .get("dsh")
        .and_then(|dsh| dsh.get("client"))
        .is_some_and(|client| !matches!(client, Value::Null))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DshClientClosure {
    pub package: String,
    pub root: String,
    pub entry: String,
    pub platform: Option<String>,
    pub inject: Vec<String>,
    pub host_provided: Vec<String>,
    pub files: BTreeMap<String, String>,
    pub bytes: usize,
}

const CLIENT_JS: [&str; 3] = ["js", "mjs", "cjs"];
const CLIENT_PROBES: [&str; 8] = [
    "",
    ".js",
    ".mjs",
    ".cjs",
    ".json",
    "/index.js",
    "/index.mjs",
    "/index.cjs",
];
const NODE_BUILTINS: [&str; 24] = [
    "assert",
    "buffer",
    "child_process",
    "cluster",
    "crypto",
    "dgram",
    "dns",
    "events",
    "fs",
    "http",
    "http2",
    "https",
    "module",
    "net",
    "os",
    "path",
    "process",
    "readline",
    "stream",
    "tls",
    "url",
    "util",
    "vm",
    "zlib",
];

fn normalize_slash(path: &str) -> Option<String> {
    if path.contains(['\\', '\0']) {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    Some(parts.join("/"))
}

fn client_entry(manifest: &Value) -> String {
    let target = manifest
        .get("exports")
        .and_then(|exports| exports.get("./client"))
        .and_then(|export| match export {
            Value::Str(target) => Some(target.as_str()),
            Value::Map(_) => ["import", "default", "."]
                .iter()
                .find_map(|key| export.get(key).and_then(Value::str)),
            _ => None,
        });
    target.unwrap_or("client.js").to_string()
}

fn package_target(manifest: &Value, subpath: &str) -> Option<String> {
    let key = if subpath.is_empty() {
        ".".to_string()
    } else {
        format!("./{subpath}")
    };
    let from_exports = manifest.get("exports").and_then(|exports| {
        let slot = if matches!(exports, Value::Map(_))
            && exports.keys().iter().any(|name| name.starts_with('.'))
        {
            exports.get(&key)
        } else if subpath.is_empty() {
            Some(exports)
        } else {
            None
        };
        match slot? {
            Value::Str(target) => Some(target.clone()),
            conditions @ Value::Map(_) => ["import", "browser", "require", "default"]
                .iter()
                .find_map(|name| conditions.get(name).and_then(Value::str))
                .map(str::to_string),
            _ => None,
        }
    });
    from_exports.or_else(|| {
        if subpath.is_empty() {
            Some(
                manifest
                    .get("main")
                    .and_then(Value::str)
                    .unwrap_or("index.js")
                    .to_string(),
            )
        } else {
            Some(subpath.to_string())
        }
    })
}

struct ClientScan<'a> {
    files: &'a BTreeMap<String, Vec<u8>>,
    closure: BTreeMap<String, String>,
    host: std::collections::BTreeSet<String>,
    bytes: usize,
}

impl ClientScan<'_> {
    fn probe(&self, base: &str) -> Option<String> {
        CLIENT_PROBES
            .iter()
            .map(|suffix| format!("{base}{suffix}"))
            .find(|candidate| {
                candidate.starts_with("source/") && self.files.contains_key(candidate.as_str())
            })
    }

    fn resolve(&mut self, from: &str, specifier: &str) -> Result<Option<String>, String> {
        let directory = from.rsplit_once('/').map_or("", |(dir, _)| dir);
        if specifier.starts_with("./") || specifier.starts_with("../") || specifier == "." {
            let base = normalize_slash(&format!("{directory}/{specifier}"))
                .ok_or_else(|| format!("`{specifier}` is outside the reviewed package files"))?;
            return self
                .probe(&base)
                .map(Some)
                .ok_or_else(|| format!("`{specifier}` is not in the reviewed package files"));
        }
        if specifier.starts_with('/') || specifier.contains(':') || specifier.is_empty() {
            return Err(format!(
                "`{specifier}` is an absolute, URL or built-in specifier"
            ));
        }
        let (name, subpath) = match specifier.split_once('/') {
            Some((scope, rest)) if scope.starts_with('@') => match rest.split_once('/') {
                Some((leaf, sub)) => (format!("{scope}/{leaf}"), sub.to_string()),
                None => (specifier.to_string(), String::new()),
            },
            Some((head, sub)) => (head.to_string(), sub.to_string()),
            None => (specifier.to_string(), String::new()),
        };
        if NODE_BUILTINS.contains(&name.as_str()) {
            return Err(format!("`{specifier}` is a Node built-in"));
        }
        let mut current = directory.to_string();
        loop {
            let package = format!("{current}/node_modules/{name}");
            if let Some(bytes) = self.files.get(&format!("{package}/package.json")) {
                let manifest = std::str::from_utf8(bytes)
                    .ok()
                    .and_then(|text| parse_json(text).ok())
                    .ok_or_else(|| format!("`{name}` has an unreadable package.json"))?;
                let target = package_target(&manifest, &subpath)
                    .and_then(|target| normalize_slash(&format!("{package}/{target}")))
                    .filter(|path| path.starts_with(&format!("{package}/")))
                    .ok_or_else(|| format!("`{specifier}` has no contained entry"))?;
                self.note(format!("{package}/package.json"));
                return self
                    .probe(&target)
                    .map(Some)
                    .ok_or_else(|| format!("`{specifier}` is not in the reviewed package files"));
            }
            match current.rsplit_once('/') {
                Some((parent, _)) => current = parent.to_string(),
                None => break,
            }
        }
        if name == "react" || name == "react-dom" || name.starts_with("@deepseek-ai/") {
            self.host.insert(name);
            return Ok(None);
        }
        Err(format!(
            "`{specifier}` is an ambient dependency that is not in the reviewed bundle"
        ))
    }

    fn note(&mut self, path: String) {
        if let Some(bytes) = self.files.get(&path)
            && !self.closure.contains_key(&path)
        {
            self.bytes += bytes.len();
            self.closure.insert(path, sha256_hex(bytes));
        }
    }

    fn visit(&mut self, entry: String) -> Result<(), String> {
        let load = pattern(
            r#"(?m)(?:\brequire\s*\(\s*|\bimport\s*\(\s*|(?:^|[;{}])\s*import\s*|\b(?:import|export)\b[^;"'`()]*?\bfrom\b\s*)(?:"([^"\n]*)"|'([^'\n]*)')"#,
        );
        let dynamic = pattern(
            r#"\b(?:require|import)\s*\(\s*(?:[^\s"'\)]|"[^"\n]*"\s*[^\s,\)]|'[^'\n]*'\s*[^\s,\)])"#,
        );
        let mut pending = vec![entry];
        while let Some(path) = pending.pop() {
            if self.closure.contains_key(&path) {
                continue;
            }
            require(self.closure.len() < MAX_FILES, "too many client files")
                .map_err(|error| error.to_string())?;
            self.note(path.clone());
            let extension = path
                .rsplit_once('.')
                .map(|(_, ext)| ext.to_ascii_lowercase());
            if !extension
                .as_deref()
                .is_some_and(|ext| CLIENT_JS.contains(&ext))
            {
                continue;
            }
            let text = std::str::from_utf8(&self.files[&path])
                .map_err(|_| format!("`{path}` is not UTF-8"))?
                .to_string();
            if dynamic.is_match(&text) {
                return Err(format!("`{path}` loads a module by a computed specifier"));
            }
            for capture in load.captures_iter(&text) {
                let specifier = capture
                    .get(1)
                    .or_else(|| capture.get(2))
                    .map_or("", |m| m.as_str());
                if let Some(resolved) = self.resolve(&path, specifier)? {
                    pending.push(resolved);
                }
            }
        }
        Ok(())
    }
}

fn client_closure(
    files: &BTreeMap<String, Vec<u8>>,
    root: &str,
    manifest: &Value,
) -> Result<DshClientClosure, String> {
    let prefix = if root.is_empty() {
        "source".to_string()
    } else {
        format!("source/{root}")
    };
    let raw = client_entry(manifest);
    let entry = normalize_slash(&format!("{prefix}/{raw}"))
        .filter(|path| path.starts_with(&format!("{prefix}/")))
        .ok_or_else(|| format!("client entry `{raw}` leaves the package"))?;
    if !files.contains_key(&entry) {
        return Err(format!(
            "client entry `{raw}` is not in the reviewed package files"
        ));
    }
    let mut scan = ClientScan {
        files,
        closure: BTreeMap::new(),
        host: Default::default(),
        bytes: 0,
    };
    scan.note(format!("{prefix}/package.json"));
    scan.visit(entry.clone())?;
    let client = manifest.get("dsh").and_then(|dsh| dsh.get("client"));
    let inject = match client.and_then(|client| client.get("inject")) {
        Some(Value::Seq(items)) => items
            .iter()
            .filter_map(|item| item.str().map(refusal_text))
            .collect(),
        _ => Vec::new(),
    };
    Ok(DshClientClosure {
        package: manifest
            .get("name")
            .and_then(Value::str)
            .map(refusal_text)
            .unwrap_or_default(),
        root: root.to_string(),
        entry,
        platform: client
            .and_then(|client| client.get("platform"))
            .and_then(Value::str)
            .map(refusal_text),
        inject,
        host_provided: scan.host.into_iter().collect(),
        files: scan.closure,
        bytes: scan.bytes,
    })
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
    let admitted = admit_host_compatibility(&manifest)?;
    let dsh_range = admitted.dsh_range.as_deref();
    let mut notes = Vec::new();
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
    let mut review_input = composition.clone();
    let target = if let Some(range) = dsh_range {
        let target = match admitted.matched_dsh_version {
            Some(matched) => matched,
            None => target_dsh_version()?,
        };
        review_input["compatibility"] = json!({"range":range,"version":target.to_string()});
        Some(target)
    } else {
        None
    };
    let reviewed = match crate::composition_review::review(&review_input) {
        Ok(reviewed) => reviewed,
        #[cfg(any(test, feature = "test-support"))]
        Err(ref msg) if msg == "composition reviewer is unavailable" => {
            let mut patches = Vec::new();
            let mut locations = Vec::new();
            for (layer_meta, raw) in layers
                .iter()
                .zip(composition["layers"].as_array().into_iter().flatten())
            {
                let src = raw.get("source").and_then(Json::as_str).unwrap_or_default();
                let parsed = parse_yaml(src, true)?;
                let Value::Seq(items) = parsed else {
                    return refuse("Each DSH patch layer must be a YAML or JSON array.");
                };
                for (idx, item) in items.into_iter().enumerate() {
                    patches.push(item);
                    locations.push((layer_meta.path.clone(), idx + 1));
                }
            }
            let (entries, outcomes) = evaluate_patches(patches, &mut notes, &locations)?;
            return Ok(LoadedBundle {
                composition,
                manifest,
                manifest_sha256,
                entries,
                notes,
                layers,
                outcomes,
                platforms: admitted.platforms,
            });
        }
        Err(err) => return Err(anyhow::Error::msg(err)),
    };
    if let (Some(range), Some(target)) = (dsh_range, target) {
        match reviewed.get("compatibility").and_then(Json::as_str) {
            Some("match") => {}
            Some("invalid") => {
                return refuse(format!(
                    "incompatible-version: `engines.dsh` value `{}` is not a valid semver range.",
                    refusal_text(range)
                ));
            }
            Some("mismatch") => {
                return refuse(format!(
                    "incompatible-version: this bundle requires DSH `{}`, but this Codewhale host targets DSH {target}; import a bundle built for that host or update the host.",
                    refusal_text(range)
                ));
            }
            _ => {
                return refuse(
                    "incompatible-version: compatibility reviewer returned no valid decision.",
                );
            }
        }
    }
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
        platforms: admitted.platforms,
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
    let canonical_root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
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
            if codewhale_util::path_identity::metadata_is_link_or_reparse(&metadata) {
                let dependency_link = path
                    .strip_prefix(root)
                    .unwrap_or(path.as_path())
                    .components()
                    .any(|part| part.as_os_str().to_str() == Some("node_modules"));
                let contained_target = dependency_link
                    && fs::canonicalize(&path).is_ok_and(|target| {
                        target.starts_with(&canonical_root)
                            && fs::metadata(&target)
                                .is_ok_and(|target| target.is_file() || target.is_dir())
                    });
                require(
                    contained_target,
                    "Source paths must not contain links or reparse points.",
                )?;
                continue;
            }
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

pub(crate) fn asked_host_services(
    files: &BTreeMap<String, Vec<u8>>,
    rows: &[String],
    client_closures: &[DshClientClosure],
    table: &[(&str, crate::host_services::HostService)],
) -> Vec<crate::host_services::HostService> {
    if table.is_empty() {
        return Vec::new();
    }
    let mut injects = std::collections::BTreeSet::new();
    for (path, bytes) in files {
        let Some(relative) = path.strip_prefix("source/") else {
            continue;
        };
        let module = relative
            .rsplit_once('.')
            .is_some_and(|(_, extension)| CLIENT_JS.contains(&extension));
        let own = match relative.split_once("node_modules/") {
            None => true,
            Some(("", dependency)) => rows.iter().any(|row| {
                dependency
                    .strip_prefix(row.as_str())
                    .and_then(|rest| rest.strip_prefix('/'))
                    .is_some_and(|rest| !rest.split('/').any(|part| part == "node_modules"))
            }),
            Some(_) => false,
        };
        if !module
            || !own
            || client_closures
                .iter()
                .any(|closure| closure.files.contains_key(path))
        {
            continue;
        }
        if let Ok(text) = std::str::from_utf8(bytes) {
            injects.extend(crate::host_services::declared_injects(text));
        }
    }
    crate::host_services::services_for_injects(injects.iter().map(String::as_str), table)
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
    client_closures: &mut Vec<DshClientClosure>,
    ui: bool,
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
    if names.is_empty() && !ui {
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
    let native_graph = !paths.is_empty()
        || names.iter().any(|name| {
            name == presets::AGENT_PRESETS
                || (host_hook_bridge(name)
                    && !matches!(name.as_str(), DSH_MCP_CLIENT | DSH_SKILL_FILESYSTEM))
        });
    if !native_graph && !ui {
        return Ok(Vec::new());
    }
    if native_graph && !unresolved.is_empty() {
        return refuse(format!(
            "Native composition contains an unsupported or missing row with no exact admitted module ({}); no partial graph was installed.",
            refusal_list(unresolved.iter().map(|name| refusal_text(name)))
        ));
    }
    let mut files = serde_json::Map::new();
    walk_files(&package.root, |path, is_dir| {
        let relative = relative_slash(path, &package.root);
        let dependency = relative.split('/').any(|part| part == "node_modules");
        let lower = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase();
        let suffix = path
            .extension()
            .map(|ext| ext.to_string_lossy().to_ascii_lowercase());
        require(
            !matches!(suffix.as_deref(), Some("node")),
            "Native Node addons (`.node`) are not supported in DSH bundles.",
        )?;
        require(
            (dependency
                || !lower.starts_with('.')
                || lower == ".agent-presets"
                || relative == ".gitignore")
                && !matches!(
                    lower.as_str(),
                    "credentials"
                        | "credentials.json"
                        | "secrets.json"
                        | "id_rsa"
                        | "id_ed25519"
                        | ".env"
                        | ".npmrc"
                        | ".netrc"
                        | ".git"
                )
                && !matches!(suffix.as_deref(), Some("pem" | "key" | "p12" | "pfx")),
            "Package a closed Native source without credential files or repository metadata; hidden dependency files are admitted only under node_modules.",
        )?;
        if is_dir {
            return Ok(());
        }
        let bytes = read_file(
            &package.contained(Path::new(&relative))?,
            output.remaining(),
        )?;
        files.insert(relative.clone(), json!(sha256_hex(&bytes)));
        output.add(format!("source/{relative}"), bytes)
    })?;
    let mut selected_manifests = std::collections::BTreeSet::new();
    let mut client_paths: Vec<_> = vec![(String::new(), package.root.join("index.js"))];
    client_paths.extend(
        paths
            .iter()
            .map(|(name, path)| (name.clone(), path.clone())),
    );
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
                if selected_manifests.insert(manifest_path.clone()) {
                    require(
                        bytes.len() <= MAX_DOCUMENT,
                        "Package manifest exceeds the conversion size limit.",
                    )?;
                    let text = std::str::from_utf8(bytes).map_err(|_| {
                        anyhow::Error::msg("Configuration and skill entrypoints must be UTF-8.")
                    })?;
                    let manifest = parse_json(text)?;
                    if declares_client(&manifest) {
                        let root = manifest_path
                            .rsplit_once('/')
                            .map_or("", |(parent, _)| parent);
                        let row = (!name.is_empty()).then(|| refusal_text(&name));
                        let package_name = refusal_text(
                            manifest.get("name").and_then(Value::str).unwrap_or(&name),
                        );
                        match client_closure(&output.files, root, &manifest) {
                            Ok(closure) => {
                                client_outcomes.push(DshOutcome {
                                    row,
                                    package: Some(package_name),
                                    kind: "client".into(),
                                    outcome: "retained".into(),
                                    reason: format!(
                                        "client closure reviewed and retained: entry {}, {} files, {} bytes{}; it is not served until the Engine client bootstrap admits it",
                                        refusal_text(&closure.entry),
                                        closure.files.len(),
                                        closure.bytes,
                                        if closure.host_provided.is_empty() {
                                            String::new()
                                        } else {
                                            format!(
                                                ", host-provided modules {}",
                                                refusal_list(closure.host_provided.iter().cloned())
                                            )
                                        }
                                    ),
                                    layer: None,
                                    patch: None,
                                });
                                client_closures.push(closure);
                            }
                            Err(reason) => client_outcomes.push(DshOutcome {
                                row,
                                package: Some(package_name),
                                kind: "client".into(),
                                outcome: "skipped".into(),
                                reason: format!(
                                    "Client UI stays disabled: {}",
                                    refusal_text(&reason)
                                ),
                                layer: None,
                                patch: None,
                            }),
                        }
                    }
                }
                break;
            }
            directory = parent.parent();
        }
    }
    if !native_graph {
        return Ok(Vec::new());
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
    let mut client_closures = Vec::new();
    let ui = declares_client(&loaded.manifest);
    let native_rows = native_composition_files(
        &package,
        &loaded.entries,
        loaded.composition,
        &mut output_files,
        &mut client_outcomes,
        &mut client_closures,
        ui,
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
    if !ui && output_files.files.is_empty() && components.servers.is_empty() {
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
    let mut capabilities = serde_json::Map::new();
    if !hosts.is_empty() {
        capabilities.insert("network_hosts".into(), json!(hosts));
    }
    let host_services = if requires_native {
        asked_host_services(
            &output_files.files,
            &native_rows,
            &client_closures,
            crate::host_services::HOST_SERVICE_INJECTS,
        )
    } else {
        Vec::new()
    };
    if !host_services.is_empty() {
        capabilities.insert("host_services".into(), json!(host_services));
        notes.push(format!(
            "asks for host services: {}; each is granted only by a review that shows it",
            host_services
                .iter()
                .map(|service| service.name())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !capabilities.is_empty() {
        extension.insert("capabilities".into(), Json::Object(capabilities));
    }
    if requires_node || loaded.platforms.is_some() {
        let mut when = serde_json::Map::new();
        if requires_node {
            when.insert("binaries".into(), json!(["node"]));
        }
        if let Some(platforms) = &loaded.platforms {
            when.insert("os".into(), json!(platforms));
        }
        extension.insert("when".into(), Json::Object(when));
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
        client_closures,
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
        "client_closures": conversion.client_closures,
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
