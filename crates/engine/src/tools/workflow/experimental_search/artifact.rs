use std::collections::{BTreeMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

use super::{git, normalized_path};

const MAX_FILES: usize = 4096;
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_TREE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CHANGE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct FileEntry {
    pub path: String,
    pub mode: u32,
    pub size_bytes: u64,
    pub blob_hash: String,
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Change {
    pub path: String,
    pub kind: String,
    pub mode: Option<u32>,
    pub size_bytes: Option<u64>,
    pub blob_hash: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Artifact {
    pub schema_version: u32,
    pub artifact_id: String,
    pub baseline_id: String,
    pub candidate_tree_hash: String,
    pub entries: Vec<Change>,
    pub parent_artifact_ids: Vec<String>,
    pub candidate_id: String,
}

pub(super) struct ArtifactStore {
    root: PathBuf,
    baseline: Vec<FileEntry>,
    baseline_id: String,
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn canonical_hash<T: Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_vec(value)
        .map(|bytes| digest(&bytes))
        .map_err(|error| error.to_string())
}

fn check_paths<'a>(paths: impl IntoIterator<Item = &'a str>) -> Result<(), String> {
    let mut folded = HashSet::new();
    for path in paths {
        normalized_path(path)?;
        let key = path.nfkc().collect::<String>().to_lowercase();
        if !folded.insert(key) {
            return Err(format!("artifact path collision: {path}"));
        }
    }
    Ok(())
}

fn mode(path: &Path) -> Result<u32, String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(format!(
            "artifact contains a link or special file: {}",
            path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::PermissionsExt;
        if metadata.nlink() != 1 {
            return Err(format!("artifact contains a hardlink: {}", path.display()));
        }
        Ok(if metadata.permissions().mode() & 0o111 != 0 {
            0o100755
        } else {
            0o100644
        })
    }
    #[cfg(not(unix))]
    {
        Ok(0o100644)
    }
}

fn read_file(path: &Path) -> Result<Vec<u8>, String> {
    let before = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !before.is_file() || before.file_type().is_symlink() || before.len() > MAX_FILE_BYTES {
        return Err(format!(
            "artifact file is unsafe or oversized: {}",
            path.display()
        ));
    }
    #[cfg(unix)]
    if std::os::unix::fs::MetadataExt::nlink(&before) != 1 {
        return Err(format!("artifact contains a hardlink: {}", path.display()));
    }
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    let after = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if bytes.len() as u64 != before.len()
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
        || !after.is_file()
        || after.file_type().is_symlink()
    {
        return Err(format!(
            "artifact file changed during seal: {}",
            path.display()
        ));
    }
    Ok(bytes)
}

fn walk(root: &Path) -> Result<BTreeMap<String, PathBuf>, String> {
    fn visit(
        root: &Path,
        directory: &Path,
        found: &mut BTreeMap<String, PathBuf>,
    ) -> Result<(), String> {
        for item in fs::read_dir(directory).map_err(|error| error.to_string())? {
            let item = item.map_err(|error| error.to_string())?;
            let path = item.path();
            let relative = path.strip_prefix(root).map_err(|error| error.to_string())?;
            let relative = normalized_path(&relative.to_string_lossy())?;
            if relative == ".git" {
                continue;
            }
            let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
            if metadata.file_type().is_symlink() {
                return Err(format!("artifact contains a link: {relative}"));
            }
            if metadata.is_dir() {
                visit(root, &path, found)?;
            } else if metadata.is_file() {
                if found.insert(relative, path).is_some() || found.len() > MAX_FILES {
                    return Err("artifact file count or path uniqueness limit exceeded".to_string());
                }
            } else {
                return Err(format!("artifact contains a special file: {relative}"));
            }
        }
        Ok(())
    }
    let mut found = BTreeMap::new();
    visit(root, root, &mut found)?;
    check_paths(found.keys().map(String::as_str))?;
    Ok(found)
}

impl ArtifactStore {
    pub(super) fn freeze(repo: &Path, root: &Path) -> Result<Self, String> {
        fs::create_dir(root.join("blobs")).map_err(|error| error.to_string())?;
        fs::create_dir(root.join("artifacts")).map_err(|error| error.to_string())?;
        let listing = git(repo, &["ls-tree", "-r", "--full-tree", "-z", "HEAD"])?.stdout;
        let mut baseline = Vec::new();
        let mut total = 0;
        for record in listing
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty())
        {
            let text = std::str::from_utf8(record).map_err(|error| error.to_string())?;
            let (header, path) = text.split_once('\t').ok_or("invalid git tree record")?;
            let mut parts = header.split(' ');
            let file_mode = parts.next().ok_or("missing git mode")?;
            let kind = parts.next().ok_or("missing git kind")?;
            let oid = parts.next().ok_or("missing git blob")?;
            if kind != "blob" || !matches!(file_mode, "100644" | "100755") {
                return Err(format!("unsupported baseline tree entry: {path}"));
            }
            let path = normalized_path(path)?;
            let bytes = git(repo, &["cat-file", "blob", oid])?.stdout;
            if bytes.len() as u64 > MAX_FILE_BYTES {
                return Err(format!("baseline file exceeds limit: {path}"));
            }
            total += bytes.len() as u64;
            if total > MAX_TREE_BYTES || baseline.len() >= MAX_FILES {
                return Err("baseline tree exceeds limits".to_string());
            }
            let blob_hash = digest(&bytes);
            let target = root.join("blobs").join(&blob_hash);
            if !target.exists() {
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&target)
                    .map_err(|error| error.to_string())?;
                file.write_all(&bytes).map_err(|error| error.to_string())?;
                file.sync_all().map_err(|error| error.to_string())?;
            }
            baseline.push(FileEntry {
                path,
                mode: if file_mode == "100755" {
                    0o100755
                } else {
                    0o100644
                },
                size_bytes: bytes.len() as u64,
                blob_hash,
            });
        }
        baseline.sort_by(|a, b| a.path.cmp(&b.path));
        check_paths(baseline.iter().map(|entry| entry.path.as_str()))?;
        let baseline_id = canonical_hash(&baseline)?;
        let bytes = serde_json::to_vec_pretty(&baseline).map_err(|error| error.to_string())?;
        fs::write(root.join("baseline.json"), bytes).map_err(|error| error.to_string())?;
        Ok(Self {
            root: root.to_path_buf(),
            baseline,
            baseline_id,
        })
    }

    pub(super) fn baseline_id(&self) -> &str {
        &self.baseline_id
    }

    pub(super) fn capture(
        &self,
        workspace: &Path,
        candidate_id: &str,
        parent_artifact_ids: Vec<String>,
    ) -> Result<Artifact, String> {
        let found = walk(workspace)?;
        let baseline = self
            .baseline
            .iter()
            .map(|entry| (entry.path.as_str(), entry))
            .collect::<BTreeMap<_, _>>();
        let mut tree = Vec::new();
        let mut changes = Vec::new();
        let mut total = 0u64;
        let mut changed_bytes = 0u64;
        for (relative, path) in found {
            let bytes = read_file(&path)?;
            let file_mode = mode(&path)?;
            total += bytes.len() as u64;
            if total > MAX_TREE_BYTES {
                return Err("candidate tree exceeds byte limit".to_string());
            }
            let blob_hash = digest(&bytes);
            let entry = FileEntry {
                path: relative.clone(),
                mode: file_mode,
                size_bytes: bytes.len() as u64,
                blob_hash: blob_hash.clone(),
            };
            if baseline
                .get(relative.as_str())
                .is_none_or(|old| old.blob_hash != blob_hash || old.mode != file_mode)
            {
                changed_bytes += bytes.len() as u64;
                if changed_bytes > MAX_CHANGE_BYTES {
                    return Err("candidate changes exceed byte limit".to_string());
                }
                let target = self.root.join("blobs").join(&blob_hash);
                if !target.exists() {
                    let mut file = OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&target)
                        .map_err(|error| error.to_string())?;
                    file.write_all(&bytes).map_err(|error| error.to_string())?;
                    file.sync_all().map_err(|error| error.to_string())?;
                }
                changes.push(Change {
                    path: relative,
                    kind: "file".to_string(),
                    mode: Some(file_mode),
                    size_bytes: Some(bytes.len() as u64),
                    blob_hash: Some(blob_hash),
                });
            }
            tree.push(entry);
        }
        for entry in &self.baseline {
            if !tree.iter().any(|current| current.path == entry.path) {
                changes.push(Change {
                    path: entry.path.clone(),
                    kind: "deleted".to_string(),
                    mode: None,
                    size_bytes: None,
                    blob_hash: None,
                });
            }
        }
        changes.sort_by(|a, b| a.path.cmp(&b.path));
        let candidate_tree_hash = canonical_hash(&tree)?;
        let artifact = Artifact {
            schema_version: 1,
            artifact_id: candidate_tree_hash.clone(),
            baseline_id: self.baseline_id.clone(),
            candidate_tree_hash,
            entries: changes,
            parent_artifact_ids,
            candidate_id: candidate_id.to_string(),
        };
        let target = self
            .root
            .join("artifacts")
            .join(format!("{candidate_id}.json"));
        let bytes = serde_json::to_vec_pretty(&artifact).map_err(|error| error.to_string())?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)
            .map_err(|error| error.to_string())?;
        file.write_all(&bytes).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        Ok(artifact)
    }

    pub(super) fn load(&self, candidate_id: &str) -> Result<Artifact, String> {
        let bytes = fs::read(
            self.root
                .join("artifacts")
                .join(format!("{candidate_id}.json")),
        )
        .map_err(|error| error.to_string())?;
        let artifact: Artifact =
            serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        if artifact.schema_version != 1
            || artifact.candidate_id != candidate_id
            || artifact.baseline_id != self.baseline_id
        {
            return Err("sealed artifact identity mismatch".to_string());
        }
        Ok(artifact)
    }

    pub(super) fn materialize(
        &self,
        artifact: &Artifact,
        destination: &Path,
    ) -> Result<(), String> {
        if destination.exists() {
            return Err("evaluation materialization already exists".to_string());
        }
        fs::create_dir(destination).map_err(|error| error.to_string())?;
        let mut files = self
            .baseline
            .iter()
            .map(|entry| (entry.path.clone(), entry.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut previous = None;
        for change in &artifact.entries {
            normalized_path(&change.path)?;
            if previous.is_some_and(|path: &str| path >= change.path.as_str()) {
                return Err("artifact entries are not strictly sorted".to_string());
            }
            previous = Some(&change.path);
            match change.kind.as_str() {
                "deleted" if change.blob_hash.is_none() && change.mode.is_none() => {
                    files.remove(&change.path);
                }
                "file" => {
                    let entry = FileEntry {
                        path: change.path.clone(),
                        mode: change.mode.ok_or("missing artifact mode")?,
                        size_bytes: change.size_bytes.ok_or("missing artifact size")?,
                        blob_hash: change.blob_hash.clone().ok_or("missing artifact blob")?,
                    };
                    if !matches!(entry.mode, 0o100644 | 0o100755)
                        || entry.size_bytes > MAX_FILE_BYTES
                        || entry.blob_hash.len() != 64
                        || !entry.blob_hash.bytes().all(|byte| byte.is_ascii_hexdigit())
                    {
                        return Err("invalid artifact entry mode or size".to_string());
                    }
                    files.insert(change.path.clone(), entry);
                }
                _ => return Err("invalid artifact change kind".to_string()),
            }
        }
        check_paths(files.keys().map(String::as_str))?;
        let tree = files.values().cloned().collect::<Vec<_>>();
        if canonical_hash(&tree)? != artifact.candidate_tree_hash
            || artifact.artifact_id != artifact.candidate_tree_hash
        {
            return Err("sealed artifact tree hash mismatch".to_string());
        }
        let total = tree.iter().map(|entry| entry.size_bytes).sum::<u64>();
        if tree.len() > MAX_FILES || total > MAX_TREE_BYTES {
            return Err("materialized tree exceeds limits".to_string());
        }
        for entry in tree {
            let bytes = fs::read(self.root.join("blobs").join(&entry.blob_hash))
                .map_err(|error| error.to_string())?;
            if bytes.len() as u64 != entry.size_bytes || digest(&bytes) != entry.blob_hash {
                return Err(format!(
                    "sealed artifact blob verification failed: {}",
                    entry.path
                ));
            }
            let target = destination.join(&entry.path);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            fs::write(&target, bytes).map_err(|error| error.to_string())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&target, fs::Permissions::from_mode(entry.mode & 0o777))
                    .map_err(|error| error.to_string())?;
            }
        }
        Ok(())
    }

    pub(super) fn review_material(&self, artifact: &Artifact) -> Result<Option<String>, String> {
        let mut result = String::new();
        for entry in &artifact.entries {
            result.push_str(&format!("\nFILE {} {}\n", entry.path, entry.kind));
            if let Some(blob_hash) = &entry.blob_hash {
                let bytes = fs::read(self.root.join("blobs").join(blob_hash))
                    .map_err(|error| error.to_string())?;
                if digest(&bytes) != *blob_hash {
                    return Err("review blob hash mismatch".to_string());
                }
                match String::from_utf8(bytes) {
                    Ok(text) => result.push_str(&text),
                    Err(_) => result.push_str(&format!("[binary sha256={blob_hash}]\n")),
                }
            }
            if result.len() > 128 * 1024 {
                return Ok(None);
            }
        }
        Ok(Some(result))
    }
}
