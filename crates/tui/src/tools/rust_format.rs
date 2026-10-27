//! Post-edit formatting normalization for Rust files (#6205).
//!
//! Model-generated edits rarely match `rustfmt` output exactly. Once an
//! unformatted edit lands, the *next* turn's `old_string` or patch context was
//! written against text that `cargo fmt` is about to move, so anchors drift and
//! the follow-up edit fails to match. Normalizing at edit time keeps anchors
//! stable for the rest of the session, and the tool result returns the
//! normalized text, so what the model remembers writing is what is on disk.
//!
//! `rustfmt` is the formatter, not `prettyplease`: `cargo fmt --check` is this
//! repository's actual gate, and a second formatter with its own opinions would
//! produce files that pass the edit path and fail the gate. Shelling out also
//! picks up the project's own `rustfmt.toml` — an in-process pretty-printer
//! cannot.
//!
//! # Policy
//!
//! Normalization applies to the whole edited file, and **only when that file
//! was already `rustfmt`-clean before the edit**. A file whose formatting the
//! author has not handed to `rustfmt` is never rewritten. Because a clean file
//! is a formatting fixpoint, reformatting it after an edit can only change the
//! edited region — so "whole file" and "edited region" coincide, without
//! needing span arithmetic to prove it.
//!
//! # Known limitations
//!
//! - **Non-fatal, always.** A missing `rustfmt`, a parse failure, a timeout, or
//!   a non-zero exit skips normalization; the edit still lands. Nothing here
//!   can fail an edit.
//! - **Silent when skipped.** Only an applied normalization is announced. A
//!   "formatting skipped" note on every edit in a project without `rustfmt`
//!   would be noise the model cannot act on, and "skipped" is indistinguishable
//!   from "would have changed nothing" without running the formatter anyway.
//! - **Edition 2024 is assumed.** A file that `rustfmt` cannot parse under that
//!   edition fails the clean-before check and is skipped, so the assumption
//!   degrades to "no normalization", never to a mangled file.
//! - **CRLF files are skipped.** `rustfmt` emits LF; rewriting every line
//!   ending is exactly the unrelated-churn this policy exists to avoid.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Files larger than this are not normalized. Two formatter runs on a
/// multi-megabyte file cost more interactive latency than stable anchors are
/// worth.
const MAX_FORMATTED_BYTES: usize = 1024 * 1024;

/// Wall-clock budget for one `rustfmt` run.
#[cfg(not(test))]
const FORMAT_TIMEOUT: Duration = Duration::from_secs(5);
/// Unit tests assert that normalization *happens*, not how fast. A loaded
/// Windows CI runner has taken over five seconds to start the rustup
/// `rustfmt` proxy, which silently skipped normalization and failed the
/// assertion. The skip-on-timeout path is unchanged; only the budget is.
#[cfg(test)]
const FORMAT_TIMEOUT: Duration = Duration::from_secs(60);

/// Suffix appended to an edit summary when the content was normalized, so the
/// model knows the returned text is not byte-identical to what it sent.
pub(super) const NORMALIZED_NOTE: &str = " (rustfmt-normalized)";

/// Normalize `after` when the edit landed in an already-`rustfmt`-clean file.
///
/// Returns the formatted text to write instead of `after`, or `None` to leave
/// `after` exactly as the caller produced it. Every failure path returns
/// `None`: normalization is a convenience and may never break an edit.
pub(super) async fn normalize_edit(path: &Path, before: &str, after: &str) -> Option<String> {
    if path.extension()?.to_str()? != "rs" {
        return None;
    }
    if after.len() > MAX_FORMATTED_BYTES || before.len() > MAX_FORMATTED_BYTES {
        return None;
    }
    // Preserving a CRLF file's line endings outranks normalizing its layout.
    if after.contains('\r') || before.contains('\r') {
        return None;
    }

    let formatted = rustfmt(path, after).await?;
    if formatted == after {
        // Already canonical. The common case, and it costs one run, not two.
        return None;
    }
    // Only now is the second run worth paying for: was this file the author's
    // to format, or `rustfmt`'s?
    if rustfmt(path, before).await? != before {
        return None;
    }
    Some(formatted)
}

/// Find the `rustfmt.toml` governing `path`, walking up to the filesystem root.
///
/// `None` means no config file exists, which is the signal to omit
/// `--config-path` entirely and let `rustfmt` use its defaults.
async fn nearest_config(path: &Path) -> Option<PathBuf> {
    let mut directory = path.parent()?;
    loop {
        for name in ["rustfmt.toml", ".rustfmt.toml"] {
            let candidate = directory.join(name);
            if tokio::fs::try_exists(&candidate).await.unwrap_or(false) {
                return Some(candidate);
            }
        }
        directory = directory.parent()?;
    }
}

/// Run `rustfmt` over `source`, returning its output, or `None` on any failure.
async fn rustfmt(path: &Path, source: &str) -> Option<String> {
    let mut command = Command::new("rustfmt");
    codewhale_runtime::child_env::apply_to_tokio_command(
        &mut command,
        std::iter::empty::<(&str, &str)>(),
    );
    command
        .arg("--emit")
        .arg("stdout")
        .arg("--edition")
        .arg("2024")
        .arg("--quiet");
    // Pick up the project's own `rustfmt.toml`: with stdin input there is no
    // file path for `rustfmt` to search upward from. The flag must name a
    // config file that actually exists — pointed at a directory without one,
    // `rustfmt` exits 1 with "unable to find a config file", which would
    // silently disable normalization everywhere.
    if let Some(config) = nearest_config(path).await {
        command.arg("--config-path").arg(config);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // A hung formatter must not outlive the edit that spawned it.
        .kill_on_drop(true)
        .spawn()
        .ok()?;

    let mut stdin = child.stdin.take()?;
    let payload = source.to_string();
    // Write and wait concurrently: `rustfmt` streams its output, so writing the
    // whole input before reading can deadlock on a full pipe buffer.
    let writer = tokio::spawn(async move {
        let _ = stdin.write_all(payload.as_bytes()).await;
        let _ = stdin.shutdown().await;
    });

    let output = match tokio::time::timeout(FORMAT_TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        // A timed-out child is already killed by dropping the future's handle
        // on the `wait_with_output` path; either way the edit proceeds.
        Ok(Err(_)) | Err(_) => {
            writer.abort();
            return None;
        }
    };
    writer.abort();

    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn rust_path() -> PathBuf {
        PathBuf::from("src/lib.rs")
    }

    /// `rustfmt` ships with the toolchain this repository pins (rustup's
    /// default profile), and `cargo fmt --check` is a standing gate here, so
    /// its absence is a broken environment, not a reason to skip. A test that
    /// passes vacuously without the formatter proves nothing — that exact
    /// hazard hid a `--config-path` bug which disabled normalization
    /// everywhere while the suite stayed green.
    async fn require_rustfmt() {
        assert_eq!(
            rustfmt(&rust_path(), "fn  main( ) {}\n").await.as_deref(),
            Some("fn main() {}\n"),
            "rustfmt must be on PATH for the formatting tests"
        );
    }

    #[tokio::test]
    async fn misformatted_edit_in_a_clean_file_is_normalized() {
        require_rustfmt().await;
        let before = "fn main() {\n    let x = 1;\n}\n";
        let after = "fn main() {\n    let x  =  1;\n        let y=2;\n}\n";
        let normalized = normalize_edit(&rust_path(), before, after)
            .await
            .expect("a clean file must be renormalized after a sloppy edit");
        assert_eq!(
            normalized,
            "fn main() {\n    let x = 1;\n    let y = 2;\n}\n"
        );
    }

    #[tokio::test]
    async fn a_file_the_author_formats_by_hand_is_left_alone() {
        require_rustfmt().await;
        // `rustfmt` would rewrite this file wholesale, so it was never its to
        // format: the edit lands verbatim.
        let before = "fn main() {\n      let x = 1;\n}\n";
        let after = "fn main() {\n      let x = 1;\n        let y=2;\n}\n";
        assert!(normalize_edit(&rust_path(), before, after).await.is_none());
    }

    #[tokio::test]
    async fn already_canonical_content_needs_no_rewrite() {
        require_rustfmt().await;
        let before = "fn main() {\n    let x = 1;\n}\n";
        let after = "fn main() {\n    let x = 1;\n    let y = 2;\n}\n";
        assert!(normalize_edit(&rust_path(), before, after).await.is_none());
    }

    #[tokio::test]
    async fn crlf_files_keep_their_line_endings() {
        let before = "fn main() {\r\n    let x = 1;\r\n}\r\n";
        let after = "fn main() {\r\n    let x  =  1;\r\n}\r\n";
        assert!(normalize_edit(&rust_path(), before, after).await.is_none());
    }

    #[tokio::test]
    async fn non_rust_files_are_not_formatted() {
        assert!(
            normalize_edit(Path::new("data.json"), "{}", "{ }")
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn unparseable_content_degrades_to_no_normalization() {
        // `rustfmt` cannot parse this; the edit must still be allowed to land.
        assert!(
            normalize_edit(&rust_path(), "fn main() {}\n", "fn main( {\n")
                .await
                .is_none()
        );
    }
}
