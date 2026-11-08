use std::collections::{BTreeMap, HashSet};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::session_secret_scrub::{directory_entries, entry_has_type};

pub const PRUNE_IMAGES_COMMAND: &str = "codewhale sessions prune-images";

const NAME_PREFIX: &[u8] = b"clipboard-";
const NAME_SUFFIX: &[u8] = b".png";
const SCAN_CHUNK: usize = 1 << 20;
const NAME_CARRY: usize = 96;

#[derive(Debug, Default)]
pub struct ImagePruneReport {
    pub images: usize,
    pub image_bytes: u64,
    pub referenced: usize,
    pub recent: usize,
    pub unreferenced: Vec<(PathBuf, u64)>,
    pub removed: Vec<(PathBuf, u64)>,
    pub failed: Vec<(PathBuf, String)>,
    pub files_scanned: usize,
    pub unreadable: Vec<PathBuf>,
}

impl ImagePruneReport {
    #[must_use]
    pub fn unreferenced_bytes(&self) -> u64 {
        self.unreferenced.iter().map(|(_, bytes)| bytes).sum()
    }

    #[must_use]
    pub fn removed_bytes(&self) -> u64 {
        self.removed.iter().map(|(_, bytes)| bytes).sum()
    }
}

#[must_use]
pub fn is_clipboard_image_name(name: &str) -> bool {
    name.strip_prefix("clipboard-")
        .and_then(|rest| rest.strip_suffix(".png"))
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

pub fn prune_clipboard_images(
    image_dir: &Path,
    scan_roots: &[PathBuf],
    grace: Duration,
    apply: bool,
) -> ImagePruneReport {
    let mut report = ImagePruneReport::default();
    let mut images: BTreeMap<String, (PathBuf, u64, SystemTime)> = BTreeMap::new();
    for entry in directory_entries(image_dir, &mut report.unreadable) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_clipboard_image_name(&name) || !entry_has_type(&entry, false, &mut report.unreadable)
        {
            continue;
        }
        match entry.metadata() {
            Ok(meta) => {
                report.image_bytes += meta.len();
                images.insert(
                    name,
                    (
                        entry.path(),
                        meta.len(),
                        meta.modified().unwrap_or(SystemTime::now()),
                    ),
                );
            }
            Err(_) => report.unreadable.push(entry.path()),
        }
    }
    report.images = images.len();
    if images.is_empty() {
        return report;
    }

    let excluded = std::fs::canonicalize(image_dir).unwrap_or_else(|_| image_dir.to_path_buf());
    let mut referenced = HashSet::new();
    let mut seen_roots = HashSet::new();
    let mut pending: Vec<PathBuf> = Vec::new();
    for root in scan_roots {
        if let Ok(root) = std::fs::canonicalize(root)
            && seen_roots.insert(root.clone())
        {
            pending.push(root);
        }
    }
    let mut visited_dirs = HashSet::new();
    while let Some(dir) = pending.pop() {
        if dir == excluded || !visited_dirs.insert(dir.clone()) {
            continue;
        }
        for entry in directory_entries(&dir, &mut report.unreadable) {
            let Ok(kind) = entry.file_type() else {
                report.unreadable.push(entry.path());
                continue;
            };
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                report.files_scanned += 1;
                if scan_file(&entry.path(), &mut referenced).is_err() {
                    report.unreadable.push(entry.path());
                }
                if images
                    .keys()
                    .all(|name| referenced.contains(name.as_bytes()))
                {
                    pending.clear();
                    break;
                }
            }
        }
    }

    let now = SystemTime::now();
    for (name, (path, bytes, modified)) in images {
        if referenced.contains(name.as_bytes()) {
            report.referenced += 1;
        } else if now.duration_since(modified).unwrap_or_default() < grace {
            report.recent += 1;
        } else {
            report.unreferenced.push((path, bytes));
        }
    }
    report.unreadable.sort();
    report.unreadable.dedup();
    if apply && report.unreadable.is_empty() {
        for (path, bytes) in std::mem::take(&mut report.unreferenced) {
            match std::fs::remove_file(&path) {
                Ok(()) => report.removed.push((path, bytes)),
                Err(error) => report.failed.push((path, error.to_string())),
            }
        }
    }
    report
}

fn scan_file(path: &Path, referenced: &mut HashSet<Vec<u8>>) -> io::Result<()> {
    let mut file = std::fs::File::open(path)?;
    let mut buffer = vec![0_u8; SCAN_CHUNK + NAME_CARRY];
    let mut carried = 0;
    loop {
        let read = file.read(&mut buffer[carried..])?;
        if read == 0 {
            break;
        }
        let filled = carried + read;
        let window = &buffer[..filled];
        let mut start = 0;
        while let Some(offset) = find(&window[start..], NAME_PREFIX) {
            let begin = start + offset;
            let digits_start = begin + NAME_PREFIX.len();
            let digits_end = window[digits_start..]
                .iter()
                .position(|byte| !byte.is_ascii_digit())
                .map_or(filled, |len| digits_start + len);
            let end = digits_end + NAME_SUFFIX.len();
            if digits_end > digits_start && end <= filled && &window[digits_end..end] == NAME_SUFFIX
            {
                referenced.insert(window[begin..end].to_vec());
            }
            start = digits_start;
        }
        carried = filled.min(NAME_CARRY);
        buffer.copy_within(filled - carried..filled, 0);
    }
    Ok(())
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
