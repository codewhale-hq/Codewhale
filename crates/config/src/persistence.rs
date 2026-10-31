//! Transactional persistence, atomic writes, and secret redaction for the
//! v0.8.67 constitution-first setup lane (#3410).
//!
//! This is the safety layer under every setup step. A setup session may touch
//! several files (the setup-state sidecar, the user-global constitution, and —
//! through the existing comment-preserving `ConfigStore` — `config.toml`). The
//! contract this module guarantees:
//!
//! - **Preview writes nothing.** [`SetupTransaction::preview`] reports what
//!   would change without touching the filesystem.
//! - **Cancel leaves files unchanged.** A staged transaction that is dropped
//!   without [`SetupTransaction::commit`] never wrote anything.
//! - **Save is atomic.** Each file is written through a temp file + rename
//!   ([`atomic_write`]); a multi-file commit either fully applies or fully
//!   rolls back, so a partial failure never leaves a half-written file.
//! - **Secrets never leak.** [`redact_secrets`] masks secret-bearing values for
//!   any report, log line, or diagnostic that might echo config text.
//!
//! This module deliberately owns only the write / rollback / secret contract.
//! Each setup step owns *which* fields it writes; see [`crate::setup_state`] and
//! [`crate::user_constitution`].

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

/// Restrictive file mode for setup-owned files (owner read/write only).
#[cfg(unix)]
const SETUP_FILE_MODE: u32 = 0o600;

/// Atomically write `bytes` to `path` via a sibling temp file + rename.
///
/// The temp file is created in the same directory as `path` so the final
/// `rename` is atomic on the same filesystem. On Unix the file is created with
/// `0o600` so setup-owned state never lands world-readable. Parent directories
/// are created as needed.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    if let Some(parent) = parent {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }

    let dir = parent.unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir)
        .with_context(|| format!("failed to create temp file in {}", dir.display()))?;

    use std::io::Write as _;
    tmp.write_all(bytes)
        .with_context(|| format!("failed to write temp file for {}", path.display()))?;
    tmp.flush()
        .with_context(|| format!("failed to flush temp file for {}", path.display()))?;

    // A filesystem that assigns modes itself (HarmonyOS hmdfs) refuses the
    // call outright; `tighten_mode` tolerates only that case and reports every
    // other failure. The temp file sits in `path`'s directory, which is where
    // the question about the mount is answered.
    #[cfg(unix)]
    crate::private_directory::tighten_mode_of(tmp.as_file(), tmp.path(), SETUP_FILE_MODE)
        .with_context(|| format!("failed to set permissions for {}", path.display()))?;

    tmp.as_file()
        .sync_all()
        .with_context(|| format!("failed to sync temp file for {}", path.display()))?;
    tmp.persist(path)
        .map_err(|e| e.error)
        .with_context(|| format!("failed to persist {}", path.display()))?;
    Ok(())
}

/// Atomically write `value` as pretty-printed JSON to `path`.
///
/// A trailing newline is appended so the file is well-formed for line-oriented
/// tooling and diffs.
pub fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut body = serde_json::to_string_pretty(value)
        .with_context(|| format!("failed to serialize JSON for {}", path.display()))?;
    body.push('\n');
    atomic_write(path, body.as_bytes())
}

/// A staged multi-file write that either fully applies or fully rolls back.
///
/// Stage every file the setup step intends to write, then call [`commit`]. If
/// any single write fails, every already-applied write in the transaction is
/// restored to its pre-commit contents (or removed if it did not previously
/// exist), and the original error is returned. A transaction that is dropped
/// without committing leaves the filesystem untouched.
///
/// [`commit`]: SetupTransaction::commit
#[derive(Debug, Default)]
pub struct SetupTransaction {
    writes: Vec<StagedWrite>,
}

#[derive(Debug, Clone)]
struct StagedWrite {
    path: PathBuf,
    bytes: Vec<u8>,
}

/// A snapshot of a file's pre-commit state, captured so [`SetupTransaction`]
/// can restore it during rollback.
struct Snapshot {
    path: PathBuf,
    /// Original bytes, or `None` if the file did not exist before commit.
    original: Option<Vec<u8>>,
}

impl SetupTransaction {
    /// Create an empty transaction.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Stage `bytes` to be written to `path` on [`commit`](Self::commit).
    ///
    /// Staging touches nothing on disk. A later stage for the same path
    /// replaces an earlier one, so a step can revise its intended output before
    /// committing.
    pub fn stage(&mut self, path: impl Into<PathBuf>, bytes: impl Into<Vec<u8>>) -> &mut Self {
        let path = path.into();
        let bytes = bytes.into();
        if let Some(existing) = self.writes.iter_mut().find(|w| w.path == path) {
            existing.bytes = bytes;
        } else {
            self.writes.push(StagedWrite { path, bytes });
        }
        self
    }

    /// Stage `value` serialized as pretty JSON (with trailing newline).
    pub fn stage_json<T: Serialize>(
        &mut self,
        path: impl Into<PathBuf>,
        value: &T,
    ) -> Result<&mut Self> {
        let path = path.into();
        let mut body = serde_json::to_string_pretty(value)
            .with_context(|| format!("failed to serialize JSON for {}", path.display()))?;
        body.push('\n');
        Ok(self.stage(path, body.into_bytes()))
    }

    /// The paths that [`commit`](Self::commit) would write, in staging order.
    /// Writes nothing — this is the preview surface.
    #[must_use]
    pub fn preview(&self) -> Vec<&Path> {
        self.writes.iter().map(|w| w.path.as_path()).collect()
    }

    /// True when nothing is staged.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.writes.is_empty()
    }

    /// Apply every staged write atomically.
    ///
    /// On success all files are updated. On the first failure, every write that
    /// already landed is rolled back to its captured pre-commit state and the
    /// original error is returned (rollback failures are attached as context).
    pub fn commit(self) -> Result<()> {
        let mut snapshots: Vec<Snapshot> = Vec::with_capacity(self.writes.len());

        for write in &self.writes {
            // Capture the pre-commit state before mutating, so we can restore it.
            let original = match fs::read(&write.path) {
                Ok(bytes) => Some(bytes),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => {
                    rollback(&snapshots);
                    return Err(e).with_context(|| {
                        format!(
                            "failed to read existing {} before write; rolled back {} prior change(s)",
                            write.path.display(),
                            snapshots.len()
                        )
                    });
                }
            };

            match atomic_write(&write.path, &write.bytes) {
                Ok(()) => snapshots.push(Snapshot {
                    path: write.path.clone(),
                    original,
                }),
                Err(err) => {
                    // This write did not land (atomic_write is all-or-nothing),
                    // so roll back only the writes that came before it.
                    rollback(&snapshots);
                    return Err(err).with_context(|| {
                        format!(
                            "setup transaction failed writing {}; rolled back {} prior change(s)",
                            write.path.display(),
                            snapshots.len()
                        )
                    });
                }
            }
        }

        Ok(())
    }
}

/// Restore every snapshot to its captured pre-commit state. Best-effort: a
/// rollback error is logged but does not abort the remaining restores, because
/// leaving as many files as possible in their original state is the goal.
fn rollback(snapshots: &[Snapshot]) {
    for snap in snapshots.iter().rev() {
        let result = match &snap.original {
            Some(bytes) => atomic_write(&snap.path, bytes),
            None => match fs::remove_file(&snap.path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.into()),
            },
        };
        if let Err(e) = result {
            tracing::error!(
                target: "config::persistence",
                "failed to roll back {} during setup transaction: {e:#}",
                snap.path.display()
            );
        }
    }
}

// FEAT-025 D4: the pure secret-redaction primitives moved to
// `codewhale-secrets::redact` so portable command helpers can share one
// implementation without depending on this crate. Re-exported here to keep
// the existing `codewhale_config::persistence::*` public API stable.
pub use codewhale_secrets::redact::{
    REDACTED, RedactionPolicy, redact_json_model_bound_secrets, redact_json_secrets,
    redact_model_bound_secrets, redact_secrets, redact_secrets_with,
};

#[cfg(test)]
mod tests {
    use super::*;

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap()
    }

    fn synthetic_secret_fixture() -> String {
        ["abc123", "def456", "ghi"].concat()
    }

    #[test]
    fn atomic_write_creates_parent_dirs_and_content() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested/dir/state.json");
        atomic_write(&path, b"hello").unwrap();
        assert_eq!(read(&path), "hello");
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_uses_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt as _;

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state.json");
        atomic_write(&path, b"x").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, SETUP_FILE_MODE);
    }

    #[test]
    fn atomic_write_replaces_existing_atomically() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state.json");
        atomic_write(&path, b"old").unwrap();
        atomic_write(&path, b"new").unwrap();
        assert_eq!(read(&path), "new");
        // No stray temp files left behind.
        let leftovers: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name() != "state.json")
            .collect();
        assert!(leftovers.is_empty(), "stray temp files: {leftovers:?}");
    }

    #[test]
    fn transaction_preview_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.json");
        let b = tmp.path().join("b.json");
        let mut tx = SetupTransaction::new();
        tx.stage(a.clone(), b"1".to_vec())
            .stage(b.clone(), b"2".to_vec());
        let preview = tx.preview();
        assert_eq!(preview, vec![a.as_path(), b.as_path()]);
        assert!(!a.exists());
        assert!(!b.exists());
    }

    #[test]
    fn dropped_transaction_leaves_files_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.json");
        {
            let mut tx = SetupTransaction::new();
            tx.stage(a.clone(), b"staged".to_vec());
            // tx dropped here without commit
        }
        assert!(!a.exists());
    }

    #[test]
    fn transaction_commit_applies_all() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.json");
        let b = tmp.path().join("sub/b.json");
        let mut tx = SetupTransaction::new();
        tx.stage(a.clone(), b"A".to_vec())
            .stage(b.clone(), b"B".to_vec());
        tx.commit().unwrap();
        assert_eq!(read(&a), "A");
        assert_eq!(read(&b), "B");
    }

    #[test]
    fn transaction_rolls_back_on_partial_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let good = tmp.path().join("good.json");
        fs::write(&good, "ORIGINAL").unwrap();

        // Second target is unwritable: a path whose parent is an existing file.
        let blocker = tmp.path().join("blocker");
        fs::write(&blocker, "i am a file").unwrap();
        let bad = blocker.join("child.json"); // parent is a file → create_dir_all fails

        let mut tx = SetupTransaction::new();
        tx.stage(good.clone(), b"UPDATED".to_vec())
            .stage(bad.clone(), b"NOPE".to_vec());
        let err = tx.commit().unwrap_err();
        assert!(format!("{err:#}").contains("rolled back"));

        // The first file must be restored to its original contents.
        assert_eq!(read(&good), "ORIGINAL");
        assert!(!bad.exists());
    }

    #[test]
    fn transaction_rollback_removes_newly_created_file() {
        let tmp = tempfile::tempdir().unwrap();
        let fresh = tmp.path().join("fresh.json"); // did not exist before
        let blocker = tmp.path().join("blocker");
        fs::write(&blocker, "file").unwrap();
        let bad = blocker.join("child.json");

        let mut tx = SetupTransaction::new();
        tx.stage(fresh.clone(), b"created".to_vec())
            .stage(bad, b"x".to_vec());
        assert!(tx.commit().is_err());
        // The newly created file must be removed on rollback, not left behind.
        assert!(!fresh.exists());
    }

    #[test]
    fn compact_json_and_query_strings_mask_their_credentials() {
        // A synthetic, unsigned test value, assembled from its three
        // segments so the source holds no complete token literal.
        let jwt = [
            "eyJhbGciOiJSUzI1NiJ9",
            "eyJzdWIiOiJ1c2VyIn0",
            "c2lnbmF0dXJlLXZhbHVl",
        ]
        .join(".");
        let jwt = jwt.as_str();
        let refresh = "rt_Q2x9fK3mZ7pL1vB8nH4sT6wY0aE5cR2d";
        let cases = [
            format!(
                r#"{{"tokens":{{"id_token":"{jwt}","access_token":"{jwt}","refresh_token":"{refresh}"}}}}"#
            ),
            format!(r#"{{"token_type":"Bearer","access_token":"{jwt}"}}"#),
            format!(r#"[{{"Authorization":"Bearer {jwt}"}}]"#),
            format!("https://example.test/cb?state=abc&access_token={jwt}&x=1"),
        ];
        for policy in [RedactionPolicy::CredentialShaped, RedactionPolicy::KeyBased] {
            for case in &cases {
                let out = redact_secrets_with(case, policy);
                assert!(!out.contains(jwt), "{policy:?} leaked a JWT: {out}");
                assert!(
                    !out.contains(refresh),
                    "{policy:?} leaked a refresh token: {out}"
                );
            }
        }
        // Structure and harmless members stay byte-exact.
        assert_eq!(
            redact_model_bound_secrets(&cases[1]),
            r#"{"token_type":"Bearer","access_token":"[redacted]"}"#
        );
        let json = serde_json::json!({
            "summary": format!("{{\"access_token\":\"{jwt}\"}}"),
            "stdout_summary": "token = make_token()",
            "api_key": "short",
            "access_token": jwt,
        });
        let masked = redact_json_model_bound_secrets(&json);
        assert!(!masked.to_string().contains(jwt), "{masked}");
        assert_eq!(masked["stdout_summary"], "token = make_token()");
        // A sensitive key's value is masked wholesale, credential-shaped or not.
        assert_eq!(masked["api_key"], REDACTED);
    }

    #[test]
    fn model_bound_redaction_keeps_code_and_config_byte_exact() {
        // Every line here is code or configuration that a model must be able
        // to quote back for an exact-match edit (#5546).
        let lines = [
            "    \"jsonwebtoken\": \"^9.0.2\",",
            "    \"@types/jsonwebtoken\": \"^9.0.5\",",
            "    \"password-validator\": \"^5.3.0\",",
            "    \"authorization\": \"1.0.0\",",
            "      password: credentials?.password,",
            "    token = generate_verification_token()",
            "  secret: process.env.NEXTAUTH_SECRET!,",
            "  password?: string;",
            "  token: string;",
            "    \"tokenizer\": \"gpt2\",",
            "max tokens = 8192",
            "export const AUTH_TOKEN_HEADER = 'x-auth-token';",
            "{\"id\":1, \"password\": \"x\", \"language\": \"en\"}",
            "{\"id\":1, \"password\":\"x\", \"language\":\"en\"}",
            "password = hunter2",
            "api_key = os.environ[\"OPENAI_API_KEY\"]",
            "let token = ${TOKEN_FROM_ENV}",
            "auth_url = https://example.test/oauth/token",
        ];
        for line in lines {
            assert_eq!(
                redact_model_bound_secrets(line),
                line,
                "line changed: {line}"
            );
        }
        let file = lines.join("\n");
        assert_eq!(redact_model_bound_secrets(&file), file);
    }

    #[test]
    fn model_bound_redaction_masks_credential_shaped_values() {
        let hex40 = ["0123456789abcdef", "0123456789abcdef", "01234567"].concat();
        let sk = ["sk-", "abcdef1234567890abcdef"].concat();
        let jwt = [
            "eyJhbGciOiJIUzI1NiJ9",
            ".",
            "eyJzdWIiOiIxMjM0NTY3ODkwIn0",
            ".",
            "c2lnbmF0dXJlLXNpZ25hdHVyZQ",
        ]
        .concat();
        let ya = ["ya29.", "a0AfH6SMBx1234567890abcdefghij"].concat();
        let cases = [
            (
                format!("NEXTAUTH_SECRET={hex40}"),
                "NEXTAUTH_SECRET=[redacted]".to_string(),
            ),
            (
                format!("api_key = \"{sk}\""),
                "api_key = \"[redacted]\"".to_string(),
            ),
            (
                format!("  \"access_token\": \"{ya}\","),
                "  \"access_token\": \"[redacted]\",".to_string(),
            ),
            (
                format!("Authorization: Bearer {jwt}"),
                "Authorization: [redacted]".to_string(),
            ),
            (
                format!("curl -H \"Authorization: Bearer {jwt}\" https://api.test"),
                "curl -H \"Authorization: Bearer [redacted]\" https://api.test".to_string(),
            ),
            (
                format!("password = {hex40}, retries = 3"),
                "password = [redacted], retries = 3".to_string(),
            ),
            (
                format!("found key {sk} in the log"),
                "found key [redacted] in the log".to_string(),
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(
                redact_model_bound_secrets(&input),
                expected,
                "input: {input}"
            );
        }
    }

    #[test]
    fn private_key_blocks_are_masked_between_pem_markers() {
        // Assemble the PEM markers at runtime so the source file never
        // contains a literal private-key header for a scanner to match; the
        // runtime strings are identical to a real block.
        let begin = ["-----BEGIN RSA", " PRIVATE KEY-----"].concat();
        let end = ["-----END RSA", " PRIVATE KEY-----"].concat();
        let body = "MIIEpAIBAAKCAQEA0Z3VS5JJcds3xfn\nabcdefghijklmnopqrstuvwxyz012345";
        let pem = format!("{begin}\n{body}\n{end}\nnext_line = ok\n");
        let expected = format!("{begin}\n[redacted]\n[redacted]\n{end}\nnext_line = ok\n");
        assert_eq!(redact_model_bound_secrets(&pem), expected);
        assert_eq!(redact_secrets(&pem), expected);
    }

    #[test]
    fn key_based_policy_is_unchanged_by_the_model_bound_mode() {
        // The broad scrubber for logs/previews keeps masking key-only hits.
        assert_eq!(
            redact_secrets("      password: credentials?.password,"),
            "      password: [redacted]"
        );
        assert_eq!(
            redact_secrets("    \"password-validator\": \"^5.3.0\","),
            "    \"password-validator\": \"[redacted]\""
        );
    }

    #[test]
    fn redact_masks_keyed_secrets_toml_and_json() {
        let synthetic_secret = synthetic_secret_fixture();
        let input = format!(
            "\
api_key = \"sk-supersecretvalue123\"
provider = \"openai\"
  \"token\": \"{synthetic_secret}\",
model = \"mimo-ultraspeed\"
PASSWORD=hunter2hunter2"
        );
        let out = redact_secrets(&input);
        assert!(!out.contains("sk-supersecretvalue123"), "{out}");
        assert!(!out.contains(&synthetic_secret), "{out}");
        assert!(!out.contains("hunter2hunter2"), "{out}");
        // Non-secret values survive untouched.
        assert!(out.contains("provider = \"openai\""));
        assert!(out.contains("model = \"mimo-ultraspeed\""));
        assert!(out.matches(REDACTED).count() >= 3, "{out}");
    }

    #[test]
    fn redact_json_masks_camel_case_and_dotted_secret_keys() {
        let synthetic_secret = synthetic_secret_fixture();
        let input = serde_json::json!({
            "accessToken": synthetic_secret.clone(),
            "refreshToken": synthetic_secret_fixture(),
            "oauth.token": synthetic_secret_fixture(),
            "APIKey": synthetic_secret_fixture(),
            "maxTokens": 8192,
            "tokenBudget": 4096,
            "tokenCount": 1024,
            "token_count": 512,
            "tokenizer": "sentencepiece",
        });

        let out = redact_json_secrets(&input);
        for key in ["accessToken", "refreshToken", "oauth.token", "APIKey"] {
            assert_eq!(out[key], REDACTED, "{key}: {out}");
        }
        assert_eq!(out["maxTokens"], 8192);
        assert_eq!(out["tokenBudget"], 4096);
        assert_eq!(out["tokenCount"], 1024);
        assert_eq!(out["token_count"], 512);
        assert_eq!(out["tokenizer"], "sentencepiece");
        assert!(!out.to_string().contains(&synthetic_secret), "{out}");
    }

    #[test]
    fn redact_json_truncates_pathological_nesting() {
        let mut value = serde_json::Value::String("leaf".to_string());
        for _ in 0..150 {
            let mut map = serde_json::Map::new();
            map.insert("t".to_string(), value);
            value = serde_json::Value::Object(map);
        }
        let out = redact_json_secrets(&value);
        let mut cursor = &out;
        let mut descended = 0;
        while let serde_json::Value::Object(map) = cursor {
            cursor = map.values().next().expect("single-key nesting");
            descended += 1;
        }
        assert_eq!(
            cursor,
            &serde_json::Value::String(REDACTED.to_string()),
            "over-deep value must be redacted, not traversed"
        );
        assert!(descended < 150, "guard must fire before the leaf");
    }

    #[test]
    fn redact_text_masks_camel_case_and_dotted_secret_assignments() {
        let synthetic_secret = synthetic_secret_fixture();
        for key in ["accessToken", "refreshToken", "oauth.token", "APIKey"] {
            let out = redact_secrets(&format!("request failed: {key} = {synthetic_secret}"));
            assert!(!out.contains(&synthetic_secret), "{key}: {out}");
            assert!(out.contains(REDACTED), "{key}: {out}");
        }
        for key in ["tokenBudget", "tokenCount", "token_count"] {
            let input = format!("model usage: {key} = 8192");
            assert_eq!(redact_secrets(&input), input, "{key}");
        }
    }

    #[test]
    fn redact_masks_bare_token_prefixes() {
        let out = redact_secrets("the leaked key sk-abcdef1234567890 appeared in a log");
        assert!(!out.contains("sk-abcdef1234567890"), "{out}");
        assert!(out.contains(REDACTED));
        assert!(out.contains("appeared in a log"));
    }

    #[test]
    fn redact_masks_inline_sensitive_assignments_after_prose_prefixes() {
        let out = redact_secrets(
            "Decision: use token=plain-secret-value and api_key:another-secret-value",
        );
        assert!(!out.contains("plain-secret-value"), "{out}");
        assert!(!out.contains("another-secret-value"), "{out}");
        assert_eq!(out.matches(REDACTED).count(), 2, "{out}");
        assert!(out.starts_with("Decision: use "), "{out}");
    }

    #[test]
    fn redact_masks_spaced_assignment_that_is_not_the_first_separator() {
        // The shape `redact_secrets(&format!("{error:#}"))` produces: an
        // anyhow chain puts prose and its own `: ` separators in front of the
        // assignment, so the sensitive key never owns the line's first
        // separator and the whole-line pass declines the line.
        let out = redact_secrets("request failed: api_key = AIzaSyDeadBeefLeak");
        assert!(!out.contains("AIzaSyDeadBeefLeak"), "{out}");
        assert!(out.contains(REDACTED), "{out}");

        let synthetic_secret = synthetic_secret_fixture();
        let out = redact_secrets(&format!("note: the token = {synthetic_secret}"));
        assert!(!out.contains(&synthetic_secret), "{out}");
        assert!(out.contains(REDACTED), "{out}");
    }

    #[test]
    fn redact_masks_whole_multi_word_value_of_a_spaced_assignment() {
        // Assemble the placeholder at runtime so secret scanners do not
        // mistake a redaction fixture for a committed credential.
        let bearer = ["Bear", "er"].concat();
        let credential = ["abc123", "def456", "ghi"].concat();
        let out = redact_secrets(&format!(
            "mcp call failed: authorization = {bearer} {credential}"
        ));
        assert!(!out.contains(&credential), "{out}");
        assert!(!out.contains(&bearer), "{out}");
        assert!(
            out.starts_with("mcp call failed: authorization = "),
            "{out}"
        );
    }

    #[test]
    fn redact_spaced_pass_leaves_ordinary_prose_alone() {
        // No sensitive key, so the spaced-assignment state machine must not
        // start swallowing the rest of the line.
        let input = "the quick brown fox = jumps over the lazy dog";
        assert_eq!(redact_secrets(input), input);
        let input = "note: the model = deepseek-v4-pro and the seed = 7";
        assert_eq!(redact_secrets(input), input);
    }

    #[test]
    fn redact_leaves_token_count_diagnostics_intact() {
        // "tokens" is the English plural of a usage metric, not a credential
        // key. The spaced-assignment pass used to treat the "token" hint as a
        // substring and then drop the rest of the line, which made the exact
        // class of error people paste into issues unreadable.
        for input in [
            "stream error: max tokens = 8192 but budget = 4096",
            "error: token expired",
            "request failed: token count: 4096 exceeds the model limit",
            "http 401: authorization header rejected",
            "warning: password policy requires 12 characters",
            "note: secret scanning found 3 issues",
        ] {
            assert_eq!(redact_secrets(input), input, "{input}");
        }
    }

    #[test]
    fn redact_keeps_a_provider_refusal_reason_readable() {
        // xAI answers an exhausted account with a 403 whose body is the whole
        // explanation. The `Authorization failed:` prefix is our label, not a
        // header, so the reason after it must survive to the error card.
        let input = "Authorization failed: You have run out of credits or need a Grok \
                     subscription. Add credits at https://grok.com/?_s=usage.";
        assert_eq!(redact_secrets(input), input);
    }

    #[test]
    fn redact_still_masks_a_bearer_token_assignment() {
        // Counterpart of the diagnostic test above: a real credential keyed
        // as `token` (or `api_token`) must still be dropped, including a
        // multi-word Bearer value that is not a known bare-token prefix.
        // The JWT is assembled at runtime so no scanner-shaped literal sits
        // in the source tree — same precedent as the AWS fixture in
        // `crates/workflow/src/redaction.rs`.
        let jwt = ["eyJhbGciOiJIUzI1NiJ9", "e30", "c2lnbmF0dXJl"].join(".");
        let out = redact_secrets(&format!("stream error: token = Bearer {jwt}"));
        assert!(!out.contains(&jwt), "{out}");
        assert!(!out.contains("Bearer"), "{out}");
        assert!(out.starts_with("stream error: token = "), "{out}");
        assert!(out.contains(REDACTED), "{out}");

        let synthetic_secret = synthetic_secret_fixture();
        let out = redact_secrets(&format!("note: api_token = {synthetic_secret}"));
        assert!(!out.contains(&synthetic_secret), "{out}");
        assert!(out.contains(REDACTED), "{out}");
    }

    #[test]
    fn redact_preserves_line_structure() {
        let input = "line1\nsecret = \"xyzsecretvalue\"\nline3";
        let out = redact_secrets(input);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "line1");
        assert_eq!(lines[2], "line3");
        assert!(lines[1].contains(REDACTED));
    }

    #[test]
    fn redact_leaves_plain_text_untouched() {
        let input = "the quick brown fox = jumps over";
        // `fox` key has no sensitive hint → unchanged.
        assert_eq!(redact_secrets(input), input);
    }
}
