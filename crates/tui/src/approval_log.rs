use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::fleet::files::WorkspaceFile;
use crate::sandbox::SandboxPolicy;

const APPROVAL_LOG_FILE: &str = "approval_receipts.jsonl";
const APPROVAL_LOCK_FILE: &str = "approval_receipts.lock";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub(crate) enum ApprovalOutcome {
    ApprovedOnce,
    Denied,
    /// The interactive approval card expired unanswered (#6101): the
    /// configured bound denied the call, not the operator.
    Timeout,
    Cancelled,
    Unavailable,
    RetryWithPolicy {
        policy: SandboxPolicy,
    },
}

// Shared data ownership; persisted approval serialization remains unchanged.
pub(crate) use codewhale_command_contract::facets::ApprovalDecider;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub(crate) enum ApprovalReceipt {
    Asked {
        approval_id: String,
        tool_call_id: String,
        tool_name: String,
        created_at: DateTime<Utc>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
    Decided {
        approval_id: String,
        tool_call_id: String,
        outcome: ApprovalOutcome,
        created_at: DateTime<Utc>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decided_by: Option<ApprovalDecider>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        grant_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        matched_label: Option<String>,
    },
}

impl ApprovalReceipt {
    pub(crate) fn asked(tool_call_id: impl Into<String>, tool_name: impl Into<String>) -> Self {
        let tool_call_id = tool_call_id.into();
        Self::Asked {
            approval_id: tool_call_id.clone(),
            tool_call_id,
            tool_name: tool_name.into(),
            created_at: Utc::now(),
            target: None,
            summary: None,
        }
    }

    pub(crate) fn asked_with(
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        input: &serde_json::Value,
        description: &str,
        workspace: &Path,
    ) -> Self {
        let mut ask = Self::asked(tool_call_id, tool_name);
        if let Self::Asked {
            target, summary, ..
        } = &mut ask
        {
            *target = ask_target(input, workspace);
            *summary = ask_summary(input, description, workspace);
        }
        ask
    }

    pub(crate) fn target(&self) -> Option<&str> {
        match self {
            Self::Asked { target, .. } => target.as_deref(),
            Self::Decided { .. } => None,
        }
    }

    pub(crate) fn summary(&self) -> Option<&str> {
        match self {
            Self::Asked { summary, .. } => summary.as_deref(),
            Self::Decided { .. } => None,
        }
    }

    /// A decision whose decider this call site does not know.
    #[cfg(test)]
    pub(crate) fn decided(tool_call_id: impl Into<String>, outcome: ApprovalOutcome) -> Self {
        Self::decided_with(tool_call_id, outcome, None)
    }

    pub(crate) fn decided_with(
        tool_call_id: impl Into<String>,
        outcome: ApprovalOutcome,
        decided_by: Option<ApprovalDecider>,
    ) -> Self {
        let tool_call_id = tool_call_id.into();
        Self::Decided {
            approval_id: tool_call_id.clone(),
            tool_call_id,
            outcome,
            created_at: Utc::now(),
            decided_by,
            grant_id: None,
            matched_label: None,
        }
    }

    pub(crate) fn with_grant(mut self, grant: &crate::core::authority::grants::GrantRef) -> Self {
        if let Self::Decided {
            grant_id,
            matched_label,
            ..
        } = &mut self
        {
            *grant_id = Some(grant.grant_id.clone());
            *matched_label = Some(grant.label.clone());
        }
        self
    }

    pub(crate) fn approval_id(&self) -> &str {
        match self {
            Self::Asked { approval_id, .. } | Self::Decided { approval_id, .. } => approval_id,
        }
    }

    fn tool_call_id(&self) -> &str {
        match self {
            Self::Asked { tool_call_id, .. } | Self::Decided { tool_call_id, .. } => tool_call_id,
        }
    }

    /// The tool the agent wanted to run — present on the ask half only.
    pub(crate) fn tool_name(&self) -> Option<&str> {
        match self {
            Self::Asked { tool_name, .. } => Some(tool_name),
            Self::Decided { .. } => None,
        }
    }

    pub(crate) fn created_at(&self) -> DateTime<Utc> {
        match self {
            Self::Asked { created_at, .. } | Self::Decided { created_at, .. } => *created_at,
        }
    }
}

const ASK_TEXT_CAP: usize = 120;

fn cap_ask_text(text: &str) -> Option<String> {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    if collapsed.chars().count() <= ASK_TEXT_CAP {
        return Some(collapsed);
    }
    let head: String = collapsed.chars().take(ASK_TEXT_CAP - 1).collect();
    Some(format!("{head}…"))
}

fn safe_path(token: &str, workspace: &Path) -> String {
    let root = workspace.to_string_lossy();
    let root = root.trim_end_matches('/');
    if !root.is_empty()
        && let Some(rest) = token.strip_prefix(root)
    {
        if rest.is_empty() {
            return ".".to_string();
        }
        if rest.starts_with('/') {
            return format!(".{rest}");
        }
    }
    if token.starts_with('/') || token.starts_with('~') {
        let base = token
            .rsplit('/')
            .find(|part| !part.is_empty())
            .unwrap_or("");
        return format!(".../{base}");
    }
    token.to_string()
}

fn split_shell_words(text: &str) -> Vec<&str> {
    let mut words = Vec::new();
    let mut start = None;
    let mut quote: Option<char> = None;
    for (at, c) in text.char_indices() {
        match (quote, c) {
            (Some(q), _) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'' | '`') => {
                quote = Some(c);
                start.get_or_insert(at);
            }
            (None, c) if c.is_whitespace() => {
                if let Some(s) = start.take() {
                    words.push(&text[s..at]);
                }
            }
            _ => {
                start.get_or_insert(at);
            }
        }
    }
    if let Some(s) = start {
        words.push(&text[s..]);
    }
    words
}

fn plain_word(word: &str) -> bool {
    const SENSITIVE: [&str; 11] = [
        "pass", "token", "secret", "key", "auth", "cred", "user", "cookie", "bearer", "header",
        "private",
    ];
    if word.is_empty() || word.chars().count() > 64 {
        return false;
    }
    if !word
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "_./~+,%-".contains(c))
    {
        return false;
    }
    if let Some(flag) = word.strip_prefix("--") {
        let lower = flag.to_ascii_lowercase();
        return !flag.is_empty() && !SENSITIVE.iter().any(|s| lower.contains(s));
    }
    if let Some(flag) = word.strip_prefix('-') {
        return word.len() > 1 && flag.len() == 1 && "alrRfnvih".contains(flag);
    }
    true
}

fn redact_ask_text(text: &str, workspace: &Path, command: bool) -> Option<String> {
    let words = split_shell_words(text);
    let redacted = if command {
        let program = words.first().copied().filter(|w| plain_word(w));
        match program {
            None => "[command redacted]".to_string(),
            Some(program) => {
                let name = safe_path(program, workspace);
                let name = name.rsplit('/').next().unwrap_or(&name).to_string();
                let rest = &words[1..];
                if rest.iter().all(|w| plain_word(w)) {
                    let mut out = vec![name];
                    out.extend(rest.iter().map(|w| safe_path(w, workspace)));
                    out.join(" ")
                } else {
                    format!("{name} [args redacted]")
                }
            }
        }
    } else {
        words
            .iter()
            .map(|w| {
                if plain_word(w)
                    || w.chars()
                        .all(|c| c.is_alphanumeric() || ".,:;()".contains(c))
                {
                    safe_path(w, workspace)
                } else {
                    "[redacted]".to_string()
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    };
    let (redacted, _) = crate::session_peek::redact(&redacted);
    cap_ask_text(&redacted)
}

fn ask_target(input: &serde_json::Value, workspace: &Path) -> Option<String> {
    for key in ["url", "uri"] {
        if let Some(raw) = input.get(key).and_then(|v| v.as_str())
            && let Ok(url) = reqwest::Url::parse(raw.trim())
            && let Some(host) = url.host_str()
        {
            return cap_ask_text(host);
        }
    }
    for key in ["path", "file_path", "filepath", "filename"] {
        if let Some(raw) = input.get(key).and_then(|v| v.as_str()) {
            let raw = raw.trim();
            if raw.is_empty() || raw.contains(char::is_whitespace) {
                return redact_ask_text(raw, workspace, false);
            }
            return cap_ask_text(&safe_path(raw, workspace));
        }
    }
    None
}

fn ask_summary(input: &serde_json::Value, description: &str, workspace: &Path) -> Option<String> {
    match input
        .get("command")
        .and_then(|v| v.as_str())
        .filter(|c| !c.trim().is_empty())
    {
        Some(command) => redact_ask_text(command, workspace, true),
        None => redact_ask_text(description, workspace, false),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompletedApproval {
    pub(crate) ask: ApprovalReceipt,
    pub(crate) outcome: ApprovalOutcome,
    pub(crate) decided_at: DateTime<Utc>,
    pub(crate) decided_by: Option<ApprovalDecider>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ApprovalReplay {
    pub(crate) completed: Vec<CompletedApproval>,
    pub(crate) unmatched_asks: Vec<ApprovalReceipt>,
}

impl ApprovalReplay {
    pub(crate) fn from_receipts(receipts: &[ApprovalReceipt]) -> Result<Self, String> {
        let mut open = BTreeMap::<String, ApprovalReceipt>::new();
        let mut closed = BTreeSet::<String>::new();
        let mut completed = Vec::new();

        for receipt in receipts {
            let approval_id = receipt.approval_id();
            if approval_id.trim().is_empty() || receipt.tool_call_id().trim().is_empty() {
                return Err("approval receipt has an empty correlation id".to_string());
            }
            match receipt {
                ApprovalReceipt::Asked {
                    approval_id,
                    tool_call_id,
                    tool_name,
                    ..
                } => {
                    if tool_name.trim().is_empty() {
                        return Err(format!(
                            "approval ask '{approval_id}' has an empty tool name"
                        ));
                    }
                    if approval_id != tool_call_id {
                        return Err(format!(
                            "approval ask '{approval_id}' does not match tool call '{tool_call_id}'"
                        ));
                    }
                    if closed.contains(approval_id) || open.contains_key(approval_id) {
                        return Err(format!("approval '{approval_id}' was asked more than once"));
                    }
                    open.insert(approval_id.clone(), receipt.clone());
                }
                ApprovalReceipt::Decided {
                    approval_id,
                    tool_call_id,
                    outcome,
                    created_at,
                    decided_by,
                    ..
                } => {
                    if approval_id != tool_call_id {
                        return Err(format!(
                            "approval decision '{approval_id}' does not match tool call '{tool_call_id}'"
                        ));
                    }
                    let Some(ask) = open.remove(approval_id) else {
                        return Err(format!(
                            "approval decision '{approval_id}' has no unmatched ask"
                        ));
                    };
                    closed.insert(approval_id.clone());
                    completed.push(CompletedApproval {
                        ask,
                        outcome: outcome.clone(),
                        decided_at: *created_at,
                        decided_by: *decided_by,
                    });
                }
            }
        }

        Ok(Self {
            completed,
            unmatched_asks: open.into_values().collect(),
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ApprovalReceiptStore {
    sessions_dir: PathBuf,
}

#[derive(Default)]
struct ApprovalLogRead {
    receipts: Vec<ApprovalReceipt>,
    valid_bytes: u64,
    torn_tail: Option<Vec<u8>>,
    needs_newline: bool,
}

fn preserve_torn_tail(log: &WorkspaceFile, bytes: &[u8]) -> io::Result<()> {
    log.sibling(&format!(
        "approval_receipts-torn-{}.recovery",
        uuid::Uuid::new_v4()
    ))?
    .publish(bytes)
}

impl ApprovalReceiptStore {
    pub(crate) fn new(sessions_dir: PathBuf) -> Self {
        Self { sessions_dir }
    }

    pub(crate) fn default_location() -> io::Result<Self> {
        crate::session_manager::default_sessions_dir().map(Self::new)
    }

    fn validated_session_id(session_id: &str) -> io::Result<&str> {
        let trimmed = session_id.trim();
        if trimmed.is_empty()
            || !trimmed
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Invalid session id '{session_id}'"),
            ));
        }
        Ok(trimmed)
    }

    pub(crate) fn log_path(&self, session_id: &str) -> io::Result<PathBuf> {
        let session_id = Self::validated_session_id(session_id)?;
        Ok(self.sessions_dir.join(session_id).join(APPROVAL_LOG_FILE))
    }

    fn log_file(&self, session_id: &str, create: bool) -> io::Result<WorkspaceFile> {
        let session_id = Self::validated_session_id(session_id)?;
        if create {
            fs::create_dir_all(&self.sessions_dir)?;
        }
        WorkspaceFile::open(
            &self.sessions_dir,
            &PathBuf::from(session_id).join(APPROVAL_LOG_FILE),
            create,
        )
    }

    fn load_unlocked(log: &WorkspaceFile) -> io::Result<Option<ApprovalLogRead>> {
        let file = match log.open_file_shared() {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err),
        };
        Self::decode_log(file).map(Some)
    }

    fn decode_log(file: File) -> io::Result<ApprovalLogRead> {
        let mut reader = BufReader::new(file);
        let mut loaded = ApprovalLogRead::default();
        let mut line = Vec::new();
        let mut index = 0;
        loop {
            line.clear();
            if reader.read_until(b'\n', &mut line)? == 0 {
                break;
            }
            index += 1;
            let terminated = line.last() == Some(&b'\n');
            match serde_json::from_slice(&line) {
                Ok(receipt) => {
                    loaded.receipts.push(receipt);
                    loaded.valid_bytes += line.len() as u64;
                    loaded.needs_newline = !terminated;
                }
                Err(error) if !terminated && (error.is_eof() || error.is_syntax()) => {
                    // Only malformed, unterminated EOF bytes can be a torn
                    // write. Complete lines and valid JSON with an invalid
                    // receipt schema remain fatal, as do replay violations.
                    loaded.torn_tail = Some(line);
                    break;
                }
                Err(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("invalid approval receipt at line {index}"),
                    ));
                }
            }
        }
        ApprovalReplay::from_receipts(&loaded.receipts)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        Ok(loaded)
    }

    pub(crate) fn load(&self, session_id: &str) -> io::Result<Vec<ApprovalReceipt>> {
        Ok(self.load_if_present(session_id)?.unwrap_or_default())
    }

    /// Preserve the distinction between a missing legacy log and an existing
    /// authoritative log with no complete receipts, including a torn first write.
    pub(crate) fn load_if_present(
        &self,
        session_id: &str,
    ) -> io::Result<Option<Vec<ApprovalReceipt>>> {
        let log = match self.log_file(session_id, false) {
            Ok(log) => log,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let lock_file = match log.sibling(APPROVAL_LOCK_FILE)?.open_file_shared() {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Self::load_unlocked(&log)?.map(|loaded| loaded.receipts));
            }
            Err(error) => return Err(error),
        };
        let lock = fd_lock::RwLock::new(lock_file);
        let _guard = lock.read()?;
        Ok(Self::load_unlocked(&log)?.map(|loaded| loaded.receipts))
    }

    pub(crate) fn replay(&self, session_id: &str) -> io::Result<ApprovalReplay> {
        let receipts = self.load(session_id)?;
        ApprovalReplay::from_receipts(&receipts)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
    }

    /// Session ids that have an approval log. Only entries whose name is a
    /// valid session id are considered, so a stray file in the sessions dir
    /// can never become a path traversal. A missing or unreadable sessions
    /// dir is an empty history, not an error.
    pub(crate) fn sessions_with_logs(&self) -> Vec<String> {
        let entries = match fs::read_dir(&self.sessions_dir) {
            Ok(entries) => entries,
            Err(_) => return Vec::new(),
        };
        let mut ids = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if Self::validated_session_id(&name).is_ok()
                && entry.path().join(APPROVAL_LOG_FILE).exists()
            {
                ids.push(name);
            }
        }
        ids
    }

    pub(crate) fn append(&self, session_id: &str, receipt: &ApprovalReceipt) -> io::Result<()> {
        self.append_with_preservation(session_id, receipt, preserve_torn_tail)
    }

    fn append_with_preservation(
        &self,
        session_id: &str,
        receipt: &ApprovalReceipt,
        preserve: impl FnOnce(&WorkspaceFile, &[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        let log = self.log_file(session_id, true)?;
        let lock_file = log.sibling(APPROVAL_LOCK_FILE)?.open_update(true, false)?;
        crate::utils::restrict_to_owner(&lock_file)?;
        let mut lock = fd_lock::RwLock::new(lock_file);
        let _guard = lock.write()?;
        let mut loaded = Self::load_unlocked(&log)?.unwrap_or_default();
        loaded.receipts.push(receipt.clone());
        ApprovalReplay::from_receipts(&loaded.receipts)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;

        let mut line = serde_json::to_vec(receipt)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        if loaded.needs_newline {
            // A complete final JSON receipt is authoritative even without LF.
            line.insert(0, b'\n');
        }
        line.push(b'\n');
        // Approval receipts are owner-only and never written through a link.
        let mut file = log.open_update(true, true)?;
        crate::utils::restrict_to_owner(&file)?;
        if let Some(tail) = &loaded.torn_tail {
            // Validate the candidate and preserve evidence before touching the
            // live log. Any preservation error leaves the original intact.
            // Windows append handles lack the write access set_len requires.
            let repair = log.open_update(false, false)?;
            preserve(&log, tail)?;
            repair.set_len(loaded.valid_bytes)?;
            repair.sync_all()?;
        }
        file.write_all(&line)?;
        file.sync_all()?;
        let _ = log.sync_parent();
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn sessions_dir(&self) -> &std::path::Path {
        &self.sessions_dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;

    #[cfg(unix)]
    #[test]
    fn repair_keeps_evidence_and_mutations_in_the_opened_parent() -> io::Result<()> {
        for fail_preservation in [false, true] {
            let temp = tempfile::tempdir()?;
            let root = temp.path().canonicalize()?;
            let selected = root.join("sessions");
            let retained = root.join("retained-sessions");
            let store = ApprovalReceiptStore::new(selected.clone());
            let ask = ApprovalReceipt::asked("original-tool", "exec_shell");
            store.append("target", &ask)?;
            let path = store.log_path("target")?;
            let tail = b"{\"phase\":\"decided\"";
            OpenOptions::new()
                .append(true)
                .open(&path)?
                .write_all(tail)?;
            let original = fs::read(&path)?;
            let decision = ApprovalReceipt::decided("original-tool", ApprovalOutcome::Denied);
            let mut replacement_snapshot = None;
            let result = store.append_with_preservation("target", &decision, |log, bytes| {
                assert_eq!(bytes, tail);
                fs::rename(&selected, &retained)?;
                let replacement = ApprovalReceiptStore::new(selected.clone());
                replacement.append(
                    "target",
                    &ApprovalReceipt::asked("replacement-tool", "read_file"),
                )?;
                let replacement_path = replacement.log_path("target")?;
                replacement_snapshot =
                    Some((replacement_path.clone(), fs::read(replacement_path)?));
                fs::write(selected.join("canary"), b"unrelated replacement")?;
                if fail_preservation {
                    return Err(io::Error::other("simulated preservation failure"));
                }
                preserve_torn_tail(log, bytes)?;
                assert_eq!(
                    fs::read(retained.join("target").join(APPROVAL_LOG_FILE))?,
                    original,
                    "evidence publication must precede truncation"
                );
                Ok(())
            });
            if fail_preservation {
                assert_eq!(
                    result.unwrap_err().to_string(),
                    "simulated preservation failure"
                );
                assert_eq!(
                    fs::read(retained.join("target").join(APPROVAL_LOG_FILE))?,
                    original
                );
            } else {
                result?;
            }
            let (replacement_path, bytes) = replacement_snapshot.unwrap();
            assert_eq!(fs::read(replacement_path)?, bytes);
            assert_eq!(fs::read(selected.join("canary"))?, b"unrelated replacement");
            let preserved = ApprovalReceiptStore::new(retained.clone());
            assert_eq!(
                preserved.load("target")?,
                if fail_preservation {
                    vec![ask]
                } else {
                    vec![ask, decision]
                }
            );
            let recovery = fs::read_dir(retained.join("target"))?
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<io::Result<Vec<_>>>()?
                .into_iter()
                .filter(|path| path.extension().is_some_and(|ext| ext == "recovery"))
                .collect::<Vec<_>>();
            assert_eq!(recovery.len(), usize::from(!fail_preservation));
            if !fail_preservation {
                assert_eq!(fs::read(&recovery[0])?, tail);
            }
            assert!(fs::read_dir(selected.join("target"))?.all(|entry| {
                !entry
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|ext| ext == "recovery")
            }));
        }
        Ok(())
    }

    #[test]
    fn reads_preserve_absent_roots_and_legacy_logs_without_locks() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let selected = temp.path().join("sessions");
        let store = ApprovalReceiptStore::new(selected.clone());
        assert!(store.load_if_present("missing")?.is_none());
        assert!(store.load("missing")?.is_empty());
        assert!(!selected.try_exists()?);
        assert!(
            store
                .append("../invalid", &ApprovalReceipt::asked("tool", "read_file"))
                .is_err()
        );
        assert!(!selected.try_exists()?);
        let ask = ApprovalReceipt::asked("legacy-tool", "read_file");
        let log = store.log_path("legacy")?;
        fs::create_dir_all(log.parent().unwrap())?;
        let mut bytes = serde_json::to_vec(&ask).map_err(io::Error::other)?;
        bytes.push(b'\n');
        fs::write(&log, &bytes)?;
        assert_eq!(store.load("legacy")?, vec![ask]);
        assert_eq!(fs::read(&log)?, bytes);
        assert_eq!(fs::read_dir(log.parent().unwrap())?.count(), 1);
        assert!(store.load_if_present("still-missing")?.is_none());
        assert!(!selected.join("still-missing").try_exists()?);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn linked_session_directory_is_not_an_approval_transaction_parent() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let selected = temp.path().join("sessions");
        let foreign = temp.path().join("foreign");
        fs::create_dir_all(&selected)?;
        fs::create_dir(&foreign)?;
        fs::write(foreign.join("canary"), b"unrelated owner")?;
        std::os::unix::fs::symlink(&foreign, selected.join("target"))?;
        let store = ApprovalReceiptStore::new(selected);
        assert!(
            store
                .append("target", &ApprovalReceipt::asked("tool", "read_file"))
                .is_err()
        );
        assert!(store.load_if_present("target").is_err());
        assert_eq!(fs::read(foreign.join("canary"))?, b"unrelated owner");
        assert_eq!(fs::read_dir(foreign)?.count(), 1);
        Ok(())
    }

    fn completed_receipts(outcome: ApprovalOutcome) -> Vec<ApprovalReceipt> {
        vec![
            ApprovalReceipt::asked("tool-1", "exec_shell"),
            ApprovalReceipt::decided("tool-1", outcome),
        ]
    }

    #[test]
    fn replay_reconstructs_every_closed_outcome() {
        let outcomes = [
            ApprovalOutcome::ApprovedOnce,
            ApprovalOutcome::Denied,
            ApprovalOutcome::Cancelled,
            ApprovalOutcome::Unavailable,
            ApprovalOutcome::RetryWithPolicy {
                policy: crate::sandbox::SandboxPolicy::DangerFullAccess,
            },
        ];

        for outcome in outcomes {
            let replay = ApprovalReplay::from_receipts(&completed_receipts(outcome.clone()))
                .expect("closed approval log replays");
            assert_eq!(replay.completed.len(), 1);
            assert_eq!(replay.completed[0].outcome, outcome);
            assert!(replay.unmatched_asks.is_empty());
        }
    }

    #[test]
    fn replay_detects_an_unmatched_ask_after_interruption() {
        let ask = ApprovalReceipt::asked("tool-interrupted", "write_file");
        let replay = ApprovalReplay::from_receipts(std::slice::from_ref(&ask))
            .expect("an unmatched ask is valid crash evidence");

        assert!(replay.completed.is_empty());
        assert_eq!(replay.unmatched_asks, vec![ask]);
    }

    #[test]
    fn replay_rejects_decisions_without_one_open_ask() {
        let orphan = ApprovalReceipt::decided("tool-orphan", ApprovalOutcome::ApprovedOnce);
        assert!(ApprovalReplay::from_receipts(&[orphan]).is_err());

        let duplicate = vec![
            ApprovalReceipt::asked("tool-duplicate", "exec_shell"),
            ApprovalReceipt::decided("tool-duplicate", ApprovalOutcome::Denied),
            ApprovalReceipt::decided("tool-duplicate", ApprovalOutcome::ApprovedOnce),
        ];
        assert!(ApprovalReplay::from_receipts(&duplicate).is_err());
    }

    #[test]
    fn store_appends_and_replays_a_session_owned_log() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = ApprovalReceiptStore::new(tmp.path().join("sessions"));
        let ask = ApprovalReceipt::asked("tool-persisted", "edit_file");
        let decision = ApprovalReceipt::decided("tool-persisted", ApprovalOutcome::ApprovedOnce);

        store
            .append("session-1", &ask)
            .expect("persist approval ask");
        store
            .append("session-1", &decision)
            .expect("persist approval decision");

        let receipts = store.load("session-1").expect("load approval log");
        assert_eq!(receipts, vec![ask, decision]);
        let replay = ApprovalReplay::from_receipts(&receipts).expect("replay approval log");
        assert_eq!(replay.completed.len(), 1);
        assert!(replay.unmatched_asks.is_empty());
    }

    #[test]
    fn loading_missing_or_imported_logs_is_read_only() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sessions_dir = tmp.path().join("sessions");
        let store = ApprovalReceiptStore::new(sessions_dir.clone());

        assert!(store.load("session-empty").expect("missing log").is_empty());
        assert!(!sessions_dir.join("session-empty").exists());

        let imported_dir = sessions_dir.join("session-imported");
        fs::create_dir_all(&imported_dir).expect("imported session dir");
        let ask = ApprovalReceipt::asked("tool-imported", "exec_shell");
        let mut line = serde_json::to_vec(&ask).expect("serialize imported ask");
        line.push(b'\n');
        fs::write(imported_dir.join(APPROVAL_LOG_FILE), line).expect("imported log");

        assert_eq!(
            store.load("session-imported").expect("load imported log"),
            vec![ask]
        );
        assert!(!imported_dir.join(APPROVAL_LOCK_FILE).exists());
    }

    #[test]
    fn torn_tail_loads_only_valid_prefix_without_changing_evidence() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ApprovalReceiptStore::new(tmp.path().join("sessions"));
        let ask = ApprovalReceipt::asked("tool-interrupted", "write_file");
        let mut prefix = serde_json::to_vec(&ask).unwrap();
        prefix.push(b'\n');
        for (index, tail) in [
            b"{\"phase\":\"decided\",\"approval_id\":\"tool-interrupted\"".as_slice(),
            b"{\"phase\":\"asked\",\"tool_name\":\"\xf0\x9f".as_slice(),
        ]
        .into_iter()
        .enumerate()
        {
            let id = format!("session-torn-{index}");
            let path = store.log_path(&id).unwrap();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mut original = prefix.clone();
            original.extend_from_slice(tail);
            fs::write(&path, &original).unwrap();
            let replay = store.replay(&id).unwrap();
            assert!(replay.completed.is_empty());
            assert_eq!(replay.unmatched_asks, vec![ask.clone()]);
            assert_eq!(fs::read(&path).unwrap(), original);
            assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        }
    }

    #[test]
    fn reader_distinguishes_missing_empty_and_torn_first_logs() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ApprovalReceiptStore::new(tmp.path().join("sessions"));
        assert!(store.load_if_present("session-missing").unwrap().is_none());
        for (id, bytes) in [
            ("session-empty", b"".as_slice()),
            ("session-torn", b"{\"phase\":\"asked\"".as_slice()),
        ] {
            let path = store.log_path(id).unwrap();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, bytes).unwrap();
            let loaded = ApprovalReceiptStore::load_unlocked(&store.log_file(id, false).unwrap())
                .unwrap()
                .expect("existing log");
            assert!(loaded.receipts.is_empty());
            assert_eq!(store.load_if_present(id).unwrap(), Some(Vec::new()));
            assert_eq!(loaded.valid_bytes, 0);
            assert_eq!(loaded.torn_tail.is_some(), !bytes.is_empty());
            assert_eq!(fs::read(path).unwrap(), bytes);
        }
    }

    #[test]
    fn append_preserves_torn_bytes_before_repair_and_never_invents_a_decision() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ApprovalReceiptStore::new(tmp.path().join("sessions"));
        let ask = ApprovalReceipt::asked("tool-interrupted", "write_file");
        store.append("session-repair", &ask).unwrap();
        let path = store.log_path("session-repair").unwrap();
        let prefix = fs::read(&path).unwrap();
        let tail = b"{\"phase\":\"decided\",\"outcome\":\"approved_once\",\"note\":\"\xf0\x9f";
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(tail)
            .unwrap();

        let new_ask = ApprovalReceipt::asked("tool-next", "read_file");
        store.append("session-repair", &new_ask).unwrap();
        let replay = store.replay("session-repair").unwrap();
        assert!(replay.completed.is_empty());
        assert_eq!(replay.unmatched_asks, vec![ask, new_ask]);
        assert!(fs::read(&path).unwrap().starts_with(&prefix));
        let recovery = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "recovery"))
            .collect::<Vec<_>>();
        assert_eq!(recovery.len(), 1);
        assert_eq!(fs::read(&recovery[0]).unwrap(), tail);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(&recovery[0]).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn failed_tail_preservation_keeps_original_log_and_grants_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ApprovalReceiptStore::new(tmp.path().join("sessions"));
        let ask = ApprovalReceipt::asked("tool-pending", "exec_shell");
        store.append("session-preserve-error", &ask).unwrap();
        let path = store.log_path("session-preserve-error").unwrap();
        let mut original = fs::read(&path).unwrap();
        original.extend_from_slice(b"{\"phase\":\"decided\"");
        fs::write(&path, &original).unwrap();
        let decision = ApprovalReceipt::decided("tool-pending", ApprovalOutcome::ApprovedOnce);
        let error = store
            .append_with_preservation("session-preserve-error", &decision, |_, _| {
                Err(io::Error::other("simulated preservation failure"))
            })
            .unwrap_err();
        assert_eq!(error.to_string(), "simulated preservation failure");
        assert_eq!(fs::read(&path).unwrap(), original);
        let replay = store.replay("session-preserve-error").unwrap();
        assert!(replay.completed.is_empty());
        assert_eq!(replay.unmatched_asks, vec![ask]);
    }

    #[test]
    fn invalid_candidate_cannot_trigger_tail_repair() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ApprovalReceiptStore::new(tmp.path().join("sessions"));
        let ask = ApprovalReceipt::asked("tool-pending", "exec_shell");
        store.append("session-invalid", &ask).unwrap();
        let path = store.log_path("session-invalid").unwrap();
        let mut original = fs::read(&path).unwrap();
        original.extend_from_slice(b"{\"phase\":\"decided\"");
        fs::write(&path, &original).unwrap();
        assert!(
            store
                .append_with_preservation("session-invalid", &ask, |_, _| {
                    panic!("invalid duplicate ask must fail before preserving or truncating")
                })
                .is_err()
        );
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[test]
    fn complete_unterminated_receipts_survive_and_get_an_append_separator() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ApprovalReceiptStore::new(tmp.path().join("sessions"));
        let receipts = completed_receipts(ApprovalOutcome::Denied);
        for count in 1..=receipts.len() {
            let id = format!("session-no-lf-{count}");
            let path = store.log_path(&id).unwrap();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let original = receipts[..count]
                .iter()
                .map(|receipt| serde_json::to_string(receipt).unwrap())
                .collect::<Vec<_>>()
                .join("\n");
            fs::write(&path, &original).unwrap();
            assert_eq!(store.load(&id).unwrap(), receipts[..count]);
            let next = ApprovalReceipt::asked("tool-next", "read_file");
            store.append(&id, &next).unwrap();
            let mut expected = receipts[..count].to_vec();
            expected.push(next);
            assert_eq!(store.load(&id).unwrap(), expected);
            assert!(
                fs::read_to_string(path)
                    .unwrap()
                    .starts_with(&format!("{original}\n"))
            );
        }
    }

    #[test]
    fn malformed_complete_lines_and_invalid_receipts_still_fail_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ApprovalReceiptStore::new(tmp.path().join("sessions"));
        let ask = ApprovalReceipt::asked("tool-1", "exec_shell");
        let line = serde_json::to_string(&ask).unwrap();
        let orphan = ApprovalReceipt::decided("tool-orphan", ApprovalOutcome::ApprovedOnce);
        for (index, original) in [
            format!("{line}\n{{broken\n"),
            format!("{line}\n{{broken\n{line}\n"),
            format!("{line}\n{line}"),
            "{}".to_string(),
            serde_json::to_string(&ApprovalReceipt::asked("", "exec_shell")).unwrap(),
            serde_json::to_string(&orphan).unwrap(),
        ]
        .into_iter()
        .enumerate()
        {
            let id = format!("session-invalid-{index}");
            let path = store.log_path(&id).unwrap();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, &original).unwrap();
            assert!(store.load(&id).is_err(), "case {index}");
            assert!(
                store
                    .append(&id, &ApprovalReceipt::asked("tool-new", "read_file"))
                    .is_err()
            );
            assert_eq!(fs::read_to_string(path).unwrap(), original);
        }
    }

    #[test]
    fn store_serializes_competing_writers() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = ApprovalReceiptStore::new(tmp.path().join("sessions"));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let mut writers = Vec::new();

        for _ in 0..8 {
            let store = store.clone();
            let barrier = barrier.clone();
            writers.push(std::thread::spawn(move || {
                barrier.wait();
                store.append(
                    "session-race",
                    &ApprovalReceipt::asked("tool-race", "exec_shell"),
                )
            }));
        }

        let results = writers
            .into_iter()
            .map(|writer| writer.join().expect("writer thread"))
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(results.iter().filter(|result| result.is_err()).count(), 7);
        assert_eq!(store.load("session-race").expect("load receipts").len(), 1);
    }
}
