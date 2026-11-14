use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use codewhale_net::guard::{GuardedGet, PrivateTargets};
use codewhale_net::network_policy::NetworkPolicy;
use codewhale_skill_install::bounded_tar::{
    entry_names_within_bound, map_stream_limit, open_bounded_tarball,
};
use codewhale_skill_install::install::is_safe_path;
use codewhale_util::utils::read_response_body_capped;
use regex::Regex;
use semver::{Version, VersionReq};
use serde::Deserialize;
use sha2::{Digest, Sha512};

use super::dsh::{Value, presets};

const NPM_REGISTRY: &str = "https://registry.npmjs.org";
const NPM_ACCEPT_HEADER: &str = "application/vnd.npm.install-v1+json, application/json";
const MAX_PACKUMENT_BYTES: usize = 32 * 1024 * 1024;
const MAX_TARBALL_BYTES: usize = 64 * 1024 * 1024;
const MAX_UNPACKED_PACKAGES: usize = 256;
const MAX_UNPACKED_FILES: usize = 14_000;
const MAX_UNPACKED_BYTES: u64 = 220 * 1024 * 1024;

type CachedTarballs = HashMap<(String, String), Arc<Vec<u8>>>;

static PACKUMENT_CACHE: OnceLock<Mutex<HashMap<String, Arc<Packument>>>> = OnceLock::new();
static TARBALL_CACHE: OnceLock<Mutex<CachedTarballs>> = OnceLock::new();

#[derive(Debug, Clone, Deserialize)]
struct PackumentDist {
    tarball: String,
    #[serde(default)]
    integrity: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct PackumentVersion {
    #[serde(default)]
    dependencies: BTreeMap<String, String>,
    dist: PackumentDist,
}

#[derive(Debug, Clone, Deserialize)]
struct Packument {
    #[serde(rename = "dist-tags", default)]
    dist_tags: BTreeMap<String, String>,
    #[serde(default)]
    versions: BTreeMap<String, serde_json::Value>,
}

fn packument_cache() -> &'static Mutex<HashMap<String, Arc<Packument>>> {
    PACKUMENT_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn tarball_cache() -> &'static Mutex<CachedTarballs> {
    TARBALL_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn is_host_package(name: &str) -> bool {
    name == "cosmokit" || name.starts_with("@deepseek-ai/")
}

fn is_valid_npm_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 214 {
        return false;
    }
    let check_part = |part: &str| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && !part.starts_with('.')
            && !part.starts_with('_')
            && part.bytes().all(|b| {
                b.is_ascii_lowercase()
                    || b.is_ascii_digit()
                    || matches!(b, b'-' | b'.' | b'_' | b'~')
            })
    };
    if let Some(rest) = name.strip_prefix('@') {
        let Some((scope, pkg)) = rest.split_once('/') else {
            return false;
        };
        !pkg.contains('/') && check_part(scope) && check_part(pkg)
    } else {
        !name.contains('/') && check_part(name)
    }
}

fn bare_package_name(specifier: &str) -> Option<String> {
    if specifier.is_empty()
        || specifier.starts_with('.')
        || specifier.starts_with('/')
        || specifier.contains(':')
    {
        return None;
    }
    let mut parts = specifier.split('/');
    if specifier.starts_with('@') {
        let scope = parts.next()?;
        let pkg = parts.next().filter(|s| !s.is_empty())?;
        Some(format!("{scope}/{pkg}"))
    } else {
        let pkg = parts.next().filter(|s| !s.is_empty())?;
        Some(pkg.to_string())
    }
}

fn declared_dependencies(manifest: &Value) -> BTreeMap<String, String> {
    let mut deps = BTreeMap::new();
    let Some(Value::Map(entries)) = manifest.get("dependencies") else {
        return deps;
    };
    for (name, val) in entries {
        if let Some(range) = val.str()
            && !is_host_package(name)
        {
            deps.insert(name.clone(), range.to_string());
        }
    }
    deps
}

fn visit_composition_entries(rows: &[Value], referenced: &mut BTreeSet<String>) {
    for row in rows {
        let is_group = row.get("group").is_some_and(Value::is_true)
            || row
                .get("name")
                .and_then(Value::str)
                .is_some_and(|n| matches!(n, "cordis:group" | "@deepseek-ai/cordis-plugin-group"));
        if is_group {
            if let Some(Value::Seq(children)) = row.get("config") {
                visit_composition_entries(children, referenced);
            }
            continue;
        }
        if let Some(name) = row.get("name").and_then(Value::str)
            && let Some(pkg) = bare_package_name(name)
        {
            referenced.insert(pkg);
        }
    }
}

fn normalize_branch_req(branch: &str) -> Result<Option<VersionReq>> {
    let trimmed = branch.trim();
    if trimmed.is_empty() || matches!(trimmed, "*" | "x" | "X" | "latest") {
        return Ok(None);
    }
    if let Some((lo, hi)) = trimmed.split_once(" - ") {
        let lo_tok = lo.trim().trim_start_matches(['v', 'V']);
        let hi_tok = hi.trim().trim_start_matches(['v', 'V']);
        let spec = format!(">={lo_tok}, <={hi_tok}");
        return Ok(Some(VersionReq::parse(&spec)?));
    }
    let normalized = trimmed.replace(',', " ");
    let raw_tokens: Vec<&str> = normalized.split_whitespace().collect();
    let mut comparators = Vec::new();
    let mut i = 0usize;
    while i < raw_tokens.len() {
        let mut tok = raw_tokens[i].to_string();
        if matches!(tok.as_str(), "=" | "<" | "<=" | ">" | ">=" | "^" | "~")
            && i + 1 < raw_tokens.len()
        {
            i += 1;
            tok.push_str(raw_tokens[i]);
        }
        i += 1;
        let op_len = if tok.starts_with("<=") || tok.starts_with(">=") {
            2
        } else if tok.starts_with(['<', '>', '=', '^', '~']) {
            1
        } else {
            0
        };
        let (op, rest) = tok.split_at(op_len);
        let rest = rest.trim_start_matches(['v', 'V']);
        let rest_stars: Vec<String> = rest
            .split('.')
            .map(|seg| {
                if matches!(seg, "x" | "X" | "*") {
                    "*".to_string()
                } else {
                    seg.to_string()
                }
            })
            .collect();
        let joined = rest_stars.join(".");
        if matches!(joined.as_str(), "*" | "*.*" | "*.*.*") {
            continue;
        }
        if op.is_empty() && !joined.contains('*') {
            comparators.push(format!("={joined}"));
        } else if matches!(op, "^" | "~") && joined.contains('*') {
            let trimmed_wild = joined.trim_end_matches(".*").to_string();
            comparators.push(format!("{op}{trimmed_wild}"));
        } else {
            comparators.push(format!("{op}{joined}"));
        }
    }
    if comparators.is_empty() {
        return Ok(None);
    }
    Ok(Some(VersionReq::parse(&comparators.join(", "))?))
}

fn version_matches_range(version: &Version, range: &str) -> Result<bool> {
    let trimmed = range.trim();
    if trimmed.is_empty() || matches!(trimmed, "*" | "x" | "X" | "latest") {
        return Ok(version.pre.is_empty());
    }
    for branch in trimmed.split("||") {
        match normalize_branch_req(branch) {
            Ok(None) => {
                if version.pre.is_empty() {
                    return Ok(true);
                }
            }
            Ok(Some(req)) => {
                if req.matches(version) {
                    return Ok(true);
                }
            }
            Err(_) => continue,
        }
    }
    Ok(false)
}

fn select_version(
    packument: &Packument,
    package_name: &str,
    range: &str,
) -> Result<(String, PackumentVersion)> {
    if let Some(latest_str) = packument.dist_tags.get("latest")
        && let Ok(latest_ver) = Version::parse(latest_str)
        && version_matches_range(&latest_ver, range).unwrap_or(false)
        && let Some(raw_entry) = packument.versions.get(latest_str)
        && let Ok(parsed) = serde_json::from_value::<PackumentVersion>(raw_entry.clone())
    {
        return Ok((latest_str.clone(), parsed));
    }
    let mut best_stable: Option<(Version, String, PackumentVersion)> = None;
    let mut best_pre: Option<(Version, String, PackumentVersion)> = None;
    for (ver_key, raw_entry) in &packument.versions {
        let Ok(ver) = Version::parse(ver_key) else {
            continue;
        };
        if !version_matches_range(&ver, range).unwrap_or(false) {
            continue;
        }
        let Ok(parsed) = serde_json::from_value::<PackumentVersion>(raw_entry.clone()) else {
            continue;
        };
        if ver.pre.is_empty() {
            if best_stable.as_ref().is_none_or(|(cur, _, _)| ver > *cur) {
                best_stable = Some((ver, ver_key.clone(), parsed));
            }
        } else if best_pre.as_ref().is_none_or(|(cur, _, _)| ver > *cur) {
            best_pre = Some((ver, ver_key.clone(), parsed));
        }
    }
    if let Some((_, key, parsed)) = best_stable.or(best_pre) {
        return Ok((key, parsed));
    }
    bail!("no npm version of `{package_name}` matches range `{range}`")
}

fn verify_tarball_integrity(
    integrity: Option<&str>,
    bytes: &[u8],
    pkg: &str,
    ver: &str,
) -> Result<()> {
    let raw = integrity
        .ok_or_else(|| anyhow::anyhow!("npm package `{pkg}@{ver}` omitted dist.integrity"))?;
    let expected = raw
        .split_whitespace()
        .find_map(|token| token.strip_prefix("sha512-"))
        .ok_or_else(|| {
            anyhow::anyhow!("npm package `{pkg}@{ver}` lacks a sha512 integrity digest")
        })?;
    let actual = base64::engine::general_purpose::STANDARD.encode(Sha512::digest(bytes));
    if actual != expected {
        bail!("npm package `{pkg}@{ver}` failed sha512 integrity verification");
    }
    Ok(())
}

fn validate_registry_tarball_url(tarball_url: &str, pkg: &str, ver: &str) -> Result<()> {
    let parsed = reqwest::Url::parse(tarball_url)
        .with_context(|| format!("invalid tarball URL for `{pkg}@{ver}`"))?;
    if parsed.scheme() != "https"
        || parsed.host_str() != Some("registry.npmjs.org")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        bail!("refusing npm tarball URL outside registry.npmjs.org for `{pkg}@{ver}`");
    }
    Ok(())
}

fn should_skip_npm_entry(rel: &Path) -> bool {
    let mut last_name = None;
    for part in rel.components() {
        let Component::Normal(os) = part else {
            return true;
        };
        let seg = os.to_string_lossy().to_ascii_lowercase();
        if matches!(
            seg.as_str(),
            "credentials"
                | "credentials.json"
                | "secrets.json"
                | "id_rsa"
                | "id_ed25519"
                | ".env"
                | ".npmrc"
                | ".netrc"
                | ".git"
        ) || seg.starts_with(".env.")
        {
            return true;
        }
        last_name = Some(seg);
    }
    let Some(lower) = last_name else {
        return true;
    };
    if lower.ends_with(".d.ts") || lower.ends_with(".d.mts") || lower.ends_with(".d.cts") {
        return true;
    }
    let ext = rel
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    matches!(
        ext.as_deref(),
        Some("node" | "pem" | "key" | "p12" | "pfx" | "map")
    )
}

fn strip_tar_package_prefix(raw_path: &Path) -> Option<PathBuf> {
    let mut comps = raw_path
        .components()
        .filter(|c| !matches!(c, Component::CurDir));
    match comps.next()? {
        Component::Normal(_) => {}
        _ => return None,
    }
    let rest: PathBuf = comps.collect();
    if rest.as_os_str().is_empty() || !is_safe_path(&rest) {
        return None;
    }
    let text = rest.to_string_lossy();
    if text.contains('\\') || text.contains(':') {
        return None;
    }
    Some(rest)
}

async fn fetch_packument(package_name: &str, network: &NetworkPolicy) -> Result<Arc<Packument>> {
    if !is_valid_npm_name(package_name) {
        bail!("invalid npm package name `{package_name}`");
    }
    if let Ok(guard) = packument_cache().lock()
        && let Some(cached) = guard.get(package_name)
    {
        return Ok(Arc::clone(cached));
    }
    let encoded = package_name.replace('/', "%2f");
    let url = format!("{NPM_REGISTRY}/{encoded}");
    let redirect_check = |next: &reqwest::Url| -> Result<()> {
        if next.scheme() != "https" || next.host_str() != Some("registry.npmjs.org") {
            bail!("unexpected npm registry redirect host");
        }
        Ok(())
    };
    let resp = GuardedGet::new(&url, network, "plugin_npm_deps", PrivateTargets::Refuse)
        .accept(NPM_ACCEPT_HEADER)
        .timeouts(Duration::from_secs(10), Duration::from_secs(45))
        .max_redirects(2)
        .redirect_check(&redirect_check)
        .send()
        .await
        .with_context(|| format!("failed to fetch npm metadata for `{package_name}`"))?
        .error_for_status()
        .with_context(|| format!("npm registry returned error for `{package_name}`"))?;
    let bytes = read_response_body_capped(resp, MAX_PACKUMENT_BYTES)
        .await
        .with_context(|| format!("failed to read npm metadata body for `{package_name}`"))?;
    let parsed: Packument = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse npm packument for `{package_name}`"))?;
    let arc = Arc::new(parsed);
    if let Ok(mut guard) = packument_cache().lock() {
        guard.insert(package_name.to_string(), Arc::clone(&arc));
    }
    Ok(arc)
}

async fn fetch_verified_tarball(
    package_name: &str,
    version: &str,
    dist: &PackumentDist,
    network: &NetworkPolicy,
) -> Result<Arc<Vec<u8>>> {
    let key = (package_name.to_string(), version.to_string());
    if let Ok(guard) = tarball_cache().lock()
        && let Some(cached) = guard.get(&key)
    {
        return Ok(Arc::clone(cached));
    }
    validate_registry_tarball_url(&dist.tarball, package_name, version)?;
    let redirect_check = |next: &reqwest::Url| -> Result<()> {
        if next.scheme() != "https" || next.host_str() != Some("registry.npmjs.org") {
            bail!("unexpected npm tarball redirect host");
        }
        Ok(())
    };
    let resp = GuardedGet::new(
        &dist.tarball,
        network,
        "plugin_npm_deps",
        PrivateTargets::Refuse,
    )
    .timeouts(Duration::from_secs(10), Duration::from_secs(60))
    .max_redirects(2)
    .redirect_check(&redirect_check)
    .send()
    .await
    .with_context(|| format!("failed to fetch tarball for `{package_name}@{version}`"))?
    .error_for_status()
    .with_context(|| format!("tarball download returned error for `{package_name}@{version}`"))?;
    let bytes = read_response_body_capped(resp, MAX_TARBALL_BYTES)
        .await
        .with_context(|| format!("failed to read tarball for `{package_name}@{version}`"))?;
    verify_tarball_integrity(dist.integrity.as_deref(), &bytes, package_name, version)?;
    let arc = Arc::new(bytes);
    if let Ok(mut guard) = tarball_cache().lock() {
        guard.insert(key, Arc::clone(&arc));
    }
    Ok(arc)
}

pub(crate) fn maybe_stage_with_dependencies(
    package_root: &Path,
    manifest: &Value,
    entries: &[Value],
    network: &NetworkPolicy,
) -> Result<Option<tempfile::TempDir>> {
    let declared = declared_dependencies(manifest);
    if declared.is_empty() {
        return Ok(None);
    }
    let root = package_root.to_path_buf();
    let entries = entries.to_vec();
    let network = network.clone();
    std::thread::spawn(move || -> Result<Option<tempfile::TempDir>> {
        fn scan_dir(
            base: &Path,
            cur: &Path,
            declared: &BTreeMap<String, String>,
            has_roster: bool,
            import_re: &Regex,
            referenced: &mut BTreeSet<String>,
        ) -> Result<()> {
            for entry in fs::read_dir(cur)? {
                let entry = entry?;
                let path = entry.path();
                let meta = fs::symlink_metadata(&path)?;
                if meta.file_type().is_symlink() {
                    continue;
                }
                let Ok(rel) = path.strip_prefix(base) else {
                    continue;
                };
                if meta.is_dir() {
                    if let Some(name) = path.file_name().and_then(|s| s.to_str())
                        && matches!(
                            name,
                            "node_modules"
                                | "artifacts"
                                | "vendor"
                                | "client"
                                | "web"
                                | "ui"
                                | "frontend"
                        )
                    {
                        continue;
                    }
                    scan_dir(base, &path, declared, has_roster, import_re, referenced)?;
                    continue;
                }
                if !meta.is_file() {
                    continue;
                }
                let lower_name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_ascii_lowercase();
                let ext = path
                    .extension()
                    .map(|e| e.to_string_lossy().to_ascii_lowercase());
                if has_roster && matches!(ext.as_deref(), Some("yml" | "yaml")) {
                    if let Ok(text) = fs::read_to_string(&path) {
                        for dep in declared.keys() {
                            if text.contains(dep.as_str()) {
                                referenced.insert(dep.clone());
                            }
                        }
                    }
                    continue;
                }
                if matches!(ext.as_deref(), Some("js" | "mjs" | "cjs"))
                    && !matches!(
                        lower_name.as_str(),
                        "client.js" | "client.mjs" | "client.cjs"
                    )
                    && !lower_name.starts_with("client-")
                    && !rel.components().any(|c| {
                        c.as_os_str().to_str().is_some_and(|s| {
                            matches!(
                                s,
                                "node_modules"
                                    | "artifacts"
                                    | "vendor"
                                    | "client"
                                    | "web"
                                    | "ui"
                                    | "frontend"
                            )
                        })
                    })
                    && let Ok(text) = fs::read_to_string(&path)
                {
                    for cap in import_re.captures_iter(&text) {
                        if let Some(spec) = cap.get(1).map(|m| m.as_str())
                            && let Some(pkg) = bare_package_name(spec)
                            && declared.contains_key(&pkg)
                        {
                            referenced.insert(pkg);
                        }
                    }
                }
            }
            Ok(())
        }

        fn copy_dir_no_symlinks(src: &Path, dst: &Path) -> Result<()> {
            fs::create_dir_all(dst)?;
            for entry in fs::read_dir(src)? {
                let entry = entry?;
                let src_path = entry.path();
                let dst_path = dst.join(entry.file_name());
                let meta = fs::symlink_metadata(&src_path)?;
                if meta.file_type().is_symlink() {
                    bail!("symlinks are not permitted in DSH packages");
                }
                if meta.is_dir() {
                    copy_dir_no_symlinks(&src_path, &dst_path)?;
                } else if meta.is_file() {
                    fs::copy(&src_path, &dst_path)?;
                }
            }
            Ok(())
        }

        fn unpack_tarball(
            bytes: &[u8],
            dest_dir: &Path,
            file_count: &mut usize,
            total_bytes: &mut u64,
        ) -> Result<()> {
            fs::create_dir_all(dest_dir)?;
            let mut archive = open_bounded_tarball(bytes, MAX_TARBALL_BYTES as u64);
            let iter = archive.entries().map_err(|err| {
                map_stream_limit(err.into(), || {
                    anyhow::anyhow!("npm tarball exceeded {MAX_TARBALL_BYTES} byte limit")
                })
            })?;
            for entry_res in iter {
                let mut entry = entry_res.map_err(|err| {
                    map_stream_limit(err.into(), || {
                        anyhow::anyhow!("npm tarball exceeded {MAX_TARBALL_BYTES} byte limit")
                    })
                })?;
                if !entry_names_within_bound(&entry) {
                    bail!("npm tarball entry path exceeds length bound");
                }
                if entry.header().entry_type().is_symlink()
                    || entry.header().entry_type().is_hard_link()
                {
                    bail!("npm tarball contains disallowed link entry");
                }
                if !entry.header().entry_type().is_file() {
                    continue;
                }
                let raw_path = entry
                    .path()
                    .context("invalid path inside npm tarball")?
                    .into_owned();
                let Some(rel) = strip_tar_package_prefix(&raw_path) else {
                    continue;
                };
                if should_skip_npm_entry(&rel) {
                    continue;
                }
                let target = dest_dir.join(&rel);
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                *file_count += 1;
                if *file_count > MAX_UNPACKED_FILES {
                    bail!("resolved npm dependencies exceed the {MAX_UNPACKED_FILES}-file budget");
                }
                let mut out = File::create(&target)?;
                let mut buf = [0u8; 8192];
                loop {
                    let n = std::io::Read::read(&mut entry, &mut buf).map_err(|err| {
                        map_stream_limit(err.into(), || {
                            anyhow::anyhow!("npm tarball exceeded {MAX_TARBALL_BYTES} byte limit")
                        })
                    })?;
                    if n == 0 {
                        break;
                    }
                    *total_bytes = total_bytes.saturating_add(n as u64);
                    if *total_bytes > MAX_UNPACKED_BYTES {
                        bail!(
                            "resolved npm dependencies exceed the {} MiB byte budget",
                            MAX_UNPACKED_BYTES / (1024 * 1024)
                        );
                    }
                    out.write_all(&buf[..n])?;
                }
            }
            Ok(())
        }

        let mut referenced = BTreeSet::new();
        visit_composition_entries(&entries, &mut referenced);
        let import_re = Regex::new(
            r#"(?:from\s+|import\s*\(\s*|require(?:\.resolve)?\s*\(\s*|import\s+)["']([^"']+)["']"#,
        )?;
        let has_roster = presets::has_roster(&entries);
        scan_dir(
            &root,
            &root,
            &declared,
            has_roster,
            &import_re,
            &mut referenced,
        )?;

        let mut seeds = Vec::new();
        for (name, range) in &declared {
            if referenced.contains(name)
                && !root
                    .join("node_modules")
                    .join(name)
                    .join("package.json")
                    .is_file()
            {
                seeds.push((root.clone(), name.clone(), range.clone()));
            }
        }
        if seeds.is_empty() {
            return Ok(None);
        }

        let stage = tempfile::Builder::new()
            .prefix("codewhale-dsh-npm-")
            .tempdir()?;
        let staged_root = stage.path().to_path_buf();
        copy_dir_no_symlinks(&root, &staged_root)?;

        let mut preexisting = HashSet::new();
        for name in declared.keys() {
            if staged_root
                .join("node_modules")
                .join(name)
                .join("package.json")
                .is_file()
            {
                preexisting.insert(name.clone());
            }
        }

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        let mut hoisted: HashMap<String, Version> = HashMap::new();
        let mut unpacked_at: HashSet<(PathBuf, String)> = HashSet::new();
        let mut package_count = 0usize;
        let mut file_count = 0usize;
        let mut total_bytes = 0u64;
        let mut queue: VecDeque<(PathBuf, String, String)> = seeds
            .into_iter()
            .map(|(_, name, range)| (staged_root.clone(), name, range))
            .collect();

        while let Some((parent_dir, name, range)) = queue.pop_front() {
            if is_host_package(&name) {
                continue;
            }
            let bundled_at_root = staged_root
                .join("node_modules")
                .join(&name)
                .join("package.json")
                .is_file();
            if parent_dir == staged_root && (preexisting.contains(&name) || bundled_at_root) {
                continue;
            }
            if let Some(installed_ver) = hoisted.get(&name)
                && version_matches_range(installed_ver, &range).unwrap_or(false)
            {
                continue;
            }
            let packument = rt.block_on(fetch_packument(&name, &network))?;
            let (resolved_ver_str, version_meta) = select_version(&packument, &name, &range)?;
            let resolved_ver = Version::parse(&resolved_ver_str)?;

            let target_dir =
                if !hoisted.contains_key(&name) && !preexisting.contains(&name) && !bundled_at_root
                {
                    hoisted.insert(name.clone(), resolved_ver);
                    staged_root.join("node_modules").join(&name)
                } else {
                    let nested = parent_dir.join("node_modules").join(&name);
                    if nested.join("package.json").is_file() {
                        continue;
                    }
                    nested
                };

            if !unpacked_at.insert((target_dir.clone(), resolved_ver_str.clone())) {
                continue;
            }
            package_count += 1;
            if package_count > MAX_UNPACKED_PACKAGES {
                bail!("resolved npm dependencies exceed the {MAX_UNPACKED_PACKAGES}-package limit");
            }
            let bytes = rt.block_on(fetch_verified_tarball(
                &name,
                &resolved_ver_str,
                &version_meta.dist,
                &network,
            ))?;
            unpack_tarball(&bytes, &target_dir, &mut file_count, &mut total_bytes)?;

            for (child_name, child_range) in version_meta.dependencies {
                if !is_host_package(&child_name) {
                    queue.push_back((target_dir.clone(), child_name, child_range));
                }
            }
        }

        Ok(Some(stage))
    })
    .join()
    .map_err(|_| anyhow::anyhow!("npm dependency resolver thread panicked"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_range_supports_or_space_and_exact_comparators() {
        let v319 = Version::parse("3.19.0").unwrap();
        let v400 = Version::parse("4.0.0").unwrap();
        let v510 = Version::parse("5.1.0").unwrap();
        assert!(version_matches_range(&v400, "^4.0.0 || ^5.0.0").unwrap());
        assert!(version_matches_range(&v510, "^4.0.0 || ^5.0.0").unwrap());
        assert!(!version_matches_range(&v319, "^4.0.0 || ^5.0.0").unwrap());
        assert!(version_matches_range(&v319, ">=3.18.1 <4.0.0").unwrap());
        assert!(!version_matches_range(&v400, ">=3.18.1 <4.0.0").unwrap());
        let v012 = Version::parse("0.12.0").unwrap();
        let v0121 = Version::parse("0.12.1").unwrap();
        assert!(version_matches_range(&v012, "0.12.0").unwrap());
        assert!(!version_matches_range(&v0121, "0.12.0").unwrap());
        let rc2 = Version::parse("0.2.0-rc.2").unwrap();
        assert!(version_matches_range(&rc2, "^0.1.7-rc.2 || ^0.2.0-rc.1 || ^0.2.0-rc.2").unwrap());
        assert!(version_matches_range(&v400, "*").unwrap());
    }

    #[test]
    fn verify_integrity_accepts_sha512_and_rejects_mismatch() {
        let payload = b"hello npm tarball";
        let digest = base64::engine::general_purpose::STANDARD.encode(Sha512::digest(payload));
        let header = format!("sha512-{digest}");
        verify_tarball_integrity(Some(&header), payload, "demo", "1.0.0").unwrap();
        assert!(verify_tarball_integrity(Some(&header), b"tampered", "demo", "1.0.0").is_err());
        assert!(verify_tarball_integrity(None, payload, "demo", "1.0.0").is_err());
    }

    #[test]
    fn seed_selection_skips_client_artifacts_and_unimported_bindings() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("lib")).unwrap();
        fs::create_dir_all(dir.path().join("client")).unwrap();
        fs::create_dir_all(dir.path().join("artifacts")).unwrap();
        fs::write(
            dir.path().join("lib/index.js"),
            "import { z } from 'zod';\nimport '@deepseek-ai/dsh-tools';\nexport function apply() { return z; }\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("lib/client-widget.js"),
            "import React from 'react';\nexport default React;\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("client/app.js"),
            "import katex from 'katex';\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("artifacts/bundle.js"),
            "require('@univerjs-pro/engine-formula-rust-binding');\n",
        )
        .unwrap();
        let no_zod_manifest = Value::Map(vec![(
            "dependencies".to_string(),
            Value::Map(vec![
                ("react".to_string(), Value::Str("^18.0.0".to_string())),
                ("katex".to_string(), Value::Str("0.16.0".to_string())),
                (
                    "@univerjs-pro/engine-formula-rust-binding".to_string(),
                    Value::Str("1.0.3".to_string()),
                ),
                (
                    "@deepseek-ai/schemastery".to_string(),
                    Value::Str("^3.18.0".to_string()),
                ),
                ("cosmokit".to_string(), Value::Str("^1.0.0".to_string())),
            ]),
        )]);
        assert!(
            maybe_stage_with_dependencies(
                dir.path(),
                &no_zod_manifest,
                &[],
                &NetworkPolicy::default()
            )
            .unwrap()
            .is_none()
        );

        let manifest = Value::Map(vec![(
            "dependencies".to_string(),
            Value::Map(vec![
                ("zod".to_string(), Value::Str("^4.0.0".to_string())),
                ("react".to_string(), Value::Str("^18.0.0".to_string())),
                ("katex".to_string(), Value::Str("0.16.0".to_string())),
                (
                    "@univerjs-pro/engine-formula-rust-binding".to_string(),
                    Value::Str("1.0.3".to_string()),
                ),
                (
                    "@deepseek-ai/schemastery".to_string(),
                    Value::Str("^3.18.0".to_string()),
                ),
                ("cosmokit".to_string(), Value::Str("^1.0.0".to_string())),
            ]),
        )]);
        let declared = declared_dependencies(&manifest);
        assert!(declared.contains_key("zod"));
        assert!(!declared.contains_key("@deepseek-ai/schemastery"));
        assert!(!declared.contains_key("cosmokit"));

        let mut referenced = BTreeSet::new();
        let re = Regex::new(
            r#"(?:from\s+|import\s*\(\s*|require(?:\.resolve)?\s*\(\s*|import\s+)["']([^"']+)["']"#,
        )
        .unwrap();
        let text = fs::read_to_string(dir.path().join("lib/index.js")).unwrap();
        for cap in re.captures_iter(&text) {
            if let Some(pkg) = bare_package_name(&cap[1])
                && declared.contains_key(&pkg)
            {
                referenced.insert(pkg);
            }
        }
        assert_eq!(referenced, BTreeSet::from(["zod".to_string()]));
    }
}
