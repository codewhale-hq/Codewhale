//! Echolocation: stateless navigation soundings for tool results.
//!
//! No index, no background work, no new dependencies. Like a whale sounding
//! its pod, every read, grep, file search, and project map echoes its
//! surroundings fresh from source — sibling podmates, symbols, import
//! roots, resolved file links — rendered deterministically (sorted,
//! capped, no timestamps) so a repeated call is byte-identical and
//! cache-friendly. Anything the scanners cannot resolve is omitted,
//! never guessed. Sounding languages: Rust, Python, JavaScript/
//! TypeScript, Go (symbols everywhere; links for Rust/Python/JS;
//! anything else renders podmates only).
//!
//! KV-cache effect: tool-result history only. Soundings never enter the
//! session-pinned prefix.
//!
//! Tuning: `CODEWHALE_ECHO` gates sections for ablation trials
//! (comma-separated `off:<name>` entries, e.g. `off:links,off:mates`;
//! section names: mates, symbols, imports, links, callers, pods,
//! soundings). Unset or malformed means everything on.
//!
//! Terminal buzz: the read footer deepens when the model is closing in.
//! Two distinct files read in a pod (see
//! `ToolContext::pod_visit_count`) put the next read there into buzz
//! mode, which adds the callers section — podmates whose resolved
//! links include this file. Sparse clicks while searching,
//! high-resolution echoes at close range. Deterministic for a fixed
//! history, like everything else here.
//!
//! Known lookalikes (line scanners, not parsers): comment markers
//! inside string literals can mute following lines until the closer
//! (`"/*"` swallows until `"*/"`); Python doc fences toggle on every
//! `"""`/`'''` occurrence, so mixed kinds on one line desync the
//! span. Keywords match at line start only, so the common failure
//! mode is a missed symbol; forging one needs a keyword at a line
//! start inside a multi-line string. Links need that AND an
//! on-disk target, so they forge only when both coincide.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use ignore::WalkBuilder;

/// Hard budget for the rendered footer, in bytes.
pub const POD_FOOTER_MAX_BYTES: usize = 1200;

/// Max podmate names listed before "+N more".
const MAX_PODMATES: usize = 12;
/// Max symbols listed before "+N more".
const MAX_SYMBOLS: usize = 24;
/// Max import roots listed before "+N more".
const MAX_IMPORT_ROOTS: usize = 12;
/// Max characters kept per sounded name.
const MAX_NAME_LEN: usize = 64;
/// Max matched dirs charted per grep result before `pods_omitted` counts.
const MAX_GREP_PODS: usize = 8;
/// Max mates listed per grep pod.
const MAX_GREP_MATES: usize = 8;
/// Max key files sounded per project map.
const MAX_MAP_SOUNDINGS: usize = 8;
/// Files bigger than this are never sounded.
const MAX_SOUNDING_BYTES: u64 = 8 * 1024 * 1024;
/// Max resolved file links listed before "+N more".
const MAX_LINKS: usize = 8;
/// Max callers (reverse links) listed before "+N more".
const MAX_CALLERS: usize = 8;
/// Mate files bigger than this are not scanned for callers.
const MAX_CALLER_SCAN_BYTES: u64 = 256 * 1024;
const MAX_CALLER_SCAN_FILES: usize = 32;
/// Tracked pod reads before the next read there enters the buzz.
pub(crate) const BUZZ_VISIT_THRESHOLD: usize = 2;
/// Max touched symbols named per edit echo before "+N more".
const MAX_EDIT_TOUCHED: usize = 6;
/// Max impacted callers named per edit echo before "+N more".
const MAX_EDIT_CALLERS: usize = 4;
/// Hard byte budget for one edit echo line.
const EDIT_ECHO_MAX_BYTES: usize = 400;
/// Changed spans attributed per edit; past this the edit is a rewrite
/// and the first spans already name its symbols.
const MAX_EDIT_SPANS: usize = 32;
/// Time budget for the touched-symbol diff. Myers is quadratic in the
/// worst case; past this `similar` approximates instead of blocking
/// the edit path, and the echo degrades to first-span attribution.
const EDIT_DIFF_TIMEOUT_MS: u64 = 100;

/// Ablation gate: true when `CODEWHALE_ECHO` disables `section`.
/// Parsed once per process; unset or malformed means everything on.
fn echo_off(section: &str) -> bool {
    static GATES: OnceLock<BTreeSet<String>> = OnceLock::new();
    GATES
        .get_or_init(|| {
            std::env::var("CODEWHALE_ECHO")
                .map(|raw| parse_echo_gates(&raw))
                .unwrap_or_default()
        })
        .contains(section)
}

/// Parse `off:<name>,…` entries; anything else is ignored (fail-open:
/// a typo never silences soundings in production).
fn parse_echo_gates(raw: &str) -> BTreeSet<String> {
    raw.split(',')
        .filter_map(|entry| entry.trim().strip_prefix("off:"))
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .collect()
}

/// A sounded chart for one file. Empty sections render as nothing; a chart
/// with nothing to say renders as the empty string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EchoChart {
    podmates: Vec<String>,
    podmate_total: usize,
    symbols: Vec<String>,
    symbol_total: usize,
    import_roots: Vec<String>,
    import_total: usize,
    links: Vec<String>,
    link_total: usize,
    callers: Vec<String>,
    caller_total: usize,
    caller_candidates_omitted: usize,
}

/// Shorten workspace-relative links to bare names when they live in
/// the same pod (directory) as the sounded file: the pod frame makes
/// them unambiguous, and the mates list shows the same names.
/// Cross-pod links keep their relative path. Never fails closed the
/// wrong way — on any doubt the full path stays.
fn pod_relative_names(workspace: &Path, file_path: &Path, links: Vec<String>) -> Vec<String> {
    let Some(home) = file_path.parent() else {
        return links;
    };
    // Compare in workspace-relative space: joining the workspace here
    // breaks when it is non-canonical but the file path is canonical
    // (macOS /var, Windows \\?\ verbatim paths).
    let home_rel = workspace_relative(workspace, home);
    links
        .into_iter()
        .map(|link| {
            let short =
                Path::new(&link).parent().and_then(|parent| parent.to_str()) == home_rel.as_deref();
            if short {
                Path::new(&link)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or(link)
            } else {
                link
            }
        })
        .collect()
}

/// Sound an echolocation chart for a file that was just read.
/// `workspace` anchors link display (workspace-relative paths);
/// `file_path` is the resolved path (for the sibling sounding); `text`
/// is the full decoded buffer (for symbols, imports, and links); `buzz`
/// adds the callers section when the model is closing in on this pod.
/// Never fails: anything unreadable is omitted from the chart rather
/// than failing the read.
pub fn sound_echolocation(workspace: &Path, file_path: &Path, text: &str, buzz: bool) -> EchoChart {
    let (_, mut podmates) = sound_podmates(file_path);
    let podmate_total = podmates.len();
    podmates.truncate(MAX_PODMATES);
    let (
        mut symbols,
        mut import_roots,
        mut links,
        link_total,
        mut callers,
        caller_total,
        caller_candidates_omitted,
    ) = if let Some(lang) = detect_lang(file_path) {
        let (links, link_total) = sound_links(workspace, file_path, text, lang);
        let (callers, caller_total, caller_candidates_omitted) = if buzz {
            sound_callers(workspace, file_path)
        } else {
            (Vec::new(), 0, 0)
        };
        (
            sound_symbols(text, lang),
            sound_import_roots(text, lang)
                .into_iter()
                .collect::<Vec<_>>(),
            links,
            link_total,
            callers,
            caller_total,
            caller_candidates_omitted,
        )
    } else {
        (Vec::new(), Vec::new(), Vec::new(), 0, Vec::new(), 0, 0)
    };
    let symbol_total = symbols.len();
    symbols.truncate(MAX_SYMBOLS);
    // Squeeze, all lossless: same-pod links and callers render bare
    // (the pod frame disambiguates), and import roots a shown link
    // already names drop — the link says the same thing with a path
    // attached. Totals count what renders, so "+N more" stays honest.
    // Each transform follows its section gate: an `off:links` trial
    // measures links alone, never roots too.
    if !echo_off("links") {
        links = pod_relative_names(workspace, file_path, links);
    }
    if !echo_off("callers") {
        callers = pod_relative_names(workspace, file_path, callers);
    }
    if !echo_off("links") && !echo_off("imports") {
        let link_stems: Vec<&str> = links
            .iter()
            .filter_map(|link| Path::new(link).file_stem()?.to_str())
            .collect();
        import_roots.retain(|root| {
            if link_stems.contains(&root.as_str()) {
                return false;
            }
            // Package roots resolve under their own dir
            // (`pkg` under `pkg/__init__.py`).
            let prefix = format!("{root}/");
            !links.iter().any(|link| link.starts_with(&prefix))
        });
    }
    let import_total = import_roots.len();
    import_roots.truncate(MAX_IMPORT_ROOTS);
    links.truncate(MAX_LINKS);
    callers.truncate(MAX_CALLERS);
    EchoChart {
        podmates,
        podmate_total,
        symbols,
        symbol_total,
        import_roots,
        import_total,
        links,
        link_total,
        callers,
        caller_total,
        caller_candidates_omitted,
    }
}

/// Sound a miss echo for a read that failed with NotFound: the parent
/// pod's mates, so a typo'd path costs one correction instead of a
/// list-dir round trip. `None` when the parent cannot be listed or the
/// mates gate is off. Callers must only invoke this for genuine
/// NotFound failures — never for denylist/permission errors, where an
/// echo would answer the probe it refused.
pub fn sound_miss_echo(file_path: &Path) -> Option<String> {
    if echo_off("mates") {
        return None;
    }
    let (pod_dir, mut mates) = sound_podmates(file_path);
    if mates.is_empty() {
        return None;
    }
    const MAX_MISS_MATES: usize = 8;
    let total = mates.len();
    mates.truncate(MAX_MISS_MATES);
    let dir = pod_dir.as_deref().unwrap_or(".");
    Some(format!(
        "\n\n[Miss echo: {dir} holds: {}]",
        render_capped(&mates, total)
    ))
}

impl EchoChart {
    /// Render the footer. Deterministic for a fixed file and directory:
    /// sorted entries, fixed caps, byte budget, no timestamps.
    pub fn render_footer(&self) -> String {
        let mut out = String::new();
        // No dir: the footer trails the read of this exact file, so the
        // pod frame is already known (miss echoes and grep pods keep
        // theirs — those orient across paths, not within one).
        if !echo_off("mates") && self.podmate_total > 0 {
            out.push_str(&format!(
                "\n\n[Pod ({}): {}]",
                self.podmate_total,
                render_capped(&self.podmates, self.podmate_total)
            ));
        }
        let mut sounding = String::new();
        if !echo_off("symbols") && self.symbol_total > 0 {
            sounding.push_str(&format!(
                "{}: {}",
                count_noun(self.symbol_total, "symbol", "symbols"),
                render_capped(&self.symbols, self.symbol_total)
            ));
        }
        if !echo_off("imports") && self.import_total > 0 {
            if !sounding.is_empty() {
                sounding.push_str("; ");
            }
            sounding.push_str(&format!(
                "imports: {}",
                render_capped(&self.import_roots, self.import_total)
            ));
        }
        if !echo_off("links") && self.link_total > 0 {
            if !sounding.is_empty() {
                sounding.push_str("; ");
            }
            sounding.push_str(&format!(
                "links: {}",
                render_capped(&self.links, self.link_total)
            ));
        }
        if !echo_off("callers") && self.caller_total > 0 {
            if !sounding.is_empty() {
                sounding.push_str("; ");
            }
            sounding.push_str(&format!(
                "heard by: {}",
                render_capped(&self.callers, self.caller_total)
            ));
        }
        if !echo_off("callers") && self.caller_candidates_omitted > 0 {
            if !sounding.is_empty() {
                sounding.push_str("; ");
            }
            sounding.push_str(&format!(
                "{} caller candidates unscanned",
                self.caller_candidates_omitted
            ));
        }
        if !sounding.is_empty() {
            out.push_str(&format!("\n\n[Sound: {sounding}]"));
        }
        truncate_bytes(&out, POD_FOOTER_MAX_BYTES)
    }
}

/// Languages with line-scanner soundings. Everything else is prose:
/// podmates only, no symbols/imports/links.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SoundLang {
    Rust,
    Python,
    JavaScript,
    Go,
}

/// Sounding language from file extension. `None` renders podmates only.
fn detect_lang(file_path: &Path) -> Option<SoundLang> {
    let ext = file_path.extension()?.to_str()?;
    match ext {
        "rs" => Some(SoundLang::Rust),
        "py" | "pyi" => Some(SoundLang::Python),
        "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "mts" | "cts" => Some(SoundLang::JavaScript),
        "go" => Some(SoundLang::Go),
        _ => None,
    }
}

/// Sibling entries of the file's directory: sorted display names (current
/// file marked, directories suffixed) plus the short directory display.
/// A directory that cannot be listed degrades to an empty pod, silently.
fn sound_podmates(file_path: &Path) -> (Option<String>, Vec<String>) {
    let parent = match file_path.parent() {
        Some(parent) => parent,
        None => return (None, Vec::new()),
    };
    let mates = list_dir_entries(parent).unwrap_or_default();
    let current = file_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    let display = mates
        .into_iter()
        .map(|(raw, mut shown)| {
            if Some(&raw) == current.as_ref() {
                shown.push_str(" (this file)");
            }
            shown
        })
        .collect();
    (Some(short_dir_display(parent)), display)
}

/// Visibility gates a pod listing must honor: the calling search hides
/// these names, so pods must not enumerate them either. The denylist
/// always applies; glob/extension/gitignore gates apply when set.
/// `glob_root` anchors glob matching and gitignore stacking, exactly
/// like the calling walk's root.
pub(crate) struct PodVisibility<'a> {
    pub include: &'a [String],
    pub exclude: &'a [String],
    pub extensions: &'a [String],
    pub glob_root: &'a Path,
    pub gitignore: bool,
}

impl PodVisibility<'_> {
    fn allows_entry(&self, abs: &Path, is_dir: bool) -> bool {
        if abs
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with('.'))
            || crate::sandbox::read_guard::active().check(abs).is_err()
        {
            return false;
        }
        let rel = abs
            .strip_prefix(self.glob_root)
            .unwrap_or(abs)
            .to_string_lossy()
            .replace('\\', "/");
        !self
            .exclude
            .iter()
            .any(|pat| super::search::matches_glob(&rel, pat))
            && (is_dir
                || ((self.include.is_empty()
                    || self
                        .include
                        .iter()
                        .any(|pat| super::search::matches_glob(&rel, pat)))
                    && (self.extensions.is_empty()
                        || super::file_search::extension_matches(abs, self.extensions))))
    }
    /// Directed-read visibility: no search gates, denylist still on.
    fn directed(glob_root: &Path) -> PodVisibility<'_> {
        PodVisibility {
            include: &[],
            exclude: &[],
            extensions: &[],
            glob_root,
            gitignore: false,
        }
    }
}

/// Sorted `(raw, display)` entries of a dir: dotfiles skipped, dirs
/// suffixed with `/`, visibility gates applied. `None` when the dir
/// cannot be listed at all (distinct from an empty dir).
fn list_dir_entries(dir: &Path) -> Option<Vec<(String, String)>> {
    list_dir_entries_filtered(dir, &PodVisibility::directed(dir))
}

/// [`list_dir_entries`] plus the caller's search gates: denylist,
/// excludes, includes/extensions (files only, like the walks), and
/// optional parent/global/repository ignore rules.
fn list_dir_entries_filtered(dir: &Path, vis: &PodVisibility<'_>) -> Option<Vec<(String, String)>> {
    let mut builder = WalkBuilder::new(dir);
    builder
        .hidden(false)
        .follow_links(false)
        .require_git(false)
        .max_depth(Some(1))
        .parents(vis.gitignore)
        .ignore(vis.gitignore)
        .git_ignore(vis.gitignore)
        .git_global(vis.gitignore)
        .git_exclude(vis.gitignore);
    let mut entries = builder.build();
    let root = entries.next()?.ok()?;
    if !root.file_type().is_some_and(|kind| kind.is_dir()) {
        return None;
    }
    let mut mates = Vec::new();
    for entry in entries.flatten() {
        let raw = entry.file_name().to_string_lossy().into_owned();
        let abs = entry.path();
        let is_dir = entry.file_type().is_some_and(|kind| kind.is_dir());
        if !vis.allows_entry(abs, is_dir) {
            continue;
        }
        let mut display = raw.clone();
        if is_dir {
            display.push('/');
        }
        mates.push((raw, display));
    }
    mates.sort();
    Some(mates)
}

/// One sounded pod for a grep result: the matched directory and its
/// entries, matched files marked, capped with an honest total.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MatchPod {
    pub dir: String,
    pub mates: Vec<String>,
    pub mate_total: usize,
}

/// Sound pods for search matches: distinct matched dirs, sorted and
/// capped, each with its entries (matched files marked ` (match)`).
/// Entries honor `vis` — the calling search's own gates — so pods
/// never enumerate names the search hides. Returns the pods plus the
/// count of dirs beyond the cap. A dir that cannot be listed still
/// charts its matched files (already search-gated), so a pod is never
/// silently empty. Deterministic for a fixed tree.
pub(crate) fn sound_match_pods(
    resolve_root: &Path,
    files: &[String],
    vis: &PodVisibility<'_>,
) -> (Vec<MatchPod>, usize) {
    if echo_off("pods") {
        return (Vec::new(), 0);
    }
    let mut by_dir: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for file in files {
        if file.is_empty() {
            continue;
        }
        let (dir, name) = split_match_path(file);
        by_dir.entry(dir).or_default().insert(name);
    }
    let mut pods = Vec::new();
    let mut omitted = 0;
    for (dir, matched) in &by_dir {
        if pods.len() >= MAX_GREP_PODS {
            omitted += 1;
            continue;
        }
        pods.push(sound_one_match_pod(resolve_root, dir, matched, vis));
    }
    (pods, omitted)
}

/// Split a match path into (dir display, file name); `.` for the root.
fn split_match_path(file: &str) -> (String, String) {
    let path = Path::new(file);
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(slash_path)
        .unwrap_or_else(|| ".".to_string());
    (dir, name)
}

fn sound_one_match_pod(
    resolve_root: &Path,
    dir: &str,
    matched: &BTreeSet<String>,
    vis: &PodVisibility<'_>,
) -> MatchPod {
    let full = Path::new(dir);
    let full = if full.is_absolute() {
        full.to_path_buf()
    } else {
        resolve_root.join(full)
    };
    let mut mates: Vec<String> = match list_dir_entries_filtered(&full, vis) {
        Some(entries) => entries
            .into_iter()
            .map(|(raw, mut shown)| {
                if matched.contains(&raw) {
                    shown.push_str(" (match)");
                }
                shown
            })
            .collect(),
        // The fallback repeats matched names through the same visibility
        // gates as enumeration; it never discovers additional siblings.
        None => matched
            .iter()
            .filter(|name| vis.allows_entry(&full.join(name), false))
            .map(|name| format!("{name} (match)"))
            .collect(),
    };
    let mate_total = mates.len();
    mates.truncate(MAX_GREP_MATES);
    if mate_total > mates.len() {
        mates.push(format!("+{} more", mate_total - mates.len()));
    }
    MatchPod {
        dir: dir.to_string(),
        mates,
        mate_total,
    }
}

/// Sound one line of symbols for each of the first key files of a
/// project map (sorted, capped). Returns the soundings plus the count
/// of key files beyond the cap. Unreadable, oversized, non-Rust, or
/// symbol-free files are skipped silently.
pub(crate) fn sound_key_files(
    root: &Path,
    key_files: &[String],
) -> (BTreeMap<String, String>, usize) {
    if echo_off("soundings") {
        return (BTreeMap::new(), 0);
    }
    let mut sorted: Vec<&str> = key_files.iter().map(String::as_str).collect();
    sorted.sort();
    let omitted = sorted.len().saturating_sub(MAX_MAP_SOUNDINGS);
    let mut soundings = BTreeMap::new();
    for name in sorted.into_iter().take(MAX_MAP_SOUNDINGS) {
        if let Some(sounding) = sound_file_symbols(root, &root.join(name)) {
            soundings.insert(name.to_string(), sounding);
        }
    }
    (soundings, omitted)
}

/// Sound one line of symbols for a single file: `N symbols: a, b, …`.
/// `None` when unreadable, oversized, non-Rust, or symbol-free.
fn sound_file_symbols(root: &Path, path: &Path) -> Option<String> {
    let lang = detect_lang(path)?;
    super::file::enforce_read_denylist(path, "read").ok()?;
    let selected_path = path.canonicalize().ok()?;
    workspace_relative(root, &selected_path)?;
    if super::file::is_codewhale_credential_path(&selected_path) {
        return None;
    }
    super::file::enforce_read_denylist(&selected_path, "read").ok()?;
    let file = crate::external_credentials::open_secure_regular_file(&selected_path, false).ok()?;
    crate::plugins::registry::validate_opened_regular_file(&selected_path, &file).ok()?;
    if file.metadata().ok()?.len() > MAX_SOUNDING_BYTES {
        return None;
    }
    let mut text = String::new();
    file.take(MAX_SOUNDING_BYTES + 1)
        .read_to_string(&mut text)
        .ok()?;
    if text.len() as u64 > MAX_SOUNDING_BYTES {
        return None;
    }
    let mut symbols = sound_symbols(&text, lang);
    let total = symbols.len();
    if total == 0 {
        return None;
    }
    symbols.truncate(MAX_SYMBOLS);
    Some(format!(
        "{}: {}",
        count_noun(total, "symbol", "symbols"),
        render_capped(&symbols, total)
    ))
}

/// Last two normal components of a directory, for a compact pod label.
fn short_dir_display(parent: &Path) -> String {
    let parts: Vec<String> = parent
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    if parts.is_empty() {
        return ".".to_string();
    }
    let start = parts.len().saturating_sub(2);
    format!("{}/", parts[start..].join("/"))
}

/// Item lines in file order, per language.
fn sound_symbols(text: &str, lang: SoundLang) -> Vec<String> {
    // Symbols carry 1-based source lines (`fn main:12`) so the model can
    // page straight at them — the sounding already sees past truncation,
    // and now it can point there too.
    sound_numbered_symbols(text, lang)
        .into_iter()
        .map(|(n, symbol)| format!("{symbol}:{n}"))
        .collect()
}

/// Structured symbol sounding: 1-based line + bare `kind name`, in file
/// order. The edit echo attributes changed spans against these lines.
fn sound_numbered_symbols(text: &str, lang: SoundLang) -> Vec<(usize, String)> {
    match lang {
        SoundLang::Rust => code_lines(text)
            .filter_map(|(n, line)| parse_item_line(line).map(|s| (n, s)))
            .collect(),
        SoundLang::Python => py_code_lines(text)
            .filter_map(|(n, line)| parse_py_item(line).map(|s| (n, s)))
            .collect(),
        SoundLang::JavaScript => code_lines(text)
            .filter_map(|(n, line)| parse_js_item(line).map(|s| (n, s)))
            .collect(),
        SoundLang::Go => code_lines(text)
            .filter_map(|(n, line)| parse_go_item(line).map(|s| (n, s)))
            .collect(),
    }
}

/// Changed old/new-side spans as (old side, 1-based start, line count).
/// Deletions use the old side; replacements attribute both sides. Reuses
/// the existing diff under its hard timeout and span budget.
fn changed_spans(before: &str, after: &str) -> Vec<(bool, usize, usize)> {
    if before == after {
        return Vec::new();
    }
    similar::TextDiff::configure()
        .timeout(std::time::Duration::from_millis(EDIT_DIFF_TIMEOUT_MS))
        .diff_lines(before, after)
        .ops()
        .iter()
        .flat_map(|op| match op {
            similar::DiffOp::Equal { .. } => vec![],
            similar::DiffOp::Delete {
                old_index, old_len, ..
            } => {
                vec![(true, old_index + 1, *old_len)]
            }
            similar::DiffOp::Insert {
                new_index, new_len, ..
            } => {
                vec![(false, new_index + 1, *new_len)]
            }
            similar::DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                vec![
                    (true, old_index + 1, *old_len),
                    (false, new_index + 1, *new_len),
                ]
            }
        })
        .take(MAX_EDIT_SPANS)
        .collect()
}

/// Symbols the mutation touched, in diff order: declarations born
/// inside a changed span, else the nearest declaration above it (the
/// edit landed inside that symbol's body). Returns the shown names
/// plus the distinct total for the "+N more" tail.
fn touched_symbols(file_path: &Path, before: &str, after: &str) -> (Vec<String>, usize) {
    let empty = (Vec::new(), 0);
    let Some(lang) = detect_lang(file_path) else {
        return empty;
    };
    let old_symbols = sound_numbered_symbols(before, lang);
    let new_symbols = sound_numbered_symbols(after, lang);
    let mut touched: Vec<String> = Vec::new();
    for (old_side, start, len) in changed_spans(before, after) {
        let symbols = if old_side { &old_symbols } else { &new_symbols };
        let mut born_inside = false;
        for (line, name) in symbols {
            if *line >= start && line.saturating_sub(start) < len {
                born_inside = true;
                let symbol = format!("{name}:{line}");
                if !touched.contains(&symbol) {
                    touched.push(symbol);
                }
            }
        }
        if !born_inside
            && let Some((line, name)) = symbols.iter().rev().find(|(line, _)| *line <= start)
        {
            let symbol = format!("{name}:{line}");
            if !touched.contains(&symbol) {
                touched.push(symbol);
            }
        }
    }
    let total = touched.len();
    touched.truncate(MAX_EDIT_TOUCHED);
    (touched, total)
}

/// Model-visible edit echo: which symbols the write/edit touched and
/// which podmates link this file (impacted callers). The model-facing
/// mutation receipt is one line and the diff rides metadata for the
/// TUI, so without this the model never learns what its edit hit.
/// `None` when there is nothing to say. Gated by the existing
/// `symbols`/`callers` sections; budgeted hard.
pub fn sound_edit_echo(
    workspace: &Path,
    file_path: &Path,
    before: &str,
    after: &str,
) -> Option<String> {
    if before == after {
        return None;
    }
    let mut halves = Vec::new();
    if !echo_off("symbols") {
        let (shown, total) = touched_symbols(file_path, before, after);
        if total > 0 {
            halves.push(format!("touched {}", render_capped(&shown, total)));
        }
    }
    if !echo_off("callers") {
        let (callers, total, omitted) = sound_callers(workspace, file_path);
        if total > 0 {
            let shown = &callers[..callers.len().min(MAX_EDIT_CALLERS)];
            halves.push(format!("heard by {}", render_capped(shown, total)));
        }
        if omitted > 0 {
            halves.push(format!("{omitted} caller candidates unscanned"));
        }
    }
    if halves.is_empty() {
        return None;
    }
    Some(truncate_bytes(
        &format!("[Edit echo: {}]", halves.join("; ")),
        EDIT_ECHO_MAX_BYTES,
    ))
}

/// Distinct import roots, sorted, per language.
fn sound_import_roots(text: &str, lang: SoundLang) -> BTreeSet<String> {
    match lang {
        SoundLang::Rust => code_lines(text)
            .filter_map(|(_, line)| parse_use_root(line))
            .collect(),
        SoundLang::Python => py_code_lines(text)
            .flat_map(|(_, line)| parse_py_import_roots(line))
            .collect(),
        SoundLang::JavaScript => code_lines(text)
            .filter_map(|(_, line)| parse_js_import_root(line))
            .collect(),
        SoundLang::Go => code_lines(text)
            .filter_map(|(_, line)| parse_go_import_root(line))
            .collect(),
    }
}

/// Source lines with `//` lines and `/* */` spans removed. Crude on
/// purpose: string literals containing comment markers are a documented
/// lookalike class (see module docs), not silently misread structure.
fn code_lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut in_block = false;
    text.lines().enumerate().filter_map(move |(idx, raw)| {
        let mut line = raw.trim_start();
        if in_block {
            match line.find("*/") {
                Some(end) => {
                    in_block = false;
                    line = line[end + 2..].trim_start();
                }
                None => return None,
            }
        }
        if line.starts_with("//") {
            return None;
        }
        if let Some(start) = line.find("/*") {
            let after_open = &line[start + 2..];
            match after_open.find("*/") {
                Some(end) => {
                    let head = line[..start].trim_end();
                    let tail = after_open[end + 2..].trim_start();
                    line = if head.is_empty() { tail } else { head };
                    if line.is_empty() {
                        return None;
                    }
                }
                None => {
                    in_block = true;
                    line = line[..start].trim_end();
                    if line.is_empty() {
                        return None;
                    }
                }
            }
        }
        Some((idx + 1, line))
    })
}

/// Parse one `fn|struct|enum|trait|type|mod|static|const|impl|union|macro`
/// line into `kind name`. Qualifiers (`pub`, `pub(..)`, `async`, `const`,
/// `unsafe`, `extern "ABI"`) are stripped first.
fn parse_item_line(mut line: &str) -> Option<String> {
    loop {
        line = line.trim_start();
        if let Some(rest) = strip_keyword(line, "pub") {
            line = strip_visibility_group(rest.trim_start())?;
            continue;
        }
        if let Some(rest) = strip_keyword(line, "extern") {
            line = strip_abi_string(rest.trim_start())?;
            continue;
        }
        let mut stripped = false;
        for qualifier in ["async", "unsafe"] {
            if let Some(rest) = strip_keyword(line, qualifier) {
                line = rest;
                stripped = true;
                break;
            }
        }
        if stripped {
            continue;
        }
        // `const fn` (qualifier) vs `const NAME` (item): strip only when
        // `fn` follows, otherwise the keyword table below owns it.
        if let Some(rest) = strip_keyword(line, "const")
            && strip_keyword(rest.trim_start(), "fn").is_some()
        {
            line = rest;
            continue;
        }
        break;
    }
    for (keyword, label) in [
        ("fn", "fn"),
        ("struct", "struct"),
        ("enum", "enum"),
        ("trait", "trait"),
        ("type", "type"),
        ("mod", "mod"),
        ("static", "static"),
        ("const", "const"),
        ("union", "union"),
        ("impl", "impl"),
        ("macro_rules", "macro"),
        ("macro", "macro"),
    ] {
        if let Some(rest) = strip_keyword(line, keyword) {
            let rest = if keyword == "macro_rules" {
                rest.strip_prefix('!').unwrap_or(rest)
            } else {
                rest
            };
            let name = item_head(rest, keyword == "impl")?;
            return Some(format!("{label} {name}"));
        }
    }
    None
}

/// Strip `keyword` only on a word boundary (so `fn` never matches `fnx`).
fn strip_keyword<'a>(line: &'a str, keyword: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(keyword)?;
    match rest.chars().next() {
        None => Some(rest),
        Some(next) if next.is_alphanumeric() || next == '_' => None,
        _ => Some(rest),
    }
}

/// Skip a `pub(..)` visibility group; bare `pub` falls through unchanged.
fn strip_visibility_group(rest: &str) -> Option<&str> {
    match rest.strip_prefix('(') {
        Some(after) => Some(&after[after.find(')')? + 1..]),
        None => Some(rest),
    }
}

/// Skip an `extern "..."` ABI string; bare `extern` falls through unchanged.
fn strip_abi_string(rest: &str) -> Option<&str> {
    match rest.strip_prefix('"') {
        Some(after) => Some(&after[after.find('"')? + 1..]),
        None => Some(rest),
    }
}

/// The sounded name: first identifier for items, the whole head (bounded)
/// for `impl` blocks (`Foo`, `Trait for Foo`, `<T> Foo<T>`).
fn item_head(rest: &str, is_impl: bool) -> Option<String> {
    let rest = rest.trim_start();
    if is_impl {
        let mut head = rest.split(['{', ';']).next()?.trim();
        if let Some(where_at) = head.find(" where ") {
            head = head[..where_at].trim();
        }
        if head.is_empty() {
            return None;
        }
        return Some(truncate_name(head));
    }
    // Capped before collecting: a minified line can run megabytes,
    // and the sounding must never allocate the file to name it.
    let name: String = rest
        .chars()
        .take_while(|next| next.is_alphanumeric() || *next == '_')
        .take(MAX_NAME_LEN + 1)
        .collect();
    if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    Some(truncate_name(&name))
}

/// First `::` segment of a `use` path (`serde`, `crate`, `super`).
fn parse_use_root(line: &str) -> Option<String> {
    let mut line = line.trim_start();
    if let Some(rest) = strip_keyword(line, "pub") {
        line = strip_visibility_group(rest.trim_start())?.trim_start();
    }
    let rest = strip_keyword(line, "use")?.trim_start();
    let head = rest.split([';', '{', ' ', '\t']).next()?.trim();
    let root = head.split("::").next()?.trim();
    valid_root(root)
}

/// Resolve `use`/`mod` edges to workspace-relative files that exist on
/// disk (sorted, with an honest total; the caller caps). Only module
/// files (`x.rs` / `x/mod.rs`) resolve — single items living directly
/// in a root file, external crates, and `#[path]` remaps are left to
/// the import roots rather than guessed.
fn sound_links(
    workspace: &Path,
    file_path: &Path,
    text: &str,
    lang: SoundLang,
) -> (Vec<String>, usize) {
    let mut links = BTreeSet::new();
    match lang {
        SoundLang::Rust => {
            for (_, line) in code_lines(text) {
                if let Some(segments) = parse_use_segments(line) {
                    if let Some(link) = resolve_use_link(workspace, file_path, &segments) {
                        links.insert(link);
                    }
                } else if let Some(child) = parse_mod_decl(line)
                    && let Some(link) = resolve_child_module(workspace, file_path, &child)
                {
                    links.insert(link);
                }
            }
        }
        SoundLang::Python => {
            for (_, line) in py_code_lines(text) {
                for link in resolve_py_links(workspace, file_path, line) {
                    links.insert(link);
                }
            }
        }
        SoundLang::JavaScript => {
            for (_, line) in code_lines(text) {
                if let Some(link) = resolve_js_link(workspace, file_path, line) {
                    links.insert(link);
                }
            }
        }
        // Go imports name packages (dirs), not files: nothing to link.
        SoundLang::Go => {}
    }
    if let Some(own) = workspace_relative(workspace, file_path) {
        links.remove(&own);
    }
    let total = links.len();
    (links.into_iter().collect(), total)
}

/// Full `use` segments (`crate::config::Config` → all three). Groups,
/// globs, and `as` renames cut at the module prefix.
fn parse_use_segments(line: &str) -> Option<Vec<String>> {
    let mut line = line.trim_start();
    if let Some(rest) = strip_keyword(line, "pub") {
        line = strip_visibility_group(rest.trim_start())?.trim_start();
    }
    let rest = strip_keyword(line, "use")?.trim_start();
    let head = rest.split([';', '{', '*']).next()?.trim();
    let head = head.split(" as ").next()?.trim();
    let segments: Vec<String> = head
        .split("::")
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect();
    if segments.is_empty()
        || segments
            .iter()
            .any(|part| part.len() > MAX_NAME_LEN || !is_ident(part))
    {
        return None;
    }
    Some(segments)
}

/// `mod name;` child decl. Inline `mod name {` is a symbol, not an edge.
fn parse_mod_decl(line: &str) -> Option<String> {
    let mut line = line.trim_start();
    if let Some(rest) = strip_keyword(line, "pub") {
        line = strip_visibility_group(rest.trim_start())?.trim_start();
    }
    let rest = strip_keyword(line, "mod")?.trim_start();
    let name: String = rest
        .chars()
        .take_while(|next| next.is_alphanumeric() || *next == '_')
        .take(MAX_NAME_LEN + 1)
        .collect();
    if name.is_empty() || name.len() > MAX_NAME_LEN || !is_ident(&name) {
        return None;
    }
    match rest[name.len()..].trim_start().chars().next() {
        Some(';') => Some(name),
        _ => None,
    }
}

fn is_ident(part: &str) -> bool {
    // No leading digit: not an identifier in any sounded language, so a
    // digit-led token is line noise, never a name.
    !part.is_empty()
        && part
            .chars()
            .next()
            .is_some_and(|first| !first.is_ascii_digit())
        && part.chars().all(|c| c.is_alphanumeric() || c == '_')
}

fn resolve_use_link(workspace: &Path, file_path: &Path, segments: &[String]) -> Option<String> {
    let first = segments.first()?.as_str();
    match first {
        "crate" => {
            let root = find_crate_root(workspace, file_path)?;
            resolve_module_file(workspace, &root, &segments[1..])
        }
        "super" => {
            let depth = segments
                .iter()
                .take_while(|part| part.as_str() == "super")
                .count();
            let mut base = super_base(file_path)?;
            for _ in 1..depth {
                base = base.parent()?.to_path_buf();
            }
            resolve_module_file(workspace, &base, &segments[depth..])
        }
        "self" => {
            let base = self_base(file_path)?;
            resolve_module_file(workspace, &base, &segments[1..])
        }
        _ => None,
    }
}

/// `mod child;` resolves under the current module: the file's own dir
/// for `mod.rs`, a child dir named for the file stem otherwise.
fn resolve_child_module(workspace: &Path, file_path: &Path, child: &str) -> Option<String> {
    let base = self_base(file_path)?;
    resolve_module_file(workspace, &base, std::slice::from_ref(&child.to_string()))
}

/// Longest-prefix module-file match: `a::b::Item` tries `a/b.rs`,
/// `a/b/mod.rs`, then `a.rs`, `a/mod.rs`. First hit on disk wins.
fn resolve_module_file(workspace: &Path, base: &Path, rest: &[String]) -> Option<String> {
    for width in (1..=rest.len()).rev() {
        let joined = rest[..width].join("/");
        for candidate in [
            base.join(format!("{joined}.rs")),
            base.join(&joined).join("mod.rs"),
        ] {
            if super::file::enforce_read_denylist(&candidate, "read").is_err() {
                continue;
            }
            let Ok(selected_path) = candidate.canonicalize() else {
                continue;
            };
            if super::file::is_codewhale_credential_path(&selected_path)
                || super::file::enforce_read_denylist(&selected_path, "read").is_err()
            {
                continue;
            }
            if selected_path.is_file() {
                return workspace_relative(workspace, &selected_path);
            }
        }
    }
    None
}

/// `self::` / `mod x;` base: own dir for `mod.rs` and the crate roots
/// (`lib.rs`/`main.rs`, whose children live beside them, not under a
/// `<stem>/` dir), else `<dir>/<stem>`.
fn self_base(file_path: &Path) -> Option<PathBuf> {
    let dir = file_path.parent()?;
    if file_path
        .file_name()
        .is_some_and(|name| name == "mod.rs" || name == "lib.rs" || name == "main.rs")
    {
        return Some(dir.to_path_buf());
    }
    let stem = file_path.file_stem()?;
    Some(dir.join(stem))
}

/// First-level `super::` base: parent dir for `mod.rs` (whose own dir
/// is the current module), else the file's own dir.
fn super_base(file_path: &Path) -> Option<PathBuf> {
    let dir = file_path.parent()?;
    if file_path.file_name().is_some_and(|name| name == "mod.rs") {
        dir.parent().map(Path::to_path_buf)
    } else {
        Some(dir.to_path_buf())
    }
}

/// Crate root: nearest ancestor (inclusive of the file's dir) holding
/// `main.rs` or `lib.rs`, never walking past the workspace.
fn find_crate_root(workspace: &Path, file_path: &Path) -> Option<PathBuf> {
    let mut current = file_path.parent()?.to_path_buf();
    loop {
        if current.join("main.rs").is_file() || current.join("lib.rs").is_file() {
            return Some(current);
        }
        if current == workspace {
            return None;
        }
        current = current.parent()?.to_path_buf();
    }
}

/// Render a path with `/` separators on every platform, so sounded
/// paths are byte-identical on Windows, macOS, and Linux (portable
/// tests, shared prompt caches). Components are re-joined rather than
/// string-replaced so a literal `\` in a file name survives, and `.`
/// / `..` segments pass through untouched for [`dot_clean`].
fn slash_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn workspace_relative(workspace: &Path, path: &Path) -> Option<String> {
    if let Some(rel) = path.strip_prefix(workspace).ok().map(slash_path) {
        return Some(rel);
    }
    // The tool layer hands us canonical file paths while the workspace
    // may be non-canonical (macOS /var -> /private/var tempdirs,
    // Windows \\?\ verbatim paths). Canonicalize both sides before
    // giving up, or every caller/link match silently misses there.
    let workspace = workspace.canonicalize().ok()?;
    let path = path.canonicalize().ok()?;
    path.strip_prefix(&workspace).ok().map(slash_path)
}

/// Python source lines: `#` comments cut, triple-quote doc spans
/// skipped. Crude on purpose: a `#` inside a string literal mangles
/// the tail, but item/import keywords match at line start, so the
/// mangling cannot forge a symbol. Doc-span tracking toggles on every
/// `"""` / `'''` occurrence — one-liner docstrings toggle twice (no
/// net change); mixed kinds on one line are a documented lookalike.
fn py_code_lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut in_doc = false;
    text.lines().enumerate().filter_map(move |(idx, raw)| {
        let mut line = raw.trim();
        if in_doc {
            let fences = line.matches("\"\"\"").count() + line.matches("'''").count();
            if fences % 2 == 1 {
                in_doc = false;
            }
            return None;
        }
        if let Some(hash) = line.find('#') {
            line = line[..hash].trim_end();
        }
        let fences = line.matches("\"\"\"").count() + line.matches("'''").count();
        if fences % 2 == 1 {
            in_doc = true;
            return None;
        }
        if fences > 0 {
            // One-liner docstring as the whole logical line: nothing
            // else on it can be a symbol.
            let stripped = line.replace("\"\"\"", "").replace("'''", "");
            if stripped.trim().is_empty() {
                return None;
            }
        }
        if line.is_empty() {
            return None;
        }
        Some((idx + 1, line))
    })
}

/// `def name` / `class Name` (any indent — methods included),
/// `async def` unwrapped.
fn parse_py_item(line: &str) -> Option<String> {
    let line = line.trim_start();
    let line = strip_keyword(line, "async").unwrap_or(line).trim_start();
    for (keyword, label) in [("def", "def"), ("class", "class")] {
        if let Some(rest) = strip_keyword(line, keyword) {
            let name: String = rest
                .trim_start()
                .chars()
                .take_while(|next| next.is_alphanumeric() || *next == '_')
                .take(MAX_NAME_LEN + 1)
                .collect();
            if !name.is_empty()
                && name.len() <= MAX_NAME_LEN
                && !name.starts_with(|c: char| c.is_ascii_digit())
            {
                return Some(format!("{label} {name}"));
            }
            return None;
        }
    }
    None
}

/// Import roots of one line: every `import a, b.c` member, the
/// `from` module (`a.b` → `a`), or the import-list leaves for
/// pure-relative modules (`from . import sib` → `sib`). Semicolons
/// split statements.
fn parse_py_import_roots(line: &str) -> Vec<String> {
    let line = line.trim_start();
    let mut out = Vec::new();
    if let Some(rest) = strip_keyword(line, "import") {
        for stmt in rest.split(';') {
            for part in stmt.split(',') {
                let part = part.split(" as ").next().unwrap_or("").trim();
                let root = part.split('.').next().unwrap_or("").trim();
                if let Some(root) = valid_root(root) {
                    out.push(root);
                }
            }
        }
        return out;
    }
    if let Some(rest) = strip_keyword(line, "from") {
        let mut parts = rest.split(" import ");
        let module = parts.next().unwrap_or("").trim();
        if module.trim_start_matches('.').is_empty() {
            // Pure-relative: the module names no package, so leaf names
            // come from the import list (`from . import sib` → `sib`),
            // which stem-shadowing then drops when the link resolves.
            let list = parts.next().unwrap_or("");
            for part in list.split(',') {
                let name = part
                    .split(" as ")
                    .next()
                    .unwrap_or("")
                    .trim_matches(['(', ')', ' ', '\t']);
                let leaf = name.split('.').next().unwrap_or("").trim();
                if let Some(root) = valid_root(leaf) {
                    out.push(root);
                }
            }
            return out;
        }
        let root = py_module_root(module);
        if !root.is_empty() {
            out.push(root);
        }
    }
    out
}

/// Root of a Python module path: first named segment after any
/// leading dots (`..parent.x` → `parent`). Pure-relative modules never
/// reach here — the caller takes leaf names from the import list.
fn py_module_root(module: &str) -> String {
    let dots = module.chars().take_while(|c| *c == '.').count();
    let named = module[dots..].split('.').next().unwrap_or("").trim();
    valid_root(named).unwrap_or_default()
}

fn valid_root(root: &str) -> Option<String> {
    if root.is_empty() || root.len() > MAX_NAME_LEN || !is_ident(root) {
        return None;
    }
    Some(root.to_string())
}

/// Resolve one Python import line to existing files. Relative imports
/// walk up from the file's dir; absolute ones try the file's dir (flat
/// scripts) then the workspace root (packages). Both `mod.py` and
/// `pkg/__init__.py` shapes. Existence is the honesty guard.
fn resolve_py_links(workspace: &Path, file_path: &Path, line: &str) -> Vec<String> {
    let line = line.trim_start();
    let mut out = Vec::new();
    if let Some(rest) = strip_keyword(line, "import") {
        // `import a, b.c as d` (semicolons split statements first).
        for stmt in rest.split(';') {
            for part in stmt.split(',') {
                let part = part.split(" as ").next().unwrap_or("").trim();
                let segments: Vec<&str> = part.split('.').map(str::trim).collect();
                if segments.iter().all(|s| is_ident(s)) {
                    out.extend(resolve_py_module(workspace, file_path, 0, &segments));
                }
            }
        }
        return out;
    }
    if let Some(rest) = strip_keyword(line, "from") {
        // `from [..]mod import names` — parenthesized/continued name
        // lists don't matter; only the module part resolves.
        let module = rest.split(" import ").next().unwrap_or("").trim();
        let dots = module.chars().take_while(|c| *c == '.').count();
        let segments: Vec<&str> = module[dots..]
            .split('.')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        if segments.iter().all(|s| is_ident(s)) {
            // `from . import sib` names the leaf in the import list,
            // not the module: resolve each imported name as a sibling.
            if dots > 0 && segments.is_empty() {
                if let Some(names) = rest.split(" import ").nth(1) {
                    for name in names
                        .replace(['(', ')'], " ")
                        .split(',')
                        .map(|n| n.split(" as ").next().unwrap_or("").trim())
                        .filter(|n| is_ident(n))
                    {
                        out.extend(resolve_py_module(
                            workspace,
                            file_path,
                            dots,
                            std::slice::from_ref(&name),
                        ));
                    }
                }
                return out;
            }
            out.extend(resolve_py_module(workspace, file_path, dots, &segments));
        }
        return out;
    }
    out
}

/// Resolve Python module segments: `dots` levels up from the file's dir
/// (0 = absolute: same dir first, workspace root as fallback), then
/// `x.py` or `x/__init__.py`. Same-dir wins outright — no double links.
fn resolve_py_module(
    workspace: &Path,
    file_path: &Path,
    dots: usize,
    segments: &[&str],
) -> Vec<String> {
    if segments.is_empty() {
        return Vec::new();
    }
    if dots == 0 {
        if let Some(dir) = file_path.parent() {
            let hits = py_candidates(workspace, dir, segments);
            if !hits.is_empty() {
                return hits;
            }
        }
        return py_candidates(workspace, workspace, segments);
    }
    let mut base = match file_path.parent() {
        Some(dir) => dir.to_path_buf(),
        None => return Vec::new(),
    };
    for _ in 1..dots {
        match base.parent() {
            Some(parent) => base = parent.to_path_buf(),
            None => return Vec::new(),
        }
    }
    py_candidates(workspace, &base, segments)
}

/// `x.py` / `x/__init__.py` under one base, workspace-relative.
fn py_candidates(workspace: &Path, base: &Path, segments: &[&str]) -> Vec<String> {
    let joined = segments.join("/");
    [
        base.join(format!("{joined}.py")),
        base.join(&joined).join("__init__.py"),
    ]
    .into_iter()
    .filter_map(|candidate| {
        super::file::enforce_read_denylist(&candidate, "read").ok()?;
        let selected_path = candidate.canonicalize().ok()?;
        if super::file::is_codewhale_credential_path(&selected_path)
            || super::file::enforce_read_denylist(&selected_path, "read").is_err()
        {
            return None;
        }
        selected_path
            .is_file()
            .then(|| workspace_relative(workspace, &selected_path))
            .flatten()
    })
    .collect()
}

/// JS/TS item lines: `function`/`class`, `const`/`let`/`var` arrows,
/// `type`/`interface`/`enum`. `export`/`default`/`async`/`declare`/
/// `abstract` unwrapped. Method shorthand skipped (indistinguishable
/// from calls at line-scan resolution — documented omission).
fn parse_js_item(line: &str) -> Option<String> {
    let mut line = line.trim_start();
    for qualifier in ["export", "default", "declare", "abstract", "async"] {
        if let Some(rest) = strip_keyword(line, qualifier) {
            line = rest.trim_start();
        }
    }
    for (keyword, label) in [
        ("function", "function"),
        ("class", "class"),
        ("interface", "interface"),
        ("enum", "enum"),
    ] {
        if let Some(rest) = strip_keyword(line, keyword) {
            let rest = rest
                .trim_start()
                .strip_prefix('*')
                .unwrap_or(rest)
                .trim_start();
            let name = ident_head(rest)?;
            return Some(format!("{label} {name}"));
        }
    }
    if let Some(rest) = strip_keyword(line, "type") {
        let name = ident_head(rest.trim_start())?;
        return Some(format!("type {name}"));
    }
    for qualifier in ["const", "let", "var"] {
        if let Some(rest) = strip_keyword(line, qualifier) {
            let rest = rest.trim_start();
            let name = ident_head(rest)?;
            let after = rest[name.len()..].trim_start();
            let body = after.strip_prefix('=').unwrap_or("").trim_start();
            // Arrow/function value, not data: `= (`, `= async`, `= function`, `=>`.
            if body.starts_with('(')
                || strip_keyword(body, "async").is_some()
                || strip_keyword(body, "function").is_some()
                || body.contains("=>")
            {
                return Some(format!("{qualifier} {name}"));
            }
            return None;
        }
    }
    None
}

fn ident_head(rest: &str) -> Option<String> {
    let name: String = rest
        .chars()
        .take_while(|next| next.is_alphanumeric() || *next == '_' || *next == '$')
        .take(MAX_NAME_LEN + 1)
        .collect();
    if name.is_empty()
        || name.len() > MAX_NAME_LEN
        || name.starts_with(|c: char| c.is_ascii_digit())
    {
        return None;
    }
    Some(name)
}

/// JS/TS import roots: relative specs resolve to the first named
/// segment past the dots (`./server` → `server`); scoped `@s/n` kept
/// whole; otherwise the first path segment. Covers `import`,
/// `export-from`, `require`, and dynamic `import()`.
fn parse_js_import_root(line: &str) -> Option<String> {
    let spec = js_import_spec(line)?;
    if spec.starts_with('.') {
        let tail = spec.trim_start_matches('.').trim_start_matches('/');
        let first = tail.split('/').next().unwrap_or("").trim();
        return valid_root(first);
    }
    if let Some(scoped) = spec.strip_prefix('@') {
        let mut parts = scoped.split('/');
        let (scope, name) = (parts.next()?, parts.next()?);
        if scope.is_empty() || name.is_empty() {
            return None;
        }
        return Some(format!("@{scope}/{name}"));
    }
    let root = spec.split('/').next()?.trim();
    valid_root(root)
}

/// The module specifier of an import-ish line, unquoted.
fn js_import_spec(line: &str) -> Option<String> {
    let line = line.trim_start();
    // Side-effect and from-forms: `import 'x'`, `import … from 'x'`, `export … from 'x'`.
    if strip_keyword(line, "import").is_some() || strip_keyword(line, "export").is_some() {
        return quoted_tail(line);
    }
    // `require('x')`, `import('x')` anywhere a call starts the line.
    // The opener needs a boundary behind it: `myrequire(` and a call
    // spelled inside a string literal (`"require('x')"`) must not forge
    // imports.
    for opener in ["require(", "import("] {
        // Every occurrence: a `myrequire(` earlier on the line must not
        // shadow a real call later on it.
        let mut start = 0;
        while let Some(rel) = line[start..].find(opener) {
            let at = start + rel;
            let bounded = line[..at].chars().next_back().is_none_or(|prev| {
                !(prev.is_alphanumeric()
                    || prev == '_'
                    || prev == '$'
                    || prev == '\''
                    || prev == '"'
                    || prev == '`')
            });
            start = at + opener.len();
            if !bounded {
                continue;
            }
            let after = line[start..].trim_start();
            // A computed argument (`require(name)`) is not a specifier;
            // keep scanning rather than bailing on the whole line.
            let Some(quote) = after.chars().next() else {
                continue;
            };
            if quote == '\'' || quote == '"' || quote == '`' {
                return after[1..].split(quote).next().map(str::to_string);
            }
        }
    }
    None
}

/// The module specifier: first quoted string after `from` when present
/// (`import … from 'x'`), else the first quoted string on the line
/// (side-effect `import 'x'`). Scoping to `from` keeps a trailing
/// string literal (`log("hi")`) from shadowing the specifier, and the
/// earliest opening quote of any kind wins, so a later apostrophe in
/// a trailing expression can never shadow the specifier either.
fn quoted_tail(line: &str) -> Option<String> {
    // Both separators are 6 bytes; the max rfind wins either way.
    let tail = [" from ", " from\t"]
        .iter()
        .filter_map(|sep| line.rfind(sep))
        .max()
        .map(|at| &line[at + 6..])
        .unwrap_or(line);
    let (pos, quote) = ['\'', '"', '`']
        .iter()
        .filter_map(|quote| tail.find(*quote).map(|pos| (pos, *quote)))
        .min()?;
    // `pos` is the byte index of a one-byte quote, so `pos + 1` is a
    // boundary.
    tail[pos + 1..]
        .split(quote)
        .next()
        .filter(|spec| !spec.is_empty())
        .map(str::to_string)
}

/// JS/TS extension search order for relative links.
const JS_LINK_EXTENSIONS: [&str; 8] = ["js", "ts", "jsx", "tsx", "mjs", "cjs", "mts", "cts"];

/// Resolve relative JS/TS specifiers (`./x`, `../x`) against the file's
/// dir: bare, +extensions, +`/index`+extensions. Bare package names
/// never resolve (node_modules is not walked). Query/hash suffixes cut.
fn resolve_js_link(workspace: &Path, file_path: &Path, line: &str) -> Option<String> {
    let mut spec = js_import_spec(line)?;
    if !spec.starts_with('.') {
        return None;
    }
    spec = spec.split(['?', '#']).next()?.to_string();
    let dir = file_path.parent()?;
    let target = dir.join(&spec);
    let mut candidates = vec![target.clone()];
    for ext in JS_LINK_EXTENSIONS {
        candidates.push(target.with_extension(ext));
    }
    for ext in JS_LINK_EXTENSIONS {
        candidates.push(target.join(format!("index.{ext}")));
    }
    for candidate in candidates {
        if super::file::enforce_read_denylist(&candidate, "read").is_err() {
            continue;
        }
        let Ok(selected_path) = candidate.canonicalize() else {
            continue;
        };
        if super::file::is_codewhale_credential_path(&selected_path)
            || super::file::enforce_read_denylist(&selected_path, "read").is_err()
        {
            continue;
        }
        if selected_path.is_file() {
            return workspace_relative(workspace, &selected_path).map(|rel| dot_clean(&rel));
        }
    }
    None
}

/// Lexically collapse `a/../b` (no FS touch — `canonicalize` would
/// resolve symlinks like `/tmp` and break the workspace prefix).
fn dot_clean(rel: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in rel.split('/') {
        if part == ".." {
            parts.pop();
        } else if !part.is_empty() && part != "." {
            parts.push(part);
        }
    }
    parts.join("/")
}

/// Go item lines: `func Name`, methods as `func Type.Method`,
/// `type Name …`.
fn parse_go_item(line: &str) -> Option<String> {
    let line = line.trim_start();
    if let Some(rest) = strip_keyword(line, "func") {
        let rest = rest.trim_start();
        if let Some(after) = rest.strip_prefix('(') {
            // Method: receiver `(name Type)` or `(Type)`.
            let receiver = after.split(')').next()?.trim();
            let typ = receiver.split_whitespace().last()?;
            let typ = typ.trim_start_matches('*').trim_start_matches("[]");
            let after = after.split(')').nth(1)?.trim_start();
            let name = ident_head(after)?;
            if typ.is_empty() || !is_ident(typ) {
                return None;
            }
            return Some(format!("func {typ}.{name}"));
        }
        let name = ident_head(rest)?;
        return Some(format!("func {name}"));
    }
    if let Some(rest) = strip_keyword(line, "type") {
        let rest = rest.trim_start();
        // Single decl only; `type ( … )` blocks list names bare —
        // resolving those needs block tracking, documented omission.
        if rest.starts_with('(') {
            return None;
        }
        let name = ident_head(rest)?;
        return Some(format!("type {name}"));
    }
    None
}

/// Go import roots: last path segment (package name by convention).
/// Covers `import` lines (with optional alias) and the bare quoted
/// entries of `import ( … )` blocks. Anything else starting with an
/// identifier (`return "x"`) is not an import — the alias shape is
/// indistinguishable at line resolution, so keyword-less aliased
/// entries are a documented miss.
fn parse_go_import_root(line: &str) -> Option<String> {
    let line = line.trim_start();
    let rest = match strip_keyword(line, "import") {
        Some(rest) => {
            let rest = rest.trim_start();
            // Optional alias: `name "…"`, `_ "…"`, `. "…"`.
            if rest.starts_with('"') || rest.starts_with('`') {
                rest
            } else {
                rest.split_whitespace().nth(1)?.trim_start()
            }
        }
        None => {
            if line.starts_with('"') || line.starts_with('`') {
                line
            } else {
                return None;
            }
        }
    };
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '`' {
        return None;
    }
    let path = rest[1..].split(quote).next()?;
    let root = path.split('/').next_back()?.trim();
    valid_root(root)
}

/// Reverse links: podmates whose own resolved links include this file.
/// The echo coming back. Exact by construction (reuses the link
/// resolver, no name guessing) and bounded (same-dir mates only,
/// oversized mates skipped, visibility-filtered like the pod itself).
fn sound_callers(workspace: &Path, file_path: &Path) -> (Vec<String>, usize, usize) {
    let own = match workspace_relative(workspace, file_path) {
        Some(own) => own,
        None => return (Vec::new(), 0, 0),
    };
    let parent = match file_path.parent() {
        Some(parent) => parent,
        None => return (Vec::new(), 0, 0),
    };
    let mates: Vec<_> = list_dir_entries(parent)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(raw, _)| {
            let path = parent.join(raw);
            let lang = detect_lang(&path)?;
            (path != file_path).then_some((path, lang))
        })
        .collect();
    let omitted = mates.len().saturating_sub(MAX_CALLER_SCAN_FILES);
    let mut callers = BTreeSet::new();
    for (mate_path, mate_lang) in mates.into_iter().take(MAX_CALLER_SCAN_FILES) {
        if super::file::enforce_read_denylist(&mate_path, "read").is_err() {
            continue;
        }
        let Ok(selected_path) = mate_path.canonicalize() else {
            continue;
        };
        if workspace_relative(workspace, &selected_path).is_none() {
            continue;
        }
        if super::file::is_codewhale_credential_path(&selected_path)
            || super::file::enforce_read_denylist(&selected_path, "read").is_err()
        {
            continue;
        }
        let Ok(file) = crate::external_credentials::open_secure_regular_file(&selected_path, false)
        else {
            continue;
        };
        if crate::plugins::registry::validate_opened_regular_file(&selected_path, &file).is_err()
            || !file
                .metadata()
                .is_ok_and(|meta| meta.len() <= MAX_CALLER_SCAN_BYTES)
        {
            continue;
        }
        let mut text = String::new();
        if file
            .take(MAX_CALLER_SCAN_BYTES + 1)
            .read_to_string(&mut text)
            .is_err()
            || text.len() as u64 > MAX_CALLER_SCAN_BYTES
        {
            continue;
        }
        let (links, _) = sound_links(workspace, &selected_path, &text, mate_lang);
        if links.iter().any(|link| link == &own)
            && let Some(rel) = workspace_relative(workspace, &selected_path)
        {
            callers.insert(rel);
        }
    }
    let total = callers.len();
    (callers.into_iter().collect(), total, omitted)
}

fn truncate_name(head: &str) -> String {
    if head.chars().count() <= MAX_NAME_LEN {
        return head.to_string();
    }
    let kept: String = head.chars().take(MAX_NAME_LEN).collect();
    format!("{kept}…")
}

fn count_noun(count: usize, singular: &str, plural: &str) -> String {
    if count == 1 {
        format!("1 {singular}")
    } else {
        format!("{count} {plural}")
    }
}

fn render_capped(items: &[String], total: usize) -> String {
    let mut out = items.join(", ");
    if total > items.len() {
        if !out.is_empty() {
            out.push_str(", ");
        }
        out.push_str(&format!("+{} more", total - items.len()));
    }
    out
}

/// Hard byte cap at a char boundary, with an ellipsis marker when cut.
fn truncate_bytes(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes.saturating_sub("…".len());
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}…", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, Instant};

    fn open_visibility(glob_root: &Path) -> PodVisibility<'_> {
        PodVisibility {
            include: &[],
            exclude: &[],
            extensions: &[],
            glob_root,
            gitignore: false,
        }
    }

    #[test]
    fn miss_echo_names_parent_mates_for_typoed_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        for name in ["alpha.rs", "beta.rs", "gamma.rs"] {
            fs::write(dir.path().join(name), "x").expect("fixture");
        }
        let echo = sound_miss_echo(&dir.path().join("alpah.rs")).expect("echo");
        assert!(echo.starts_with("\n\n[Miss echo: "), "{echo}");
        assert!(echo.contains("alpha.rs"), "{echo}");
        assert!(echo.contains("beta.rs"), "{echo}");
        assert!(echo.contains("gamma.rs"), "{echo}");
        assert!(!echo.contains("alpah.rs"), "{echo}");
    }

    #[test]
    fn miss_echo_caps_mates_and_reports_the_rest() {
        let dir = tempfile::tempdir().expect("tempdir");
        for i in 0..12 {
            fs::write(dir.path().join(format!("f{i:02}.rs")), "x").expect("fixture");
        }
        let echo = sound_miss_echo(&dir.path().join("nope.rs")).expect("echo");
        assert!(echo.contains("+4 more"), "{echo}");
        assert!(echo.len() < 600, "miss echo stays small: {echo}");
    }

    #[test]
    fn miss_echo_is_none_when_parent_is_unlistable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("no-such-dir").join("nope.rs");
        assert_eq!(sound_miss_echo(&missing), None);
    }

    const SAMPLE: &str = r#"use std::collections::BTreeSet;
use std::path::Path;
use crate::tools::echolocation::sound_echolocation;
use super::MAX_SYMBOLS;
pub use serde_json::Value as JsonValue;
use tokio::fs::{
    self,
    File,
};

// a comment mentioning fn ghost
/// doc comment: struct Phantom
/* block comment: enum Shade */
#[derive(Debug)]
pub struct Whale {
    name: String,
}

pub(crate) async fn swim() {}
unsafe extern "C" fn breach() {}
const DEPTH: u32 = 11_000;
static CURRENT: &str = "cold";
type Pod = Vec<Whale>;
trait Navigable {}
impl Navigable for Whale {}
impl Whale {
    fn dive(&self) {}
}
mod abyss;
macro_rules! song {
    () => {};
}
fnx not_an_item;
define nothing;
"#;

    #[test]
    fn items_sound_in_file_order() {
        let symbols = sound_symbols(SAMPLE, SoundLang::Rust);
        let names: Vec<&str> = symbols.iter().map(String::as_str).collect();
        assert_eq!(
            names,
            [
                "struct Whale:15",
                "fn swim:19",
                "fn breach:20",
                "const DEPTH:21",
                "static CURRENT:22",
                "type Pod:23",
                "trait Navigable:24",
                "impl Navigable for Whale:25",
                "impl Whale:26",
                "fn dive:27",
                "mod abyss:29",
                "macro song:30",
            ]
        );
    }

    #[test]
    fn import_roots_dedupe_and_sort() {
        let roots: Vec<String> = sound_import_roots(SAMPLE, SoundLang::Rust)
            .into_iter()
            .collect();
        assert_eq!(roots, ["crate", "serde_json", "std", "super", "tokio"]);
    }

    #[test]
    fn block_comments_stay_silent_across_lines() {
        let text = "/*\nfn hidden_one() {}\nuse hidden::root;\n*/\nfn visible() {}\n";
        assert_eq!(sound_symbols(text, SoundLang::Rust), ["fn visible:5"]);
        assert!(sound_import_roots(text, SoundLang::Rust).is_empty());
    }

    #[test]
    fn code_after_a_closed_comment_still_sounds() {
        assert_eq!(
            sound_symbols("/* note */ fn after() {}\n", SoundLang::Rust),
            ["fn after:1"]
        );
        assert_eq!(
            sound_symbols("let x = 1; /* fn nope() {} */\n", SoundLang::Rust),
            Vec::<String>::new()
        );
    }

    #[test]
    fn podmates_sort_mark_and_cap() {
        let dir = tempfile::tempdir().expect("tempdir");
        for name in ["zebra.rs", "alpha.rs", "main.rs", ".hidden", "beta"] {
            fs::write(dir.path().join(name), "x").expect("fixture");
        }
        fs::create_dir(dir.path().join("shoal")).expect("subdir");
        let chart = sound_echolocation(
            dir.path(),
            &dir.path().join("main.rs"),
            "fn main() {}\n",
            false,
        );
        assert_eq!(chart.podmate_total, 5);
        assert_eq!(
            chart.podmates,
            [
                "alpha.rs",
                "beta",
                "main.rs (this file)",
                "shoal/",
                "zebra.rs",
            ]
        );
    }

    #[test]
    fn long_pods_truncate_with_a_count() {
        let dir = tempfile::tempdir().expect("tempdir");
        for index in 0..20 {
            fs::write(dir.path().join(format!("file-{index:02}.rs")), "x").expect("fixture");
        }
        let chart = sound_echolocation(dir.path(), &dir.path().join("file-00.rs"), "x\n", false);
        assert_eq!(chart.podmate_total, 20);
        assert_eq!(chart.podmates.len(), MAX_PODMATES);
        let footer = chart.render_footer();
        assert!(footer.contains("+8 more"), "{footer}");
        assert!(footer.len() <= POD_FOOTER_MAX_BYTES);
    }

    #[test]
    fn footer_is_deterministic_and_budgeted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let long = "n".repeat(200);
        let mut text = String::new();
        for index in 0..100 {
            text.push_str(&format!("fn {long}_{index}() {{}}\n"));
        }
        fs::write(dir.path().join("big.rs"), &text).expect("fixture");
        let path = dir.path().join("big.rs");
        let first = sound_echolocation(dir.path(), &path, &text, false).render_footer();
        let second = sound_echolocation(dir.path(), &path, &text, false).render_footer();
        assert_eq!(first, second);
        assert!(first.len() <= POD_FOOTER_MAX_BYTES, "{}", first.len());
        assert!(first.starts_with("\n\n[Pod ("), "{first}");
    }

    #[test]
    fn prose_files_get_podmates_but_no_sounding() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("notes.txt"), "fn not_rust() {}\n").expect("fixture");
        let footer = sound_echolocation(
            dir.path(),
            &dir.path().join("notes.txt"),
            "fn not_rust() {}\n",
            false,
        )
        .render_footer();
        assert!(footer.contains("[Pod ("), "{footer}");
        assert!(!footer.contains("[Sound:"), "{footer}");
    }

    #[test]
    fn unlistable_non_source_renders_empty() {
        let chart = sound_echolocation(Path::new("."), Path::new("lonely"), "hello\n", false);
        assert_eq!(chart.render_footer(), "");
    }

    #[test]
    fn whole_footer_shape() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("lib.rs"), SAMPLE).expect("fixture");
        fs::write(dir.path().join("main.rs"), "fn main() {}\n").expect("fixture");
        let footer = sound_echolocation(dir.path(), &dir.path().join("lib.rs"), SAMPLE, false)
            .render_footer();
        assert!(footer.contains("12 symbols"), "{footer}");
        assert!(footer.contains("fn swim"), "{footer}");
        assert!(
            footer.contains("imports: crate, serde_json, std, super, tokio"),
            "{footer}"
        );
        assert!(footer.contains("lib.rs (this file), main.rs"), "{footer}");
    }

    #[test]
    fn match_pods_group_mark_and_sort() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::create_dir(dir.path().join("net")).expect("subdir");
        fs::create_dir(dir.path().join("store")).expect("subdir");
        for name in [
            "net/server.rs",
            "net/client.rs",
            "store/cache.rs",
            "main.rs",
        ] {
            fs::write(dir.path().join(name), "x").expect("fixture");
        }
        let vis = open_visibility(dir.path());
        let (pods, omitted) = sound_match_pods(
            dir.path(),
            &["store/cache.rs".to_string(), "net/server.rs".to_string()],
            &vis,
        );
        assert_eq!(omitted, 0);
        assert_eq!(pods.len(), 2);
        assert_eq!(pods[0].dir, "net");
        assert_eq!(pods[0].mates, ["client.rs", "server.rs (match)"]);
        assert_eq!(pods[1].dir, "store");
        assert_eq!(pods[1].mates, ["cache.rs (match)"]);
    }

    #[test]
    fn match_pods_cap_dirs_and_mates() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut files = Vec::new();
        for index in 0..12 {
            let sub = format!("pod-{index:02}");
            fs::create_dir(dir.path().join(&sub)).expect("subdir");
            for mate in 0..12 {
                fs::write(dir.path().join(&sub).join(format!("m{mate}.rs")), "x").expect("fixture");
            }
            files.push(format!("{sub}/m0.rs"));
        }
        let vis = open_visibility(dir.path());
        let (pods, omitted) = sound_match_pods(dir.path(), &files, &vis);
        assert_eq!(pods.len(), MAX_GREP_PODS);
        assert_eq!(omitted, 12 - MAX_GREP_PODS);
        assert_eq!(pods[0].mate_total, 12);
        assert!(pods[0].mates.last().expect("cap").starts_with("+4 more"));
    }

    #[test]
    fn match_pods_chart_matched_files_when_dir_unlistable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let vis = open_visibility(dir.path());
        let (pods, omitted) = sound_match_pods(
            dir.path(),
            &["gone/deep.rs".to_string(), "".to_string()],
            &vis,
        );
        assert_eq!(omitted, 0);
        assert_eq!(pods.len(), 1);
        assert_eq!(pods[0].dir, "gone");
        assert_eq!(pods[0].mates, ["deep.rs (match)"]);
    }

    #[test]
    fn match_pods_honor_excludes_and_includes() {
        let dir = tempfile::tempdir().expect("tempdir");
        for name in ["keep.rs", "skip.log", "notes.md"] {
            fs::write(dir.path().join(name), "x").expect("fixture");
        }
        let open = open_visibility(dir.path());
        let exclude = ["*.log".to_string()];
        let vis = PodVisibility {
            exclude: &exclude,
            ..open
        };
        let (pods, _) = sound_match_pods(dir.path(), &["keep.rs".to_string()], &vis);
        assert_eq!(pods[0].mates, ["keep.rs (match)", "notes.md"]);
        let include = ["*.rs".to_string()];
        let vis = PodVisibility {
            include: &include,
            ..open
        };
        let (pods, _) = sound_match_pods(dir.path(), &["keep.rs".to_string()], &vis);
        assert_eq!(pods[0].mates, ["keep.rs (match)"]);
    }

    #[test]
    fn match_pods_honor_gitignore_stacking() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join(".gitignore"), "ignored.txt\n").expect("fixture");
        for name in ["ignored.txt", "keep.txt"] {
            fs::write(dir.path().join(name), "x").expect("fixture");
        }
        let open = open_visibility(dir.path());
        let vis = PodVisibility {
            gitignore: true,
            ..open
        };
        let (pods, _) = sound_match_pods(dir.path(), &["keep.txt".to_string()], &vis);
        assert_eq!(pods[0].mates, ["keep.txt (match)"]);
    }

    #[test]
    fn match_pods_hide_denied_siblings() {
        let dir = tempfile::tempdir().expect("tempdir");
        let secret = dir.path().join("secret.txt");
        for name in ["open.txt", "secret.txt"] {
            fs::write(dir.path().join(name), "x").expect("fixture");
        }
        let prior = crate::sandbox::read_guard::active();
        crate::sandbox::read_guard::set_active(crate::sandbox::read_guard::ReadDenylist::build(
            true,
            &[secret],
            &[],
        ));
        let vis = open_visibility(dir.path());
        let (pods, _) = sound_match_pods(dir.path(), &["open.txt".to_string()], &vis);
        crate::sandbox::read_guard::set_active((*prior).clone());
        assert_eq!(pods[0].mates, ["open.txt (match)"]);
    }

    #[test]
    fn key_files_sound_rust_and_skip_the_rest() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("main.rs"), "fn main() {}\nfn serve() {}\n").expect("fixture");
        fs::write(dir.path().join("notes.md"), "# hi\n").expect("fixture");
        let (soundings, omitted) =
            sound_key_files(dir.path(), &["notes.md".to_string(), "main.rs".to_string()]);
        assert_eq!(omitted, 0);
        assert_eq!(soundings.len(), 1);
        assert_eq!(
            soundings.get("main.rs").map(String::as_str),
            Some("2 symbols: fn main:1, fn serve:2")
        );
    }

    #[test]
    fn links_resolve_crate_super_self_and_mod_edges() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        for path in [
            "src/main.rs",
            "src/config.rs",
            "src/net/mod.rs",
            "src/net/server.rs",
            "src/net/http.rs",
        ] {
            let full = root.join(path);
            fs::create_dir_all(full.parent().expect("parent")).expect("mkdir");
            fs::write(&full, "x\n").expect("fixture");
        }
        // crate:: + super:: + external (skipped) + missing (skipped).
        let text = "use crate::config::Config;\n\
             use crate::net::http::Handler;\n\
             use super::server::serve;\n\
             use tokio::fs::File;\n\
             use crate::missing::Thing;\n";
        let (links, total) =
            sound_links(root, &root.join("src/net/http.rs"), text, SoundLang::Rust);
        // crate::net::http resolves to the file itself: dropped as a self-link.
        assert_eq!(total, 2);
        assert_eq!(links, ["src/config.rs", "src/net/server.rs"]);
        // self:: + mod decl from a mod.rs.
        let (links, _) = sound_links(
            root,
            &root.join("src/net/mod.rs"),
            "use self::http::Handler;\nmod server;\nmod absent;\n",
            SoundLang::Rust,
        );
        assert_eq!(links, ["src/net/http.rs", "src/net/server.rs"]);
        // mod decl from a plain file resolves under <stem>/.
        fs::create_dir_all(root.join("src/net/http")).expect("mkdir");
        fs::write(root.join("src/net/http/router.rs"), "x\n").expect("fixture");
        let (links, _) = sound_links(
            root,
            &root.join("src/net/http.rs"),
            "mod router;\nmod nope;\n",
            SoundLang::Rust,
        );
        assert_eq!(links, ["src/net/http/router.rs"]);
    }

    #[test]
    fn links_skip_self_and_inline_mods() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src")).expect("mkdir");
        fs::write(root.join("src/main.rs"), "fn main() {}\n").expect("fixture");
        fs::write(root.join("src/config.rs"), "pub struct Config;\n").expect("fixture");
        // `use crate::config` from config.rs itself: self-link dropped.
        // Inline `mod tests {` is a symbol, never an edge.
        let (links, total) = sound_links(
            root,
            &root.join("src/config.rs"),
            "use crate::config::Config;\nmod tests {\n}\n",
            SoundLang::Rust,
        );
        assert_eq!(total, 0);
        assert!(links.is_empty());
    }

    #[test]
    fn links_render_in_footer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src")).expect("mkdir");
        fs::write(root.join("src/main.rs"), "fn main() {}\n").expect("fixture");
        let footer = sound_echolocation(
            root,
            &root.join("src/main.rs"),
            "use crate::config::Config;\nfn main() {}\n",
            false,
        )
        .render_footer();
        // config.rs does not exist: imports show, links stay silent.
        assert!(footer.contains("imports: crate"), "{footer}");
        assert!(!footer.contains("links:"), "{footer}");
        fs::write(root.join("src/config.rs"), "pub struct Config;\n").expect("fixture");
        let footer = sound_echolocation(
            root,
            &root.join("src/main.rs"),
            "use crate::config::Config;\nfn main() {}\n",
            false,
        )
        .render_footer();
        assert!(footer.contains("links: config.rs"), "{footer}");
    }

    #[test]
    fn shadowed_import_roots_drop_by_stem_and_package_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let text = "import a\nimport pkg\n";
        fs::write(root.join("main.py"), text).expect("fixture");
        fs::write(root.join("a.py"), "x = 1\n").expect("fixture");
        fs::create_dir(root.join("pkg")).expect("mkdir");
        fs::write(root.join("pkg/__init__.py"), "y = 2\n").expect("fixture");
        let footer = sound_echolocation(root, &root.join("main.py"), text, false).render_footer();
        // Both roots are named by their links: file stem (`a` under
        // `a.py`) and package path (`pkg` under `pkg/__init__.py`).
        // Sounded paths are `/`-separated on every OS; vacuous on
        // unix, load-bearing on Windows CI.
        assert!(!footer.contains("imports:"), "{footer}");
        assert!(footer.contains("links: a.py, pkg/__init__.py"), "{footer}");
        assert!(!footer.contains('\\'), "{footer}");
    }

    #[test]
    fn key_files_cap_with_omitted_count() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut keys = Vec::new();
        for index in 0..12 {
            let name = format!("k{index:02}.rs");
            fs::write(dir.path().join(&name), "fn f() {}\n").expect("fixture");
            keys.push(name);
        }
        let (soundings, omitted) = sound_key_files(dir.path(), &keys);
        assert_eq!(soundings.len(), MAX_MAP_SOUNDINGS);
        assert_eq!(omitted, 12 - MAX_MAP_SOUNDINGS);
        let again = sound_key_files(dir.path(), &keys);
        assert_eq!(again, (soundings, omitted));
    }

    #[test]
    fn callers_are_exact_reverse_links() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src/net")).expect("mkdir");
        fs::write(root.join("src/main.rs"), "fn main() {}\n").expect("fixture");
        fs::write(
            root.join("src/net/mod.rs"),
            "pub mod server;\npub mod client;\n",
        )
        .expect("fixture");
        fs::write(
            root.join("src/net/server.rs"),
            "use crate::config::Config;\nfn serve() {}\n",
        )
        .expect("fixture");
        fs::write(
            root.join("src/net/client.rs"),
            "use super::server::serve;\nfn connect() {}\n",
        )
        .expect("fixture");
        let (callers, total, omitted) = sound_callers(root, &root.join("src/net/server.rs"));
        assert_eq!(omitted, 0);
        assert_eq!(total, 2);
        assert_eq!(callers, ["src/net/client.rs", "src/net/mod.rs"]);
        for n in 0..=MAX_CALLER_SCAN_FILES {
            fs::write(root.join(format!("src/net/z{n:03}.rs")), "fn spare() {}\n")
                .expect("fixture");
        }
        let (bounded, bounded_total, omitted) =
            sound_callers(root, &root.join("src/net/server.rs"));
        assert_eq!(bounded, callers);
        assert_eq!(bounded_total, total);
        assert_eq!(omitted, 3);
    }

    #[test]
    fn callers_stay_inside_the_pod() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src/net")).expect("mkdir");
        fs::write(root.join("src/main.rs"), "fn main() {}\n").expect("fixture");
        fs::write(root.join("src/config.rs"), "pub struct Config;\n").expect("fixture");
        fs::write(
            root.join("src/net/server.rs"),
            "use crate::config::Config;\nfn serve() {}\n",
        )
        .expect("fixture");
        // server.rs links to config.rs, but lives in another pod:
        // same-pod callers stay silent rather than half-answer.
        let (callers, total, omitted) = sound_callers(root, &root.join("src/config.rs"));
        assert_eq!(omitted, 0);
        assert_eq!(total, 0);
        assert!(callers.is_empty());
    }

    #[test]
    fn buzz_adds_heard_by_and_stays_deterministic() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src/net")).expect("mkdir");
        fs::write(root.join("src/main.rs"), "fn main() {}\n").expect("fixture");
        fs::write(root.join("src/net/mod.rs"), "pub mod server;\n").expect("fixture");
        let text = "fn serve() {}\n";
        fs::write(root.join("src/net/server.rs"), text).expect("fixture");
        let plain =
            sound_echolocation(root, &root.join("src/net/server.rs"), text, false).render_footer();
        assert!(!plain.contains("heard by"), "{plain}");
        let first =
            sound_echolocation(root, &root.join("src/net/server.rs"), text, true).render_footer();
        let second =
            sound_echolocation(root, &root.join("src/net/server.rs"), text, true).render_footer();
        assert_eq!(first, second);
        assert!(first.contains("heard by: mod.rs"), "{first}");
    }

    #[test]
    fn echo_gates_parse_off_entries_and_ignore_noise() {
        assert!(parse_echo_gates("").is_empty());
        assert!(parse_echo_gates("mates").is_empty());
        let gates = parse_echo_gates("off:links, off:mates ,bogus,off:,off:links");
        assert_eq!(
            gates.into_iter().collect::<Vec<_>>(),
            ["links".to_string(), "mates".to_string()]
        );
    }

    const PY_SAMPLE: &str = r#"import os, sys as system
from pkg.sub import thing
from . import sibling
from ..parent import helper
import localmod

"""Module docstring mentioning def ghost."""

class Whale:
    """Docstring with class Phantom inside."""

    def swim(self):
        pass

    async def dive(self):
        pass

def surface():
    # def sunk:
    return 1

define_not_a_keyword = True
"#;

    #[test]
    fn python_sounds_defs_classes_and_imports() {
        let symbols = sound_symbols(PY_SAMPLE, SoundLang::Python);
        assert_eq!(
            symbols,
            [
                "class Whale:9",
                "def swim:12",
                "def dive:15",
                "def surface:18",
            ]
        );
        let roots: Vec<String> = sound_import_roots(PY_SAMPLE, SoundLang::Python)
            .into_iter()
            .collect();
        assert_eq!(roots, ["localmod", "os", "parent", "pkg", "sibling", "sys"]);
    }

    #[test]
    fn python_links_resolve_relative_and_absolute() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        for path in [
            "app.py",
            "util.py",
            "pkg/__init__.py",
            "pkg/mod.py",
            "pkg/sub/__init__.py",
            "pkg/sub/deep.py",
        ] {
            let full = root.join(path);
            fs::create_dir_all(full.parent().expect("parent")).expect("mkdir");
            fs::write(&full, "x\n").expect("fixture");
        }
        // Absolute: same dir wins; package shape at root.
        let (links, _) = sound_links(
            root,
            &root.join("app.py"),
            "import util\nfrom pkg.sub import thing\nimport os\n",
            SoundLang::Python,
        );
        assert_eq!(links, ["pkg/sub/__init__.py", "util.py"]);
        // Relative: `from . import sib` + `from .. import x`.
        let (links, _) = sound_links(
            root,
            &root.join("pkg/sub/deep.py"),
            "from . import sib\nfrom .. import mod\nfrom ... import nowhere\n",
            SoundLang::Python,
        );
        assert_eq!(links, ["pkg/mod.py"]);
    }

    const JS_SAMPLE: &str = r#"import React from 'react';
import { serve } from "./server";
import '@scope/pkg/sub';
import './setup';
const fs = require("fs");
const dynamic = await import("./chunk");

export async function swim() {}
export default class Whale {}
const dive = (x) => x;
let data = 5;
type Alias = string;
interface Shape {}
enum Color { Red }

if (x) { y(); }
"#;

    #[test]
    fn js_sounds_functions_classes_and_imports() {
        let symbols = sound_symbols(JS_SAMPLE, SoundLang::JavaScript);
        assert!(
            symbols.contains(&"function swim:8".to_string()),
            "{symbols:?}"
        );
        assert!(
            symbols.contains(&"class Whale:9".to_string()),
            "{symbols:?}"
        );
        assert!(
            symbols.contains(&"const dive:10".to_string()),
            "{symbols:?}"
        );
        assert!(
            symbols.contains(&"type Alias:12".to_string()),
            "{symbols:?}"
        );
        assert!(
            symbols.contains(&"interface Shape:13".to_string()),
            "{symbols:?}"
        );
        assert!(
            symbols.contains(&"enum Color:14".to_string()),
            "{symbols:?}"
        );
        assert!(!symbols.iter().any(|s| s.contains("data")), "{symbols:?}");
        let roots: Vec<String> = sound_import_roots(JS_SAMPLE, SoundLang::JavaScript)
            .into_iter()
            .collect();
        assert_eq!(
            roots,
            ["@scope/pkg", "chunk", "fs", "react", "server", "setup"]
        );
    }

    #[test]
    fn js_links_resolve_relative_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src/ui")).expect("mkdir");
        for path in ["src/app.ts", "src/server.ts", "src/ui/index.tsx"] {
            fs::write(root.join(path), "x\n").expect("fixture");
        }
        let (links, _) = sound_links(
            root,
            &root.join("src/app.ts"),
            "import { s } from './server';\nimport { U } from './ui';\nimport _ from 'lodash';\n",
            SoundLang::JavaScript,
        );
        assert_eq!(links, ["src/server.ts", "src/ui/index.tsx"]);
    }

    const GO_SAMPLE: &str = r#"package net

import (
    "fmt"
)

import "os"
import alias "example.com/foo/bar"
import _ "example.com/foo/side"

func Serve() {}
func (s *Server) Handle() {}
func (s Server) Close() {}
type Server struct{}
type (
    Grouped int
)

func main() {
    fmt.Println("hi")
    return "literal"
}
"#;

    #[test]
    fn go_sounds_funcs_methods_types_and_imports() {
        let symbols = sound_symbols(GO_SAMPLE, SoundLang::Go);
        assert!(
            symbols.contains(&"func Serve:11".to_string()),
            "{symbols:?}"
        );
        assert!(
            symbols.contains(&"func Server.Handle:12".to_string()),
            "{symbols:?}"
        );
        assert!(
            symbols.contains(&"func Server.Close:13".to_string()),
            "{symbols:?}"
        );
        assert!(
            symbols.contains(&"type Server:14".to_string()),
            "{symbols:?}"
        );
        assert!(
            !symbols.iter().any(|s| s.contains("Grouped")),
            "{symbols:?}"
        );
        let roots: Vec<String> = sound_import_roots(GO_SAMPLE, SoundLang::Go)
            .into_iter()
            .collect();
        assert_eq!(roots, ["bar", "fmt", "os", "side"]);
        // Go names packages, not files: no links by design.
        let dir = tempfile::tempdir().expect("tempdir");
        let (links, total) = sound_links(
            dir.path(),
            &dir.path().join("x.go"),
            GO_SAMPLE,
            SoundLang::Go,
        );
        assert_eq!(total, 0);
        assert!(links.is_empty());
    }

    #[test]
    fn detect_lang_routes_by_extension() {
        assert_eq!(detect_lang(Path::new("a.rs")), Some(SoundLang::Rust));
        assert_eq!(detect_lang(Path::new("a.py")), Some(SoundLang::Python));
        assert_eq!(detect_lang(Path::new("a.pyi")), Some(SoundLang::Python));
        assert_eq!(detect_lang(Path::new("a.ts")), Some(SoundLang::JavaScript));
        assert_eq!(detect_lang(Path::new("a.tsx")), Some(SoundLang::JavaScript));
        assert_eq!(detect_lang(Path::new("a.jsx")), Some(SoundLang::JavaScript));
        assert_eq!(detect_lang(Path::new("a.mjs")), Some(SoundLang::JavaScript));
        assert_eq!(detect_lang(Path::new("a.go")), Some(SoundLang::Go));
        assert_eq!(detect_lang(Path::new("a.txt")), None);
        assert_eq!(detect_lang(Path::new("Makefile")), None);
    }

    #[test]
    fn scanners_survive_adversarial_text() {
        let long_line = "x".repeat(1_000_000);
        let nasties = [
            "\0\0\0",
            "fn \u{feff}\u{200b}\u{202e}evil() {}",
            "/* unterminated",
            "\"\"\"unterminated doc",
            "def f(:\n  pass",
            "use \u{1f980}::x;",
            "import \u{1f980}",
            "func (((((",
            "/* /* /* nested",
            "'''\"\"\"'\"\" mixed",
            "#\u{0}def fake():",
            "\r\ndef crlf():\r\n",
            "type (",
            "export default",
            "from",
            "import",
            long_line.as_str(),
        ];
        for lang in [
            SoundLang::Rust,
            SoundLang::Python,
            SoundLang::JavaScript,
            SoundLang::Go,
        ] {
            for nasty in nasties {
                let first = sound_symbols(nasty, lang);
                let second = sound_symbols(nasty, lang);
                assert_eq!(first, second, "{lang:?} {nasty:?}");
                let _ = sound_import_roots(nasty, lang);
            }
        }
    }

    #[test]
    fn sounding_stays_bounded_and_fast_on_huge_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let big = "fn f() {}\n".repeat(200_000);
        let path = dir.path().join("big.rs");
        fs::write(&path, &big).expect("fixture");
        let start = Instant::now();
        let first = sound_echolocation(dir.path(), &path, &big, true).render_footer();
        let second = sound_echolocation(dir.path(), &path, &big, true).render_footer();
        assert_eq!(first, second);
        assert!(first.len() <= POD_FOOTER_MAX_BYTES, "{}", first.len());
        // Absurdly generous: catches hangs and super-linear blowups,
        // not machine speed (measured separately for the thesis).
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "{:?}",
            start.elapsed()
        );
    }

    #[test]
    fn sounding_is_stable_on_real_repo_files() {
        // Byte-stability on the real thing (not just fixtures), with
        // wall-time reported for cost analysis (`--nocapture`).
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut sounded = 0;
        for rel in [
            "src/core/engine/turn_loop.rs",
            "src/tools/spec.rs",
            "src/tools/echolocation.rs",
        ] {
            let path = root.join(rel);
            let text = fs::read_to_string(&path).expect("repo file");
            let first = sound_echolocation(root, &path, &text, true).render_footer();
            let second = sound_echolocation(root, &path, &text, true).render_footer();
            assert_eq!(first, second, "{rel}");
            assert!(first.len() <= POD_FOOTER_MAX_BYTES, "{rel}");
            sounded += 1;
        }
        assert_eq!(sounded, 3);
    }

    #[test]
    fn non_rust_footer_renders_full_sounding() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("swim.py"), PY_SAMPLE).expect("fixture");
        fs::write(dir.path().join("other.py"), "x = 1\n").expect("fixture");
        let footer = sound_echolocation(dir.path(), &dir.path().join("swim.py"), PY_SAMPLE, false)
            .render_footer();
        assert!(footer.contains("[Pod ("), "{footer}");
        assert!(footer.contains("def swim"), "{footer}");
        assert!(footer.contains("imports:"), "{footer}");
    }

    #[test]
    fn crate_root_self_and_mod_edges_resolve_beside_the_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(
            dir.path().join("lib.rs"),
            "mod pod;\nuse self::song::Sing;\n",
        )
        .expect("fixture");
        fs::write(dir.path().join("pod.rs"), "fn swim() {}\n").expect("fixture");
        fs::create_dir(dir.path().join("song")).expect("mkdir");
        fs::write(dir.path().join("song/mod.rs"), "fn sing() {}\n").expect("fixture");
        let text = fs::read_to_string(dir.path().join("lib.rs")).expect("read");
        let (links, _) = sound_links(
            dir.path(),
            &dir.path().join("lib.rs"),
            &text,
            SoundLang::Rust,
        );
        assert!(links.contains(&"pod.rs".to_string()), "{links:?}");
        assert!(links.contains(&"song/mod.rs".to_string()), "{links:?}");
    }

    #[test]
    fn earliest_quote_wins_the_specifier() {
        assert_eq!(
            js_import_spec("import x from \"mod\"; foo('bar')"),
            Some("mod".to_string())
        );
        assert_eq!(
            js_import_spec("import x from 'mod'; foo(\"bar\")"),
            Some("mod".to_string())
        );
        assert_eq!(
            js_import_spec("import 'side-effect'"),
            Some("side-effect".to_string())
        );
    }

    #[test]
    fn require_opener_needs_a_boundary() {
        // `myrequire(` forges nothing …
        assert_eq!(js_import_spec("const x = myrequire('a')"), None);
        // … nor does a call spelled inside a string …
        assert_eq!(js_import_spec("const s = \"require('a')\""), None);
        // … but a real call later on the same line still sounds …
        assert_eq!(
            js_import_spec("myrequire('a'); const x = require('b')"),
            Some("b".to_string())
        );
        // … as does a computed argument followed by a real one.
        assert_eq!(
            js_import_spec("require(name); require('c')"),
            Some("c".to_string())
        );
    }

    #[test]
    fn digit_led_tokens_are_never_names() {
        assert!(sound_symbols("fn 1x() {}\nmod 2y;\n", SoundLang::Rust).is_empty());
        assert!(sound_symbols("def 2y():\n", SoundLang::Python).is_empty());
        assert!(sound_import_roots("use 123;\n", SoundLang::Rust).is_empty());
        assert_eq!(
            sound_symbols("fn fine() {}\n", SoundLang::Rust),
            ["fn fine:1"]
        );
    }

    #[test]
    fn minified_runs_never_blow_up_a_symbol() {
        let run = "a".repeat(10_000);
        let symbols = sound_symbols(&format!("fn {run}() {{}}\n"), SoundLang::Rust);
        assert_eq!(symbols.len(), 1);
        assert!(symbols[0].len() <= MAX_NAME_LEN + 8, "{}", symbols[0].len());
        let js = sound_symbols(&format!("function {run}() {{}}\n"), SoundLang::JavaScript);
        assert!(js.is_empty(), "{js:?}");
    }

    #[test]
    fn buzz_read_of_a_crowded_pod_stays_fast() {
        // Worst-case shape: 200 mates × 50KB each, all re-scanned for
        // callers on every buzz read. The 5s bound is orders past the
        // expected tens of milliseconds — it trips only on pathological
        // regression, never on a loaded machine.
        let dir = tempfile::tempdir().expect("tempdir");
        let body = "fn swim() {}\n".repeat(4_000);
        for i in 0..200 {
            fs::write(dir.path().join(format!("mate{i:03}.rs")), &body).expect("fixture");
        }
        let target = dir.path().join("target.rs");
        fs::write(&target, "fn target() {}\n").expect("fixture");
        let text = fs::read_to_string(&target).expect("read");
        let start = Instant::now();
        let chart = sound_echolocation(dir.path(), &target, &text, true);
        let footer = chart.render_footer();
        let elapsed = start.elapsed();
        assert!(footer.len() <= POD_FOOTER_MAX_BYTES);
        assert!(
            elapsed < Duration::from_secs(5),
            "buzz read took {elapsed:?}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn pod_listing_skips_symlinks_into_denied_trees() {
        // The denylist filter is distinct from the dotfile skip: a
        // non-dot symlink into a denied tree must not enumerate. Needs a
        // real ~/.ssh to resolve against; honest skip otherwise (the
        // read-level twin is
        // `read_file_refuses_a_workspace_symlink_pointing_at_a_denied_tree`).
        let _env_lock = crate::test_support::lock_test_env();
        let Some(home) = dirs::home_dir() else {
            return;
        };
        if !home.join(".ssh").is_dir() {
            return;
        }
        let dir = tempfile::tempdir().expect("tempdir");
        std::os::unix::fs::symlink(home.join(".ssh"), dir.path().join("vault_link"))
            .expect("symlink");
        fs::write(dir.path().join("app.rs"), "fn main() {}\n").expect("fixture");
        let entries = list_dir_entries(dir.path()).expect("listed");
        let raws: Vec<&str> = entries.iter().map(|(raw, _)| raw.as_str()).collect();
        assert!(raws.contains(&"app.rs"), "{raws:?}");
        assert!(!raws.contains(&"vault_link"), "{raws:?}");
    }

    #[test]
    fn block_comments_mute_and_release_across_lines() {
        let text = "/*\nfn fake() {}\n*/\nfn real() {}\n";
        let symbols = sound_symbols(text, SoundLang::Rust);
        assert_eq!(symbols, ["fn real:4"]);
        // A comment opener inside a string still mutes until the closer:
        // the failure mode is a miss, never a forge.
        let text = "let s = \"/*\";\nfn muted() {}\nlet t = \"*/\";\nfn kept() {}\n";
        let symbols = sound_symbols(text, SoundLang::Rust);
        assert_eq!(symbols, ["fn kept:4"]);
    }

    #[test]
    fn py_decorators_methods_and_comment_fences() {
        let text = "@route(\"/x\")\ndef handler():\n    pass\nclass Pod:\n    def swim(self):\n        pass\n# a \"\"\" in a comment toggles nothing\ndef after():\n";
        let symbols = sound_symbols(text, SoundLang::Python);
        assert!(
            symbols.contains(&"def handler:2".to_string()),
            "{symbols:?}"
        );
        assert!(symbols.contains(&"class Pod:4".to_string()), "{symbols:?}");
        assert!(symbols.contains(&"def swim:5".to_string()), "{symbols:?}");
        assert!(symbols.contains(&"def after:8".to_string()), "{symbols:?}");
    }

    #[test]
    fn js_anonymous_and_arrow_shapes() {
        // Anonymous default export: no name to sound, no phantom.
        assert!(sound_symbols("export default function() {}\n", SoundLang::JavaScript).is_empty());
        let symbols = sound_symbols(
            "const swim = () => {};\nlet dive = async () => {};\nvar data = 42;\n",
            SoundLang::JavaScript,
        );
        assert!(symbols.contains(&"const swim:1".to_string()), "{symbols:?}");
        assert!(symbols.contains(&"let dive:2".to_string()), "{symbols:?}");
        assert!(!symbols.iter().any(|s| s.contains("data")), "{symbols:?}");
    }

    #[test]
    fn edit_echo_names_touched_symbol_and_callers() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("a.py"), "import b\nprint(b.x)\n").expect("fixture");
        let before = "x = 1\ndef swim():\n    return x\n";
        let after = "x = 1\ndef swim():\n    return x + 1\n";
        fs::write(dir.path().join("b.py"), after).expect("fixture");
        let echo =
            sound_edit_echo(dir.path(), &dir.path().join("b.py"), before, after).expect("echo");
        assert!(echo.contains("touched def swim:2"), "{echo}");
        assert!(echo.contains("heard by a.py"), "{echo}");
    }

    #[test]
    fn edit_echo_attributes_body_edits_to_enclosing_symbol() {
        let dir = tempfile::tempdir().expect("tempdir");
        // No mates: touched only, no heard-by half.
        let before = "fn main() {\n    let x = 1;\n    println!(\"{x}\");\n}\nfn other() {}\n";
        let after = "fn main() {\n    let x = 2;\n    println!(\"{x}\");\n}\nfn other() {}\n";
        fs::write(dir.path().join("solo.rs"), after).expect("fixture");
        let echo =
            sound_edit_echo(dir.path(), &dir.path().join("solo.rs"), before, after).expect("echo");
        assert_eq!(echo, "[Edit echo: touched fn main:1]");
        let deleted = touched_symbols(
            &dir.path().join("solo.rs"),
            "fn survivor() {}\nfn removed() {}\n",
            "fn survivor() {}\n",
        );
        assert_eq!(deleted, (vec!["fn removed:2".to_string()], 1));
        let replaced = touched_symbols(
            &dir.path().join("solo.rs"),
            "fn removed() {}\n",
            "fn added() {}\n",
        );
        assert_eq!(
            replaced,
            (
                vec!["fn removed:1".to_string(), "fn added:1".to_string()],
                2
            )
        );
    }

    #[test]
    fn edit_echo_silent_when_nothing_to_say() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("note.txt"), "hi\n").expect("fixture");
        // Identical: no mutation, no echo.
        assert!(
            sound_edit_echo(dir.path(), &dir.path().join("note.txt"), "hi\n", "hi\n").is_none()
        );
        // Unknown language, no mates: neither half can sound.
        assert!(
            sound_edit_echo(dir.path(), &dir.path().join("note.txt"), "hi\n", "yo\n").is_none()
        );
    }

    #[test]
    fn edit_echo_new_file_names_first_symbols() {
        let dir = tempfile::tempdir().expect("tempdir");
        let after = "fn alpha() {}\nfn beta() {}\n";
        fs::write(dir.path().join("fresh.rs"), after).expect("fixture");
        let echo =
            sound_edit_echo(dir.path(), &dir.path().join("fresh.rs"), "", after).expect("echo");
        assert!(echo.contains("touched fn alpha:1, fn beta:2"), "{echo}");
    }

    #[test]
    fn unlistable_pod_fallback_skips_dotfiles_like_the_listed_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (pods, _) = sound_match_pods(
            dir.path(),
            &["nodir/.env".to_string(), "nodir/app.rs".to_string()],
            &open_visibility(dir.path()),
        );
        assert_eq!(pods.len(), 1);
        assert!(
            pods[0].mates.contains(&"app.rs (match)".to_string()),
            "{:?}",
            pods[0].mates
        );
        assert!(
            !pods[0].mates.iter().any(|mate| mate.contains(".env")),
            "{:?}",
            pods[0].mates
        );
    }
}
