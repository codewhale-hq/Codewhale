use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const DESIGNS_DIR: &str = "designs";
pub const TOKENS_CSS: &str = include_str!("../../../vendor/codewhale-design/tokens.css");
pub const GEIST_SANS: &[u8] = include_bytes!("../../../web/public/brand/fonts/Geist-Variable.woff2");
pub const GEIST_MONO: &[u8] =
    include_bytes!("../../../web/public/brand/fonts/GeistMono-Variable.woff2");

const MAX_FRAGMENT_BYTES: usize = 4 * 1024 * 1024;
const MAX_ASSET_BYTES: u64 = 10 * 1024 * 1024;
const MAX_ARTBOARD_EDGE: u32 = 8192;
const ARTBOARD_GAP: i64 = 80;
const SCREENSHOT_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Debug, thiserror::Error)]
pub enum DesignError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Io(String),
}

impl From<std::io::Error> for DesignError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err.to_string())
    }
}

pub type DesignResult<T> = Result<T, DesignError>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Artboard {
    pub id: String,
    pub name: String,
    pub x: i64,
    pub y: i64,
    pub width: u32,
    pub height: u32,
    #[serde(default = "default_theme")]
    pub theme: String,
    pub rev: u64,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Comment {
    pub id: String,
    pub artboard_id: String,
    pub x: f64,
    pub y: f64,
    pub author: String,
    pub body: String,
    #[serde(default)]
    pub resolved: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DesignDoc {
    pub version: u32,
    pub id: String,
    pub name: String,
    pub rev: u64,
    pub tokens: String,
    pub artboards: Vec<Artboard>,
    #[serde(default)]
    pub comments: Vec<Comment>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesignSummary {
    pub id: String,
    pub name: String,
    pub rev: u64,
    pub artboards: usize,
    pub open_comments: usize,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteMode {
    Replace,
    Append,
}

#[derive(Debug, Clone, Default)]
pub struct NewArtboard {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub theme: Option<String>,
    pub x: Option<i64>,
    pub y: Option<i64>,
    pub html: String,
    pub source_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteReport {
    pub artboard: Option<Artboard>,
    pub bytes: usize,
    pub assets_copied: Vec<String>,
    pub refused_references: Vec<String>,
    pub remote_references: usize,
}

fn default_theme() -> String {
    "dark".to_string()
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !id.starts_with('-')
}

pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for ch in name.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
        if out.len() >= 48 {
            break;
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() { "artboard".to_string() } else { out }
}

fn require_id(kind: &str, id: &str) -> DesignResult<()> {
    if valid_id(id) {
        Ok(())
    } else {
        Err(DesignError::Invalid(format!(
            "{kind} id `{id}` must be 1-64 lowercase letters, digits or dashes"
        )))
    }
}

pub struct DesignStore {
    root: PathBuf,
}

impl DesignStore {
    pub fn for_workspace(workspace: &Path) -> Self {
        Self {
            root: codewhale_util::files::workspace_state_dir(workspace).join(DESIGNS_DIR),
        }
    }

    pub fn at(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn doc_dir(&self, doc: &str) -> DesignResult<PathBuf> {
        require_id("design", doc)?;
        Ok(self.root.join(doc))
    }

    fn doc_file(&self, doc: &str) -> DesignResult<PathBuf> {
        Ok(self.doc_dir(doc)?.join("design.json"))
    }

    fn fragment_file(&self, doc: &str, artboard: &str) -> DesignResult<PathBuf> {
        require_id("artboard", artboard)?;
        Ok(self.doc_dir(doc)?.join("artboards").join(format!("{artboard}.html")))
    }

    pub fn shot_file(&self, doc: &str, artboard: &str) -> DesignResult<PathBuf> {
        require_id("artboard", artboard)?;
        Ok(self.doc_dir(doc)?.join("shots").join(format!("{artboard}.png")))
    }

    pub fn list(&self) -> DesignResult<Vec<DesignSummary>> {
        let mut out = Vec::new();
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(err) => return Err(err.into()),
        };
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if !valid_id(&name) {
                continue;
            }
            if let Ok(doc) = self.load(&name) {
                out.push(DesignSummary {
                    open_comments: doc.comments.iter().filter(|c| !c.resolved).count(),
                    artboards: doc.artboards.len(),
                    id: doc.id,
                    name: doc.name,
                    rev: doc.rev,
                    updated_at: doc.updated_at,
                });
            }
        }
        out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(out)
    }

    pub fn load(&self, doc: &str) -> DesignResult<DesignDoc> {
        let path = self.doc_file(doc)?;
        let raw = match std::fs::read(&path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Err(DesignError::NotFound(format!("design `{doc}` does not exist")));
            }
            Err(err) => return Err(err.into()),
        };
        serde_json::from_slice(&raw)
            .map_err(|err| DesignError::Io(format!("design `{doc}` is unreadable: {err}")))
    }

    pub fn load_or_create(&self, doc: &str, name: Option<&str>) -> DesignResult<DesignDoc> {
        match self.load(doc) {
            Ok(found) => Ok(found),
            Err(DesignError::NotFound(_)) => {
                let created = DesignDoc {
                    version: 1,
                    id: doc.to_string(),
                    name: name.map(str::to_string).unwrap_or_else(|| doc.to_string()),
                    rev: 0,
                    tokens: "codewhale-design".to_string(),
                    artboards: Vec::new(),
                    comments: Vec::new(),
                    updated_at: now(),
                };
                self.install_base_assets(doc)?;
                self.save(&created)?;
                Ok(created)
            }
            Err(err) => Err(err),
        }
    }

    fn install_base_assets(&self, doc: &str) -> DesignResult<()> {
        let fonts = self.doc_dir(doc)?.join("assets").join("fonts");
        std::fs::create_dir_all(&fonts)?;
        std::fs::write(fonts.join("Geist-Variable.woff2"), GEIST_SANS)?;
        std::fs::write(fonts.join("GeistMono-Variable.woff2"), GEIST_MONO)?;
        Ok(())
    }

    fn save(&self, doc: &DesignDoc) -> DesignResult<()> {
        let path = self.doc_file(&doc.id)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        let body = serde_json::to_vec_pretty(doc)
            .map_err(|err| DesignError::Io(format!("could not encode design: {err}")))?;
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn read_fragment(&self, doc: &str, artboard: &str) -> DesignResult<String> {
        let loaded = self.load(doc)?;
        find(&loaded, artboard)?;
        let path = self.fragment_file(doc, artboard)?;
        match std::fs::read_to_string(&path) {
            Ok(text) => Ok(text),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(err) => Err(err.into()),
        }
    }

    pub fn create_artboard(&self, doc: &str, spec: NewArtboard) -> DesignResult<WriteReport> {
        if spec.width == 0 || spec.height == 0 || spec.width > MAX_ARTBOARD_EDGE || spec.height > MAX_ARTBOARD_EDGE {
            return Err(DesignError::Invalid(format!(
                "artboard size must be between 1 and {MAX_ARTBOARD_EDGE} px on each edge"
            )));
        }
        let mut loaded = self.load_or_create(doc, None)?;
        let base = slugify(&spec.name);
        let mut id = base.clone();
        let mut n = 2;
        while loaded.artboards.iter().any(|a| a.id == id) {
            id = format!("{base}-{n}");
            n += 1;
        }
        let x = spec.x.unwrap_or_else(|| {
            loaded
                .artboards
                .iter()
                .map(|a| a.x + i64::from(a.width) + ARTBOARD_GAP)
                .max()
                .unwrap_or(0)
        });
        let y = spec.y.unwrap_or(0);
        let mut report = WriteReport::default();
        let html = self.localize(doc, &spec.html, spec.source_dir.as_deref(), &mut report)?;
        check_fragment(&html)?;
        let fragment = self.fragment_file(doc, &id)?;
        if let Some(parent) = fragment.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&fragment, &html)?;
        let stamp = now();
        let artboard = Artboard {
            id,
            name: spec.name,
            x,
            y,
            width: spec.width,
            height: spec.height,
            theme: spec.theme.unwrap_or_else(default_theme),
            rev: 1,
            updated_at: stamp.clone(),
        };
        loaded.artboards.push(artboard.clone());
        loaded.rev += 1;
        loaded.updated_at = stamp;
        self.save(&loaded)?;
        report.bytes = html.len();
        report.artboard = Some(artboard);
        Ok(report)
    }

    pub fn write_html(
        &self,
        doc: &str,
        artboard: &str,
        html: &str,
        mode: WriteMode,
        source_dir: Option<&Path>,
    ) -> DesignResult<WriteReport> {
        let mut loaded = self.load(doc)?;
        let index = find(&loaded, artboard)?;
        let mut report = WriteReport::default();
        let incoming = self.localize(doc, html, source_dir, &mut report)?;
        let next = match mode {
            WriteMode::Replace => incoming,
            WriteMode::Append => append_into_root(&self.read_fragment(doc, artboard)?, &incoming),
        };
        check_fragment(&next)?;
        std::fs::write(self.fragment_file(doc, artboard)?, &next)?;
        let stamp = now();
        let board = &mut loaded.artboards[index];
        board.rev += 1;
        board.updated_at = stamp.clone();
        let board = board.clone();
        loaded.rev += 1;
        loaded.updated_at = stamp;
        self.save(&loaded)?;
        report.bytes = next.len();
        report.artboard = Some(board);
        Ok(report)
    }

    pub fn move_artboard(&self, doc: &str, artboard: &str, x: i64, y: i64) -> DesignResult<Artboard> {
        let mut loaded = self.load(doc)?;
        let index = find(&loaded, artboard)?;
        let stamp = now();
        let board = &mut loaded.artboards[index];
        board.x = x;
        board.y = y;
        board.updated_at = stamp.clone();
        let board = board.clone();
        loaded.rev += 1;
        loaded.updated_at = stamp;
        self.save(&loaded)?;
        Ok(board)
    }

    pub fn add_comment(
        &self,
        doc: &str,
        artboard: &str,
        x: f64,
        y: f64,
        author: &str,
        body: &str,
    ) -> DesignResult<Comment> {
        let body = body.trim();
        if body.is_empty() || body.len() > 8000 {
            return Err(DesignError::Invalid("comment body must be 1-8000 bytes".to_string()));
        }
        let mut loaded = self.load(doc)?;
        find(&loaded, artboard)?;
        let stamp = now();
        let comment = Comment {
            id: uuid::Uuid::new_v4().simple().to_string()[..12].to_string(),
            artboard_id: artboard.to_string(),
            x: x.clamp(0.0, 1.0),
            y: y.clamp(0.0, 1.0),
            author: author.chars().take(80).collect(),
            body: body.to_string(),
            resolved: false,
            created_at: stamp.clone(),
        };
        loaded.comments.push(comment.clone());
        loaded.rev += 1;
        loaded.updated_at = stamp;
        self.save(&loaded)?;
        Ok(comment)
    }

    pub fn set_comment_resolved(&self, doc: &str, comment: &str, resolved: bool) -> DesignResult<Comment> {
        let mut loaded = self.load(doc)?;
        let Some(found) = loaded.comments.iter_mut().find(|c| c.id == comment) else {
            return Err(DesignError::NotFound(format!("comment `{comment}` does not exist")));
        };
        found.resolved = resolved;
        let found = found.clone();
        loaded.rev += 1;
        loaded.updated_at = now();
        self.save(&loaded)?;
        Ok(found)
    }

    pub fn asset_path(&self, doc: &str, relative: &str) -> DesignResult<PathBuf> {
        let assets = self.doc_dir(doc)?.join("assets");
        let mut path = assets.clone();
        for part in relative.split('/') {
            if part.is_empty()
                || part == "."
                || part == ".."
                || part.contains('\\')
                || part.contains('\0')
                || part.starts_with('.')
            {
                return Err(DesignError::Invalid(format!("asset path `{relative}` is not allowed")));
            }
            path.push(part);
        }
        Ok(path)
    }

    pub fn compose_document(&self, doc: &str, artboard: &str, base: &str, csp_source: &str) -> DesignResult<String> {
        let loaded = self.load(doc)?;
        let board = &loaded.artboards[find(&loaded, artboard)?];
        let fragment = self.read_fragment(doc, artboard)?;
        Ok(compose(board, &fragment, base, csp_source))
    }

    pub async fn screenshot(&self, doc: &str, artboard: &str, scale: f32) -> DesignResult<PathBuf> {
        let dir = self.doc_dir(doc)?;
        let base = url_for_dir(&dir);
        let html = self.compose_document(doc, artboard, &base, "file:")?;
        let loaded = self.load(doc)?;
        let board = loaded.artboards[find(&loaded, artboard)?].clone();
        let shots = dir.join("shots");
        std::fs::create_dir_all(&shots)?;
        let page = shots.join(format!("{artboard}.render.html"));
        std::fs::write(&page, html)?;
        let out = self.shot_file(doc, artboard)?;
        let _ = std::fs::remove_file(&out);
        let browser = find_chromium().ok_or_else(|| {
            DesignError::Io(
                "no Chromium-family browser found for headless rendering; set CODEWHALE_CHROMIUM to a Chrome, Chromium or Edge binary"
                    .to_string(),
            )
        })?;
        let profile = tempfile::tempdir()?;
        let mut cmd = tokio::process::Command::new(&browser);
        cmd.arg("--headless=new")
            .arg("--disable-gpu")
            .arg("--hide-scrollbars")
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--disable-extensions")
            .arg("--disable-background-networking")
            .arg("--disable-sync")
            .arg("--disable-component-update")
            .arg("--disable-features=Translate,MediaRouter,OptimizationHints")
            .arg("--blink-settings=scriptEnabled=false")
            .arg("--host-resolver-rules=MAP * ~NOTFOUND")
            .arg("--proxy-server=127.0.0.1:9")
            .arg("--proxy-bypass-list=<-loopback>")
            .arg(format!("--user-data-dir={}", profile.path().display()))
            .arg(format!("--force-device-scale-factor={}", scale.clamp(0.25, 3.0)))
            .arg(format!("--window-size={},{}", board.width, board.height))
            .arg(format!("--screenshot={}", out.display()))
            .arg(url_for_file(&page))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let child = cmd.spawn()?;
        let output = tokio::time::timeout(SCREENSHOT_TIMEOUT, child.wait_with_output())
            .await
            .map_err(|_| DesignError::Io("headless render timed out".to_string()))??;
        if !out.is_file() {
            let tail: String = String::from_utf8_lossy(&output.stderr)
                .lines()
                .rev()
                .take(4)
                .collect::<Vec<_>>()
                .join(" | ");
            return Err(DesignError::Io(format!(
                "headless render produced no image (exit {:?}): {tail}",
                output.status.code()
            )));
        }
        Ok(out)
    }

    fn localize(
        &self,
        doc: &str,
        html: &str,
        source_dir: Option<&Path>,
        report: &mut WriteReport,
    ) -> DesignResult<String> {
        let mut out = String::with_capacity(html.len());
        let mut rest = html;
        while let Some(start) = rest.find("file://") {
            out.push_str(&rest[..start]);
            let tail = &rest[start..];
            let end = tail
                .find(|c: char| c == '"' || c == '\'' || c == ')' || c.is_whitespace() || c == '<' || c == '>')
                .unwrap_or(tail.len());
            let url = &tail[..end];
            match self.copy_local_asset(doc, url, source_dir) {
                Ok(name) => {
                    out.push_str(&format!("assets/{name}"));
                    if !report.assets_copied.contains(&name) {
                        report.assets_copied.push(name);
                    }
                }
                Err(reason) => {
                    report.refused_references.push(format!("{url}: {reason}"));
                    out.push_str("about:blank");
                }
            }
            rest = &tail[end..];
        }
        out.push_str(rest);
        report.remote_references = out.matches("http://").count() + out.matches("https://").count();
        Ok(out)
    }

    fn copy_local_asset(&self, doc: &str, url: &str, source_dir: Option<&Path>) -> Result<String, String> {
        let raw = url.trim_start_matches("file://");
        let decoded = percent_decode(raw);
        let path = PathBuf::from(&decoded);
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        if !matches!(ext.as_str(), "svg" | "png" | "jpg" | "jpeg" | "gif" | "webp" | "avif" | "woff2" | "woff" | "ttf" | "otf") {
            return Err("only image and font files are copied into a design".to_string());
        }
        let canonical = path.canonicalize().map_err(|_| "file does not exist".to_string())?;
        let Some(allowed) = source_dir.and_then(|d| d.canonicalize().ok()) else {
            return Err("local files are only copied when the HTML comes from a file inside the workspace".to_string());
        };
        let fonts_ok = matches!(ext.as_str(), "woff2" | "woff" | "ttf" | "otf")
            && canonical
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("Geist"));
        if fonts_ok {
            let name = canonical.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            return Ok(format!("fonts/{name}"));
        }
        if !canonical.starts_with(&allowed) {
            return Err(format!("outside {}", allowed.display()));
        }
        let meta = std::fs::metadata(&canonical).map_err(|e| e.to_string())?;
        if !meta.is_file() || meta.len() > MAX_ASSET_BYTES {
            return Err("not a regular file under 10 MiB".to_string());
        }
        let bytes = std::fs::read(&canonical).map_err(|e| e.to_string())?;
        let digest = Sha256::digest(&bytes);
        let stem = canonical
            .file_stem()
            .and_then(|s| s.to_str())
            .map(slugify)
            .unwrap_or_else(|| "asset".to_string());
        let name = format!("{stem}-{}.{ext}", hex_prefix(&digest, 6));
        let target = self.asset_path(doc, &name).map_err(|e| e.to_string())?;
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        if !target.exists() {
            std::fs::write(&target, &bytes).map_err(|e| e.to_string())?;
        }
        Ok(name)
    }
}

fn find(doc: &DesignDoc, artboard: &str) -> DesignResult<usize> {
    require_id("artboard", artboard)?;
    doc.artboards
        .iter()
        .position(|a| a.id == artboard)
        .ok_or_else(|| DesignError::NotFound(format!("artboard `{artboard}` is not in design `{}`", doc.id)))
}

fn check_fragment(html: &str) -> DesignResult<()> {
    if html.len() > MAX_FRAGMENT_BYTES {
        return Err(DesignError::Invalid("artboard HTML is larger than 4 MiB".to_string()));
    }
    Ok(())
}

fn append_into_root(existing: &str, incoming: &str) -> String {
    let trimmed = existing.trim_end();
    if trimmed.is_empty() {
        return incoming.to_string();
    }
    match trimmed.rfind("</") {
        Some(at) if trimmed[at..].ends_with('>') && !trimmed[at..].contains(' ') => {
            format!("{}{}{}", &trimmed[..at], incoming, &trimmed[at..])
        }
        _ => format!("{trimmed}{incoming}"),
    }
}

pub fn compose(board: &Artboard, fragment: &str, base: &str, csp_source: &str) -> String {
    let csp = format!(
        "default-src 'none'; style-src 'unsafe-inline' {csp_source}; img-src {csp_source} data:; font-src {csp_source} data:; script-src 'none'; frame-src 'none'; form-action 'none'; base-uri {csp_source}"
    );
    format!(
        "<!doctype html><html data-theme=\"{theme}\"><head><meta charset=\"utf-8\">\
<meta http-equiv=\"Content-Security-Policy\" content=\"{csp}\">\
<base href=\"{base}\">\
<style>\
@font-face{{font-family:'Geist';src:url('assets/fonts/Geist-Variable.woff2') format('woff2');font-weight:100 900;}}\
@font-face{{font-family:'Geist Sans';src:url('assets/fonts/Geist-Variable.woff2') format('woff2');font-weight:100 900;}}\
@font-face{{font-family:'Geist Mono';src:url('assets/fonts/GeistMono-Variable.woff2') format('woff2');font-weight:100 900;}}\
{tokens}\
html,body{{margin:0;padding:0;width:{w}px;height:{h}px;overflow:hidden;}}\
</style></head><body>{fragment}</body></html>",
        theme = html_attr(&board.theme),
        base = html_attr(base),
        tokens = TOKENS_CSS.replace("</", "<\\/"),
        w = board.width,
        h = board.height,
    )
}

fn html_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn hex_prefix(bytes: &[u8], n: usize) -> String {
    bytes.iter().take(n).map(|b| format!("{b:02x}")).collect()
}

fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Some(v) = raw.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn url_for_file(path: &Path) -> String {
    let display = path.to_string_lossy().replace('\\', "/");
    let encoded: String = display
        .chars()
        .map(|c| match c {
            ' ' => "%20".to_string(),
            '#' => "%23".to_string(),
            '?' => "%3F".to_string(),
            '%' => "%25".to_string(),
            c => c.to_string(),
        })
        .collect();
    if encoded.starts_with('/') {
        format!("file://{encoded}")
    } else {
        format!("file:///{encoded}")
    }
}

fn url_for_dir(path: &Path) -> String {
    let mut url = url_for_file(path);
    if !url.ends_with('/') {
        url.push('/');
    }
    url
}

pub fn find_chromium() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("CODEWHALE_CHROMIUM") {
        let path = PathBuf::from(explicit);
        return path.is_file().then_some(path);
    }
    let fixed = [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
        "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe",
    ];
    for candidate in fixed {
        let path = PathBuf::from(candidate);
        if path.is_file() {
            return Some(path);
        }
    }
    let names = ["chromium", "chromium-browser", "google-chrome", "google-chrome-stable", "microsoft-edge"];
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        for name in names {
            let path = dir.join(name);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, DesignStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = DesignStore::at(dir.path().join("designs"));
        (dir, store)
    }

    #[test]
    fn artboards_round_trip_and_place_left_to_right() {
        let (_dir, store) = store();
        let first = store
            .create_artboard("shell", NewArtboard { name: "Shell Dark".into(), width: 1440, height: 900, html: "<div id=\"ab\"></div>".into(), ..Default::default() })
            .unwrap();
        let second = store
            .create_artboard("shell", NewArtboard { name: "Shell Dark".into(), width: 400, height: 300, html: String::new(), ..Default::default() })
            .unwrap();
        let first = first.artboard.unwrap();
        let second = second.artboard.unwrap();
        assert_eq!(first.id, "shell-dark");
        assert_eq!(second.id, "shell-dark-2");
        assert_eq!(second.x, 1440 + ARTBOARD_GAP);
        store.write_html("shell", "shell-dark", "<p>hi</p>", WriteMode::Append, None).unwrap();
        assert_eq!(store.read_fragment("shell", "shell-dark").unwrap(), "<div id=\"ab\"><p>hi</p></div>");
        let doc = store.load("shell").unwrap();
        assert_eq!(doc.rev, 3);
        assert_eq!(doc.artboards[0].rev, 2);
        assert!(store.doc_dir("shell").unwrap().join("assets/fonts/Geist-Variable.woff2").is_file());
    }

    #[test]
    fn local_assets_copy_only_from_the_source_tree() {
        let (dir, store) = store();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("whale.svg"), "<svg/>").unwrap();
        let outside = dir.path().join("secret.svg");
        std::fs::write(&outside, "<svg/>").unwrap();
        let html = format!(
            "<img src=\"file://{}\"><img src=\"file://{}\"><img src=\"https://x.test/a.png\">",
            src.join("whale.svg").display(),
            outside.display()
        );
        let report = store
            .create_artboard("d", NewArtboard { name: "a".into(), width: 10, height: 10, html, source_dir: Some(src), ..Default::default() })
            .unwrap();
        assert_eq!(report.assets_copied.len(), 1);
        assert_eq!(report.refused_references.len(), 1);
        assert_eq!(report.remote_references, 1);
        let fragment = store.read_fragment("d", "a").unwrap();
        assert!(fragment.contains("src=\"assets/whale-"));
        assert!(!fragment.contains("file://"));
    }

    #[test]
    fn ids_and_asset_paths_are_confined() {
        let (_dir, store) = store();
        assert!(store.doc_dir("../x").is_err());
        assert!(store.asset_path("d", "../design.json").is_err());
        assert!(store.asset_path("d", "fonts/../../x").is_err());
        assert!(store.asset_path("d", "fonts/Geist-Variable.woff2").is_ok());
    }

    #[test]
    fn composed_document_blocks_scripts_and_network() {
        let board = Artboard { id: "a".into(), name: "a".into(), x: 0, y: 0, width: 10, height: 20, theme: "dark\"><script>".into(), rev: 1, updated_at: String::new() };
        let html = compose(&board, "<p>x</p>", "artboard://d/", "artboard:");
        assert!(html.contains("script-src 'none'"));
        assert!(html.contains("default-src 'none'"));
        assert!(!html.contains("dark\"><script>"));
    }
}
