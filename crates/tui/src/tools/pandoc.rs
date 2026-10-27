//! `pandoc_convert` tool — universal document conversion via the
//! `pandoc` binary (<https://pandoc.org>).
//!
//! Pandoc is the de-facto Swiss Army knife for moving prose between
//! the formats writers and engineers actually use: Markdown to HTML,
//! HTML to Markdown, anything to LaTeX or DOCX, RST to Markdown,
//! ReST imports, etc. Surfacing it as a model-callable tool unblocks
//! a large class of "rewrite this report as ..." / "publish this
//! changelog as ..." workflows that previously required the user
//! to drop into a terminal between turns.
//!
//! Registration is gated by [`codewhale_dependencies::resolve_pandoc`]
//! (see [`crate::tools::registry::ToolRegistryBuilder::with_pandoc_tools`]).
//! When pandoc isn't installed the tool simply doesn't appear in the
//! catalog, so the model never sees a binary it can't actually use.
//!
//! ## Format whitelist
//!
//! Pandoc supports ~30 input and ~50 output formats, and exposing
//! every one of them as a free-text string would let the model
//! ask for `pdf` (which needs LaTeX installed), `epub3` (works
//! everywhere but ambiguous vs. `epub`), or typos like `markown`.
//! The whitelist below is the curated subset that a) covers ~95%
//! of real document-handling needs and b) doesn't require additional
//! system dependencies (LaTeX engines, ImageMagick) beyond pandoc
//! itself.
//!
//! Adding a format: append to [`SUPPORTED_TARGET_FORMATS`] and the
//! schema description; the dispatch logic is whitelist-driven so
//! anything in the list goes through unchanged.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::process::Command as TokioCommand;

use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
    optional_str, required_str,
};

/// Curated whitelist of pandoc target formats. Each entry corresponds
/// to a `--to=<format>` value pandoc accepts natively without
/// additional system tooling. Keep this list short and intentional —
/// the schema description below references it verbatim.
pub(crate) const SUPPORTED_TARGET_FORMATS: &[&str] = &[
    "markdown",   // Pandoc-flavored Markdown (the safe round-trip default)
    "gfm",        // GitHub-Flavored Markdown
    "commonmark", // strict CommonMark
    "html",       // HTML5
    "rst",        // reStructuredText
    "latex",      // LaTeX source (does not require a TeX install to *generate*)
    "docx",       // Microsoft Word .docx
    "odt",        // OpenDocument Text
    "epub",       // EPUB 2/3
    "plain",      // plain text (formatting stripped)
    "asciidoc",   // AsciiDoc
];

/// Wall-clock bound for one pandoc conversion: a pathological document
/// (giant epub, pathological LaTeX) would otherwise hold the call for as
/// long as pandoc felt like taking. Mirrors the 600s interpreter budget
/// used by js_execution.
const PANDOC_TIMEOUT: Duration = Duration::from_secs(600);

/// Bound for the one-shot `pandoc --version` probe. A probe that cannot
/// finish in 10s means the binary itself is wedged; like an unparseable
/// banner, the gate then lets the conversion through and pandoc reports
/// any flag it does not know itself.
const PANDOC_VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Tool implementing `pandoc_convert`. Converts a source file into
/// a target format and either writes the output to disk or returns
/// the converted text inline.
pub struct PandocConvertTool;

#[async_trait]
impl ToolSpec for PandocConvertTool {
    fn name(&self) -> &'static str {
        "pandoc_convert"
    }

    fn description(&self) -> &'static str {
        "Convert a document between formats via pandoc. Reads `source_path` (any pandoc-supported input format — pandoc autodetects from extension), converts to `target_format`, and either writes the result to `output_path` (when provided) or returns the converted text inline. Supported targets: markdown, gfm, commonmark, html, rst, latex, docx, odt, epub, plain, asciidoc. Use this instead of shelling out to pandoc via `bash` — no approval prompt for output_path-less reads, structured errors, and a curated format whitelist."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "source_path": {
                    "type": "string",
                    "description": "Path to the source document (relative to workspace or absolute). Pandoc autodetects the input format from the file extension."
                },
                "target_format": {
                    "type": "string",
                    "description": "One of: markdown, gfm, commonmark, html, rst, latex, docx, odt, epub, plain, asciidoc.",
                    "enum": SUPPORTED_TARGET_FORMATS,
                },
                "output_path": {
                    "type": "string",
                    "description": "Optional path to write the converted document to. When omitted, the converted text is returned inline (text formats only — binary formats like docx/odt/epub require output_path)."
                }
            },
            "required": ["source_path", "target_format"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![
            ToolCapability::WritesFiles,
            ToolCapability::Sandboxable,
            ToolCapability::RequiresApproval,
        ]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Suggest
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let source_path_str = required_str(&input, "source_path")?;
        let target_format = required_str(&input, "target_format")?.trim().to_lowercase();
        let output_path_str = optional_str(&input, "output_path")?;

        if !SUPPORTED_TARGET_FORMATS.contains(&target_format.as_str()) {
            return Err(ToolError::invalid_input(format!(
                "unsupported target_format `{target_format}`. Pick one of: {}",
                SUPPORTED_TARGET_FORMATS.join(", ")
            )));
        }

        // pandoc reads the source in full and its output goes to the model,
        // so it takes the same read guards as `read`.
        let source_path = crate::tools::file::resolve_guarded_read_path(
            context,
            source_path_str,
            "pandoc_convert",
        )?;
        if !source_path.exists() {
            return Err(ToolError::execution_failed(format!(
                "source_path does not exist: {}",
                source_path.display()
            )));
        }

        let resolved_output_path: Option<PathBuf> = match output_path_str {
            Some(p) => Some(context.resolve_path(p)?),
            None => None,
        };

        // Binary formats can't round-trip through stdout reliably —
        // require an output_path so the bytes survive the trip.
        if resolved_output_path.is_none() && format_is_binary(&target_format) {
            return Err(ToolError::invalid_input(format!(
                "target_format `{target_format}` is binary; provide an `output_path` to write the converted file."
            )));
        }

        // Resolve the pandoc binary at execution time too — registration
        // gated on resolve_pandoc(), but a concurrent uninstall between
        // catalog build and the model's call should produce a clear
        // error rather than the cryptic "program not found" from raw
        // Command::spawn.
        let pandoc = codewhale_dependencies::resolve_pandoc().ok_or_else(|| {
            ToolError::execution_failed(
                "pandoc_convert: pandoc binary not found on PATH. \
                 Install pandoc 2.15 or newer (macOS: `brew install pandoc`; \
                 Linux: the release package from https://pandoc.org/installing.html, \
                 since older distro packages predate 2.15; \
                 Windows: `winget install JohnMacFarlane.Pandoc`) and restart codewhale.",
            )
        })?;
        require_sandbox_support(&pandoc).await?;

        let mut cmd = TokioCommand::new(&pandoc);
        cmd.args(pandoc_args(
            &source_path,
            &target_format,
            resolved_output_path.as_deref(),
        ));
        // Kill the converter if the timeout below drops the output()
        // future: pandoc on a pathological document would otherwise keep
        // running orphaned after the call already failed.
        cmd.kill_on_drop(true);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let output = tokio::time::timeout(PANDOC_TIMEOUT, cmd.output())
            .await
            .map_err(|_| ToolError::Timeout {
                seconds: PANDOC_TIMEOUT.as_secs(),
            })?
            .map_err(|e| ToolError::execution_failed(format!("failed to launch pandoc: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(ToolError::execution_failed(format!(
                "pandoc failed (exit {:?}): {stderr}",
                output.status.code()
            )));
        }

        let summary = if let Some(out) = resolved_output_path {
            format!(
                "Converted {} → {} via pandoc; wrote {}",
                source_path.display(),
                target_format,
                out.display()
            )
        } else {
            let text = String::from_utf8_lossy(&output.stdout).to_string();
            return Ok(ToolResult::success(text));
        };
        Ok(ToolResult::success(summary))
    }
}

/// First pandoc release that understands `--sandbox`.
const MIN_SANDBOX_VERSION: (u32, u32) = (2, 15);

/// Arguments for one conversion. `--sandbox` stops readers and writers from
/// touching any file but the named source and output: no include
/// directives, no embedded images or other resources fetched from disk or
/// the network. It is always present; a pandoc without it is refused by
/// [`require_sandbox_support`] rather than run without it.
fn pandoc_args(source: &Path, target_format: &str, output: Option<&Path>) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "--sandbox".into(),
        source.as_os_str().to_owned(),
        "--to".into(),
        target_format.into(),
    ];
    if let Some(out) = output {
        args.push("--output".into());
        args.push(out.as_os_str().to_owned());
    }
    args
}

/// Parse `major.minor` from the first line of `pandoc --version`
/// (`pandoc 3.1.9`, `pandoc.exe 2.9.2.1`).
fn parse_pandoc_version(banner: &str) -> Option<(u32, u32)> {
    let version = banner.lines().next()?.split_whitespace().nth(1)?;
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().map_or(Some(0), |m| m.parse().ok())?;
    Some((major, minor))
}

/// Refuse a pandoc older than 2.15 with an actionable message instead of
/// pandoc's own "Unknown option --sandbox". The version probe runs once per
/// process; an unparseable banner — or a probe that outlives its bound — is
/// let through, and pandoc itself then rejects the flag if it does not know
/// it. The child is async and kill-on-drop so a wedged `--version` can
/// neither block the executor nor outlive the probe.
async fn require_sandbox_support(pandoc: &str) -> Result<(), ToolError> {
    static VERSION: tokio::sync::OnceCell<Option<(u32, u32)>> = tokio::sync::OnceCell::const_new();
    let version = *VERSION
        .get_or_init(|| async {
            let mut cmd = TokioCommand::new(pandoc);
            cmd.arg("--version")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            let out = tokio::time::timeout(PANDOC_VERSION_PROBE_TIMEOUT, cmd.output())
                .await
                .ok()?
                .ok()?;
            parse_pandoc_version(&String::from_utf8_lossy(&out.stdout))
        })
        .await;
    match version {
        Some(found) if found < MIN_SANDBOX_VERSION => Err(ToolError::execution_failed(format!(
            "pandoc_convert: pandoc {}.{} or newer is required (found {}.{}). \
             Upgrade from https://pandoc.org/installing.html and restart codewhale.",
            MIN_SANDBOX_VERSION.0, MIN_SANDBOX_VERSION.1, found.0, found.1
        ))),
        _ => Ok(()),
    }
}

/// Whitelist of target formats whose output is binary (and therefore
/// can't be returned as inline text). `docx`, `odt`, and `epub` are
/// ZIP archives; everything else in [`SUPPORTED_TARGET_FORMATS`]
/// renders to UTF-8 text.
pub(crate) fn format_is_binary(target_format: &str) -> bool {
    matches!(target_format, "docx" | "odt" | "epub")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn pandoc_present() -> bool {
        codewhale_dependencies::resolve_pandoc().is_some()
    }

    fn pandoc_environment_unavailable(err: &ToolError) -> bool {
        let msg = err.to_string();
        msg.contains("getXdgDirectory") || msg.contains("sHGetFolderPath")
    }

    // Test-only skip diagnostic; the module-wide print_stderr deny targets prod code.
    #[allow(clippy::print_stderr)]
    async fn execute_pandoc_or_skip(input: Value, ctx: &ToolContext) -> Option<ToolResult> {
        match PandocConvertTool.execute(input, ctx).await {
            Ok(result) => Some(result),
            Err(err) if pandoc_environment_unavailable(&err) => {
                eprintln!("skipping pandoc integration assertion: {err}");
                None
            }
            Err(err) => panic!("execute: {err:?}"),
        }
    }

    #[test]
    fn supported_target_formats_match_schema_enum() {
        let tool = PandocConvertTool;
        let schema = tool.input_schema();
        let enum_vals = schema
            .get("properties")
            .and_then(|p| p.get("target_format"))
            .and_then(|t| t.get("enum"))
            .and_then(|e| e.as_array())
            .expect("target_format enum must be present in schema");
        let from_schema: Vec<&str> = enum_vals.iter().filter_map(|v| v.as_str()).collect();
        assert_eq!(
            from_schema, SUPPORTED_TARGET_FORMATS,
            "schema enum must mirror the SUPPORTED_TARGET_FORMATS constant exactly",
        );
    }

    #[test]
    fn binary_formats_require_output_path() {
        for fmt in ["docx", "odt", "epub"] {
            assert!(format_is_binary(fmt));
        }
        for fmt in [
            "markdown",
            "html",
            "rst",
            "latex",
            "plain",
            "gfm",
            "commonmark",
        ] {
            assert!(!format_is_binary(fmt));
        }
    }

    #[tokio::test]
    async fn pandoc_convert_refuses_deny_listed_sources() {
        // `.env` is on the default read deny-list; the refusal comes before
        // pandoc would run, so this holds with or without pandoc installed.
        let tmp = tempdir().expect("tempdir");
        fs::write(tmp.path().join(".env"), "TOKEN=SECRET_PANDOC_VALUE\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(tmp.path().join(".env"), tmp.path().join("notes.md")).unwrap();
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let mut sources = vec![".env"];
        if cfg!(unix) {
            sources.push("notes.md");
        }
        for source in sources {
            let err = PandocConvertTool
                .execute(
                    json!({"source_path": source, "target_format": "plain"}),
                    &ctx,
                )
                .await
                .expect_err("a deny-listed source must be refused");
            assert!(
                matches!(err, ToolError::PermissionDenied { .. }),
                "{source}: {err:?}"
            );
            assert!(!err.to_string().contains("SECRET_PANDOC_VALUE"));
        }
    }

    #[tokio::test]
    async fn pandoc_convert_does_not_follow_include_directives() {
        if !pandoc_present() {
            return;
        }
        let outside = tempdir().expect("outside");
        let secret = outside.path().join("outside.txt");
        fs::write(&secret, "SECRET_INCLUDED_VALUE\n").unwrap();
        let tmp = tempdir().expect("tempdir");
        fs::write(
            tmp.path().join("p.rst"),
            format!("Intro\n\n.. include:: {}\n", secret.display()),
        )
        .unwrap();
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let result = PandocConvertTool
            .execute(
                json!({"source_path": "p.rst", "target_format": "plain"}),
                &ctx,
            )
            .await;
        if let Err(err) = &result
            && pandoc_environment_unavailable(err)
        {
            return;
        }
        if let Ok(result) = result {
            assert!(
                !result.content.contains("SECRET_INCLUDED_VALUE"),
                "include directive must not pull in outside files: {}",
                result.content
            );
        }
    }

    #[test]
    fn pandoc_args_always_include_sandbox() {
        for output in [None, Some(Path::new("/w/out.docx"))] {
            let args = pandoc_args(Path::new("/w/in.md"), "docx", output);
            assert_eq!(
                args.first().map(OsString::as_os_str),
                Some("--sandbox".as_ref())
            );
            assert_eq!(args.iter().filter(|a| *a == "--sandbox").count(), 1);
        }
    }

    #[test]
    fn pandoc_version_gate_matches_sandbox_release() {
        assert_eq!(
            parse_pandoc_version("pandoc 2.9.2.1\nCompiled with"),
            Some((2, 9))
        );
        assert_eq!(parse_pandoc_version("pandoc.exe 3.1.9\n"), Some((3, 1)));
        assert_eq!(parse_pandoc_version("pandoc 3\n"), Some((3, 0)));
        assert_eq!(parse_pandoc_version("garbage"), None);
        assert!((2, 9) < MIN_SANDBOX_VERSION);
        assert!((2, 14) < MIN_SANDBOX_VERSION);
        assert!((2, 15) >= MIN_SANDBOX_VERSION);
        assert!((3, 0) >= MIN_SANDBOX_VERSION);
    }

    // Both invocations are bounded in time; a conversion that outlives its
    // budget is answered with ToolError::Timeout and its child killed, but
    // exercising that path would need a wedged `pandoc` injected past the
    // process-global resolve_pandoc() cache, so the bounds are pinned here
    // instead.
    #[test]
    fn pandoc_invocations_carry_time_bounds() {
        assert_eq!(PANDOC_TIMEOUT, Duration::from_secs(600));
        assert_eq!(PANDOC_VERSION_PROBE_TIMEOUT, Duration::from_secs(10));
        assert!(
            PANDOC_VERSION_PROBE_TIMEOUT < PANDOC_TIMEOUT,
            "a wedged version probe must fail fast, not ride the conversion budget"
        );
    }

    #[tokio::test]
    async fn pandoc_convert_rejects_unsupported_target_format() {
        let tmp = tempdir().expect("tempdir");
        let src = tmp.path().join("in.md");
        fs::write(&src, "# hi").unwrap();
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let err = PandocConvertTool
            .execute(
                json!({"source_path": "in.md", "target_format": "definitely-not-real"}),
                &ctx,
            )
            .await
            .expect_err("unsupported target format must reject before pandoc spawn");
        assert!(
            err.to_string().contains("unsupported target_format"),
            "error must call out the unsupported format; got {err}"
        );
    }

    #[tokio::test]
    async fn pandoc_convert_rejects_inline_request_for_binary_format() {
        let tmp = tempdir().expect("tempdir");
        let src = tmp.path().join("in.md");
        fs::write(&src, "# hi").unwrap();
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let err = PandocConvertTool
            .execute(
                json!({"source_path": "in.md", "target_format": "docx"}),
                &ctx,
            )
            .await
            .expect_err("missing output_path for docx must reject");
        assert!(
            err.to_string().contains("binary") && err.to_string().contains("output_path"),
            "error must explain why output_path is required; got {err}"
        );
    }

    #[tokio::test]
    async fn pandoc_convert_roundtrips_markdown_to_html_inline() {
        if !pandoc_present() {
            // Tool wouldn't be registered without pandoc; mirror the
            // catalog-build behaviour.
            return;
        }
        let tmp = tempdir().expect("tempdir");
        let src = tmp.path().join("note.md");
        fs::write(&src, "# Title\n\nA paragraph with `inline code`.\n").unwrap();
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let Some(result) = execute_pandoc_or_skip(
            json!({"source_path": "note.md", "target_format": "html"}),
            &ctx,
        )
        .await
        else {
            return;
        };
        assert!(result.success);
        assert!(
            result.content.contains("<h1") && result.content.contains("Title"),
            "html output must contain the heading; got {}",
            result.content
        );
        assert!(
            result.content.contains("<code") || result.content.contains("inline code"),
            "html output must preserve inline code; got {}",
            result.content
        );
    }

    #[tokio::test]
    async fn pandoc_convert_writes_output_path_and_reports_summary() {
        if !pandoc_present() {
            return;
        }
        let tmp = tempdir().expect("tempdir");
        let src = tmp.path().join("note.md");
        fs::write(&src, "# Title\n").unwrap();
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let Some(result) = execute_pandoc_or_skip(
            json!({
                "source_path": "note.md",
                "target_format": "html",
                "output_path": "out.html",
            }),
            &ctx,
        )
        .await
        else {
            return;
        };
        assert!(result.success);
        assert!(result.content.contains("wrote"));
        let written = fs::read_to_string(tmp.path().join("out.html")).expect("read");
        assert!(
            written.contains("Title"),
            "written file must contain converted body; got {written}"
        );
    }

    #[tokio::test]
    async fn pandoc_convert_surfaces_missing_source_path_clearly() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let err = PandocConvertTool
            .execute(
                json!({"source_path": "missing.md", "target_format": "html"}),
                &ctx,
            )
            .await
            .expect_err("nonexistent source must reject");
        assert!(
            err.to_string().contains("source_path") && err.to_string().contains("does not exist"),
            "error must call out missing source; got {err}"
        );
    }
}
