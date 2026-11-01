//! Raw roster import joins existing Native entries; no writable root or agent store.
use super::*;

pub(super) const AGENT_PRESETS: &str = "@deepseek-ai/dsh-agent-presets";
pub const MARKER: &str = "// codewhale-native-preset-v1 ";

/// Exact contained package lookup. No upward node_modules walk, exports glob,
/// install script, require fallback or package-selected resolver is executed.
pub fn contained_module(package: &Package, name: &str) -> Result<PathBuf> {
    if name.starts_with("./") {
        return package.contained(Path::new(name));
    }
    let parts: Vec<_> = name.split('/').collect();
    let count = if name.starts_with('@') { 2 } else { 1 };
    require(
        parts.len() >= count
            && parts
                .iter()
                .all(|p| !p.is_empty() && !matches!(*p, "." | ".."))
            && !name.chars().any(|c| c <= ' ' || matches!(c, '\\' | ':')),
        "Bare module must name one contained package export.",
    )?;
    let identity = parts[..count].join("/");
    let root = format!("node_modules/{identity}");
    let manifest = package.contained(Path::new(&format!("{root}/package.json")))?;
    let data = parse_json(&utf8(read_file(&manifest, MAX_DOCUMENT)?)?)?;
    require(
        data.get("name").and_then(Value::str) == Some(identity.as_str())
            && data.get("type").and_then(Value::str) == Some("module"),
        "Bare module requires an exact contained ESM package identity.",
    )?;
    fn target(value: &Value) -> Result<Option<&str>> {
        match value {
            Value::Str(text) => Ok(Some(text)),
            Value::Map(items) => {
                for (key, child) in items {
                    if matches!(key.as_str(), "node" | "import" | "default")
                        && let Some(picked) = target(child)?
                    {
                        return Ok(Some(picked));
                    }
                }
                Ok(None)
            }
            _ => refuse(
                "Null/array/non-string package targets are refused; a blocked export must not fall through to another condition.",
            ),
        }
    }
    let subpath = if parts.len() == count {
        ".".into()
    } else {
        format!("./{}", parts[count..].join("/"))
    };
    let selected = match data.get("exports") {
        Some(Value::Map(items)) if items.iter().any(|(key, _)| key.starts_with('.')) => match items.iter().find(|(key,_)|key==&subpath) { Some((_,value))=>target(value)?,None=>None },
        Some(value) if subpath == "." => target(value)?,
        Some(_) => None,
        None if subpath == "." => Some(data.get("main").and_then(Value::str).unwrap_or("./index.js")),
        None => Some(subpath.as_str()),
    }.ok_or_else(|| anyhow::Error::msg("Bare package export is not admitted; conditional arrays/globs/require-only exports are refused."))?;
    let selected = selected
        .strip_prefix("./")
        .filter(|path| plain_relative(path))
        .ok_or_else(|| anyhow::Error::msg("Package export must be one plain contained path."))?;
    package.contained(Path::new(&format!("{root}/{selected}")))
}

pub(super) fn has_roster(entries: &[Value]) -> bool {
    entries.iter().any(|row| {
        row.get("name").and_then(Value::str) == Some(AGENT_PRESETS)
            || row
                .get("config")
                .is_some_and(|children| matches!(children, Value::Seq(rows) if has_roster(rows)))
    })
}

/// Runs the same nonexecuting reviewer as ordinary composition import, over
/// contained documents and directories already covered by the source inventory.
pub(super) fn prepare(
    package: &Package,
    document: &Json,
    output: &mut OutputFiles,
) -> Result<Vec<String>> {
    let mut directories = Vec::new();
    let mut documents = Vec::new();
    walk_files(&package.root, |path, is_dir| {
        let relative = relative_slash(path, &package.root);
        if is_dir {
            directories.push(relative);
            return Ok(());
        }
        if !matches!(
            path.file_name().and_then(|p| p.to_str()),
            Some("package.json" | "preset.yml" | "agent.cordis.yml")
        ) {
            return Ok(());
        }
        let source = utf8(read_file(
            &package.contained(Path::new(&relative))?,
            MAX_DOCUMENT,
        )?)?;
        documents
            .push(json!({"path":relative,"sha256":sha256_hex(source.as_bytes()),"source":source}));
        Ok(())
    })?;
    let review = crate::composition_review::review(&json!({"kind":"agent-presets","composition":document,"documents":documents,"directories":directories})).map_err(anyhow::Error::msg)?;
    let catalog = review
        .get("catalog")
        .filter(|c| c.get("version") == Some(&json!(1)))
        .ok_or_else(|| anyhow::Error::msg("Preset reviewer omitted its catalog."))?;
    let prepared = review
        .get("presets")
        .and_then(Json::as_array)
        .filter(|p| !p.is_empty() && p.len() <= 64)
        .ok_or_else(|| anyhow::Error::msg("Preset reviewer returned no usable bounded entries."))?;
    let mut catalog = catalog.clone();
    let mut entries = Vec::new();
    for item in prepared {
        let mut metadata = item
            .get("metadata")
            .cloned()
            .ok_or_else(|| anyhow::Error::msg("Preset reviewer omitted metadata."))?;
        let id = metadata
            .get("id")
            .and_then(Json::as_str)
            .filter(|id| id.len() <= 64 && pattern(r"^[a-z0-9][a-z0-9-]*$").is_match(id))
            .ok_or_else(|| anyhow::Error::msg("Preset reviewer returned an invalid id."))?;
        let entry = format!("native/presets/{id}.mjs");
        let config = format!("native/presets/{id}.json");
        // The first line is inert data; Rust binds these display/default facts
        // to the exact existing EntryRef bytes instead of another metadata store.
        let source = format!(
            "{MARKER}{}\nimport {{ mountReviewedPreset }} from '@codewhale/dsh-composition';\nimport data from './{id}.json' with {{ type: 'json' }};\nexport async function apply(ctx) {{ await mountReviewedPreset(ctx, new URL('../../source/', import.meta.url).href, data.composition, data.catalog, data.selected); }}\n",
            serde_json::to_string(&metadata)?
        );
        require(
            source.len() <= 16 * 1024,
            "Preset entry metadata is too large.",
        )?;
        let digest = sha256_hex(source.as_bytes());
        metadata["entry"] = json!({"path":entry,"sha256":digest});
        let rows = catalog
            .get_mut("presets")
            .and_then(Json::as_array_mut)
            .ok_or_else(|| anyhow::Error::msg("Preset catalog omitted rows."))?;
        let row = rows
            .iter_mut()
            .find(|p| p.get("id") == metadata.get("id"))
            .ok_or_else(|| anyhow::Error::msg("Preset catalog/entry mismatch."))?;
        *row = metadata;
        output.add(entry.clone(), source.into_bytes())?;
        entries.push((entry, config, item.clone()));
    }
    output.add(
        "native/presets.json".into(),
        serde_json::to_vec_pretty(&catalog)?,
    )?;
    let mut paths = Vec::new();
    for (entry, config, selected) in entries {
        output.add(
            config,
            serde_json::to_vec_pretty(
                &json!({"catalog":catalog,"selected":selected,"composition":document}),
            )?,
        )?;
        paths.push(entry);
    }
    Ok(paths)
}
