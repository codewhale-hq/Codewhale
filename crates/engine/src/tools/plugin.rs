//! Plugin tool system — scripts and commands as first-class tools.
//!
//! Users can drop self-describing scripts in `~/.codewhale/tools/` and they
//! are auto-discovered, parsed for frontmatter, and registered as model-visible
//! tools alongside built-in implementations.
//!
//! # Script frontmatter format
//!
//! Every plugin script must have a frontmatter header in its first 20 lines:
//!
//! ```sh
//! # name: my-tool
//! # description: Does something useful
//! # schema: {"type":"object","properties":{"input":{"type":"string"}}}
//! # approval: required
//! ```
//!
//! The script receives the tool's JSON input on **stdin** and must return
//! a JSON `ToolResult` (`{"content": "...", "success": true}`) on **stdout**.
//! Non-JSON output is wrapped in a `ToolResult` with `success: false`.
//!
//! # What a script tool cannot do (D4, CURRENT_DECISIONS §26)
//!
//! - **Approve itself.** `# approval:` accepts `suggest` (the default) and
//!   `required`. `auto` is no longer honoured: the tool gets the default a
//!   script without the line gets, and [`PluginMetadata::auto_approval_ignored`]
//!   lets each loader say so (runtime log, `/plugin tools`) instead of
//!   downgrading silently.
//! - **Replace a built-in.** A drop-in script whose name is already registered
//!   is refused by `ToolRegistry::load_plugins`, and a `[tools.overrides]`
//!   `script` / `command` entry keyed by a built-in is refused by
//!   `ToolRegistry::apply_overrides_with_executor`; `disabled` still turns a built-in off.
//!
//! Known limitation: an unrecognised `# approval:` value (a typo such as
//! `requried`) still falls back to the default without a diagnostic.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;

use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};

use codewhale_runtime::config::ToolOverride;

/// Timeout for plugin script execution (120 seconds).
const PLUGIN_EXECUTION_TIMEOUT: Duration = Duration::from_secs(120);

/// Captured at catalogue preparation. Enabled host errors never select Legacy.
#[derive(Clone)]
pub enum PluginExecutor {
    Legacy,
    Host(Arc<crate::extension_host::ExtensionHostManager>),
}
impl PluginExecutor {
    pub fn for_engine() -> Self {
        if codewhale_plugins::activation::extension_host_policy_enabled() {
            let manager = crate::extension_host::manager();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                manager.bind_engine_handle(handle);
            }
            Self::Host(manager)
        } else {
            Self::Legacy
        }
    }
    async fn run(
        &self,
        mut command: tokio::process::Command,
        label: &str,
        input: Value,
        context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        match self {
            Self::Legacy if !codewhale_plugins::activation::extension_host_policy_enabled() => {
                run_plugin_child_raw(&mut command, label, input).await
            }
            Self::Legacy => Err(ToolError::not_available(
                "script catalogue predates enabled execution; prepare tools again",
            )),
            Self::Host(manager) => manager.execute_script(command, input, context).await,
        }
    }
}

/// Metadata extracted from a plugin script's frontmatter header.
#[derive(Debug, Clone)]
pub struct PluginMetadata {
    /// Tool name (from `# name:`).
    pub name: String,
    /// Human-readable description (from `# description:`).
    pub description: String,
    /// JSON Schema for the tool's input (from `# schema:`).
    /// Defaults to a permissive `{"type": "object"}` when absent.
    pub input_schema: Value,
    /// Approval requirement (from `# approval:`).
    /// Defaults to `Suggest`; never `Auto` (see the module docs).
    pub approval: ApprovalRequirement,
    /// The frontmatter asked for `approval: auto`, which script tools may no
    /// longer use; `approval` holds the default instead. Loaders report it
    /// with `AUTO_APPROVAL_UNSUPPORTED`.
    pub auto_approval_ignored: bool,
}

/// Why a script's `approval: auto` was ignored. Shared by the load warning and
/// the `/plugin tools` diagnostic so the two surfaces say the same thing.
pub const AUTO_APPROVAL_UNSUPPORTED: &str = "`approval: auto` is no longer supported for script tools; \
     the tool follows the session's approval setting like a script with no `approval:` line";

/// Log the D4 downgrade for a script tool registered as `tool_name`.
fn warn_auto_approval_ignored(tool_name: &str) {
    tracing::warn!(
        "Script tool '{}': {AUTO_APPROVAL_UNSUPPORTED}",
        codewhale_runtime::safe_label::SafeLabel::identifier(tool_name)
    );
}

/// A tool backed by an external script or executable dropped into the
/// plugins directory. The script receives JSON input on stdin and writes
/// a JSON `ToolResult` to stdout.
struct ScriptPluginTool {
    executor: PluginExecutor,
    metadata: PluginMetadata,
    /// Absolute path to the script.
    script_path: PathBuf,
    /// Optional static arguments passed before the JSON input.
    args: Vec<String>,
}

impl std::fmt::Debug for ScriptPluginTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptPluginTool")
            .field("name", &self.metadata.name)
            .field("script_path", &self.script_path)
            .finish()
    }
}

#[async_trait]
impl ToolSpec for ScriptPluginTool {
    fn name(&self) -> &str {
        &self.metadata.name
    }

    fn registration_origin(&self) -> std::borrow::Cow<'_, str> {
        use codewhale_runtime::safe_label::SafeLabel;
        let filename = self
            .script_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        format!(
            "plugin script {} ({})",
            SafeLabel::identifier(&filename),
            SafeLabel::identifier(&self.script_path.to_string_lossy())
        )
        .into()
    }

    fn description(&self) -> &str {
        &self.metadata.description
    }

    fn input_schema(&self) -> Value {
        self.metadata.input_schema.clone()
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        // Unknown plugin — conservative: mark as requiring execution + approval.
        vec![
            ToolCapability::ExecutesCode,
            ToolCapability::RequiresApproval,
        ]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        self.metadata.approval
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let (interpreter, script_args) = script_command_parts(&self.script_path, &self.args);
        let mut command = tokio::process::Command::new(&interpreter);
        codewhale_util::utils::suppress_tokio_console_window(&mut command);
        command.args(script_args);
        self.executor
            .run(
                command,
                &self.script_path.display().to_string(),
                input,
                context,
            )
            .await
    }
}

/// A tool backed by an arbitrary shell command from config.toml overrides.
/// Behaves like `ScriptPluginTool` but uses the user-specified command string.
struct CommandPluginTool {
    executor: PluginExecutor,
    name: String,
    description: String,
    input_schema: Value,
    command: String,
    args: Vec<String>,
    approval: ApprovalRequirement,
}

impl std::fmt::Debug for CommandPluginTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandPluginTool")
            .field("name", &self.name)
            .field("command", &self.command)
            .finish()
    }
}

#[async_trait]
impl ToolSpec for CommandPluginTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn registration_origin(&self) -> std::borrow::Cow<'_, str> {
        format!(
            "config [tools.overrides.{}]",
            codewhale_runtime::safe_label::SafeLabel::identifier(&self.name)
        )
        .into()
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> Value {
        self.input_schema.clone()
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![
            ToolCapability::ExecutesCode,
            ToolCapability::RequiresApproval,
        ]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        self.approval
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        // On Windows, if the command doesn't have an extension, try wrapping
        // in `cmd /c` or use `powershell` for `.ps1` files. For portability
        // we let tokio::process::Command resolve via PATH.
        let mut cmd = if cfg!(windows) && !self.command.contains('.') {
            let mut c = tokio::process::Command::new("cmd");
            codewhale_util::utils::suppress_tokio_console_window(&mut c);
            c.arg("/c").arg(&self.command);
            c
        } else {
            let mut c = tokio::process::Command::new(&self.command);
            codewhale_util::utils::suppress_tokio_console_window(&mut c);
            c
        };
        cmd.args(&self.args);
        let label = format!("command '{}'", self.command);
        self.executor.run(cmd, &label, input, context).await
    }
}

// ---------------------------------------------------------------------------
// Script interpreter resolution
// ---------------------------------------------------------------------------

/// Parse a shebang line (`#!/usr/bin/env node`) to extract the interpreter.
fn parse_shebang(path: &Path) -> Option<(String, Vec<String>)> {
    let mut file = std::fs::File::open(path).ok()?;
    let content = read_prefix_to_string(&mut file, 256)?;
    let first_line = content.lines().next()?;
    let rest = first_line.strip_prefix("#!")?;
    let parts: Vec<&str> = rest.split_whitespace().collect();
    if parts.is_empty() {
        return None;
    }
    let interpreter = parts[0].to_string();
    let args: Vec<String> = parts[1..].iter().map(|s| s.to_string()).collect();
    Some((interpreter, args))
}

/// Resolve the interpreter binary and pre-args for a script file.
///
/// Priority:
/// 1. Shebang line from the script itself (`#!/usr/bin/env node`)
/// 2. Extension-based fallback for known script types
/// 3. Direct execution (assumes the OS knows how to run it)
fn resolve_interpreter(path: &Path) -> (String, Vec<String>) {
    // 1. Try shebang
    if let Some((interp, shebang_args)) = parse_shebang(path) {
        let bin_name = interp.rsplit('/').next().unwrap_or(&interp);
        // `env` is a special case: `#!/usr/bin/env node` → `node`
        // On Windows, `env` is not available, so extract the intended binary.
        if bin_name == "env" && !shebang_args.is_empty() {
            return (shebang_args[0].clone(), shebang_args[1..].to_vec());
        }
        if cfg!(windows) {
            return (bin_name.to_string(), shebang_args);
        }
        return (interp, shebang_args);
    }

    // 2. Extension-based fallback for common script types
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "ps1" => ("powershell".into(), vec!["-File".into()]),
        "py" => ("python".into(), vec![]),
        "js" | "mjs" => ("node".into(), vec![]),
        "ts" => ("npx".into(), vec!["tsx".into()]),
        "rb" => ("ruby".into(), vec![]),
        "sh" | "bash" | "zsh" => {
            // On Windows, route shell scripts through sh if available
            if cfg!(windows) {
                ("sh".into(), vec![])
            } else {
                (path.to_string_lossy().into(), vec![])
            }
        }
        _ => (path.to_string_lossy().into(), vec![]),
    }
}

fn script_command_parts(script_path: &Path, args: &[String]) -> (String, Vec<String>) {
    let (interpreter, mut script_args) = resolve_interpreter(script_path);
    let script_path_arg = script_path.to_string_lossy().to_string();
    if interpreter != script_path_arg {
        script_args.push(script_path_arg);
    }
    script_args.extend(args.iter().cloned());
    (interpreter, script_args)
}

fn read_prefix_to_string(reader: impl std::io::Read, max_bytes: u64) -> Option<String> {
    use std::io::Read;

    let mut buf = Vec::new();
    reader.take(max_bytes).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

// ---------------------------------------------------------------------------
// Shared child process helpers
// ---------------------------------------------------------------------------

/// Run a pre-configured tokio Command, pipe JSON input, collect ToolResult.
async fn run_plugin_child_raw(
    cmd: &mut tokio::process::Command,
    label: &str,
    input: Value,
) -> Result<ToolResult, ToolError> {
    let input_bytes = serde_json::to_vec(&input)
        .map_err(|e| ToolError::invalid_input(format!("failed to serialize input: {e}")))?;

    // Contained: a timed-out or cancelled plugin takes everything it started
    // down with it, not just the interpreter that `kill_on_drop` would reach.
    let output = tokio::time::timeout(
        PLUGIN_EXECUTION_TIMEOUT,
        codewhale_runtime::process_tree::contained_output_with_input(cmd, input_bytes),
    )
    .await
    .map_err(|_| ToolError::Timeout {
        seconds: PLUGIN_EXECUTION_TIMEOUT.as_secs(),
    })?
    .map_err(|e| ToolError::execution_failed(format!("failed to run {label}: {e}")))?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        if let Ok(parsed) = serde_json::from_str::<ToolResult>(&stdout) {
            Ok(parsed)
        } else {
            Ok(ToolResult::success(stdout))
        }
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let combined = if stderr.is_empty() {
            stdout
        } else if stdout.is_empty() {
            stderr
        } else {
            format!("{stdout}\n{stderr}")
        };
        Err(ToolError::execution_failed(combined))
    }
}

// ---------------------------------------------------------------------------
// Frontmatter parsing
// ---------------------------------------------------------------------------

/// Parse frontmatter header from the first `max_lines` lines of a text file.
///
/// Expected format (one `# key: value` per line):
/// ```text
/// # name: my-tool
/// # description: Does something
/// # schema: {"type":"object"}
/// # approval: required
/// ```
///
/// Also supports `// ` prefix for JavaScript/TypeScript scripts and `-- ` for Lua.
pub fn parse_frontmatter(content: &str) -> PluginMetadata {
    let mut name = String::new();
    let mut description = String::new();
    let mut schema_str = String::new();
    let mut approval_str = String::new();

    for line in content.lines().take(20) {
        let line = line.trim();
        // Strip leading comment markers: `#`, `//`, `--`.
        let rest = line
            .strip_prefix('#')
            .or_else(|| line.strip_prefix("//"))
            .or_else(|| line.strip_prefix("--"));
        let Some(rest) = rest else { continue };
        if let Some((key, value)) = rest.trim_start().split_once(':') {
            let key = key.trim().to_lowercase();
            let value = value.trim();
            match key.as_str() {
                "name" => name = value.to_string(),
                "description" => description = value.to_string(),
                "schema" => schema_str = value.to_string(),
                "approval" => approval_str = value.to_string(),
                _ => {}
            }
        }
    }

    let input_schema = if schema_str.is_empty() {
        // Default: accept any object payload
        serde_json::json!({"type": "object"})
    } else {
        serde_json::from_str(&schema_str).unwrap_or_else(|_| serde_json::json!({"type": "object"}))
    };

    // A script cannot approve itself (D4): `auto` gets the default, flagged so
    // the loaders can report it.
    let (approval, auto_approval_ignored) = match approval_str.to_lowercase().as_str() {
        "required" => (ApprovalRequirement::Required, false),
        "auto" => (ApprovalRequirement::Suggest, true),
        _ => (ApprovalRequirement::Suggest, false),
    };

    PluginMetadata {
        name: if name.is_empty() {
            "unnamed-plugin".to_string()
        } else {
            name
        },
        description: if description.is_empty() {
            "User-provided plugin tool".to_string()
        } else {
            description
        },
        input_schema,
        approval,
        auto_approval_ignored,
    }
}

/// Read the first 4 KB of a file and parse its frontmatter.
fn read_script_metadata(path: &Path) -> Option<PluginMetadata> {
    let mut file = std::fs::File::open(path).ok()?;
    let content = read_prefix_to_string(&mut file, 4096)?;
    let meta = parse_frontmatter(&content);
    // Require at least the `name` field to consider it a valid plugin.
    if meta.name == "unnamed-plugin" {
        return None;
    }
    Some(meta)
}

// ---------------------------------------------------------------------------
// Directory scanning
// ---------------------------------------------------------------------------

/// Scan a directory for plugin script files with frontmatter headers.
///
/// Files are considered eligible when:
/// - They are regular files (not directories, not symlinks)
/// - They don't start with `.` (hidden files)
/// - They are not `README.md`
/// - Their first 20 lines contain `# name:` frontmatter
pub fn scan_plugin_dir(dir: &Path) -> Vec<(PathBuf, PluginMetadata)> {
    let mut results = Vec::new();

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!("Failed to read plugin directory {}: {e}", dir.display());
            return results;
        }
    };

    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let path = entry.path();

        // Skip directories and hidden files
        if path.is_dir() {
            continue;
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && (name.starts_with('.') || name == "README.md")
        {
            continue;
        }

        // Try to parse frontmatter
        if let Some(meta) = read_script_metadata(&path) {
            results.push((path, meta));
        }
    }

    results
}

/// Load all plugin tools from a directory. Each eligible script becomes
/// a registered `ScriptPluginTool`.
#[cfg(any(test, feature = "test-support"))]
pub fn load_plugin_tools(plugin_dir: &Path) -> Vec<Arc<dyn ToolSpec>> {
    load_plugin_tools_with_executor(plugin_dir, PluginExecutor::for_engine())
}
pub fn load_plugin_tools_with_executor(
    plugin_dir: &Path,
    executor: PluginExecutor,
) -> Vec<Arc<dyn ToolSpec>> {
    let discovered = scan_plugin_dir(plugin_dir);
    let mut tools: Vec<Arc<dyn ToolSpec>> = Vec::with_capacity(discovered.len());

    for (path, meta) in discovered {
        tracing::info!(
            "Discovered plugin tool '{}' at {}",
            meta.name,
            path.display()
        );
        if meta.auto_approval_ignored {
            warn_auto_approval_ignored(&meta.name);
        }
        tools.push(Arc::new(ScriptPluginTool {
            executor: executor.clone(),
            metadata: meta,
            script_path: path,
            args: Vec::new(),
        }));
    }

    tools
}

/// Create a single tool from a `ToolOverride` config entry.
///
/// Returns `None` for `Disabled` (the caller handles removal separately).
/// This builds the tool only; `ToolRegistry::apply_overrides_with_executor` decides whether
/// the name may be taken, and refuses one owned by a built-in.
#[cfg(any(test, feature = "test-support"))]
pub fn tool_from_override(
    tool_name: &str,
    override_cfg: &ToolOverride,
    plugin_dir: &Path,
) -> Option<Arc<dyn ToolSpec>> {
    tool_from_override_with_executor(
        tool_name,
        override_cfg,
        plugin_dir,
        PluginExecutor::for_engine(),
    )
}
pub fn tool_from_override_with_executor(
    tool_name: &str,
    override_cfg: &ToolOverride,
    plugin_dir: &Path,
    executor: PluginExecutor,
) -> Option<Arc<dyn ToolSpec>> {
    match override_cfg {
        ToolOverride::Disabled => None,
        ToolOverride::Script { path, args } => {
            let script_path = if Path::new(path).is_absolute() {
                PathBuf::from(path)
            } else {
                // Relative paths resolve relative to the plugin directory.
                plugin_dir.join(path)
            };

            if !script_path.exists() {
                tracing::warn!(
                    "Override script for '{}' not found at {}",
                    tool_name,
                    script_path.display()
                );
                return None;
            }

            // Read the script's own frontmatter for metadata, or provide
            // defaults if it has none.
            let mut meta = read_script_metadata(&script_path).unwrap_or_else(|| PluginMetadata {
                name: tool_name.to_string(),
                description: format!("Script tool '{tool_name}' from [tools.overrides]"),
                input_schema: serde_json::json!({"type": "object"}),
                approval: ApprovalRequirement::Suggest,
                auto_approval_ignored: false,
            });

            // The config key owns the replacement target; frontmatter supplies metadata only.
            meta.name = tool_name.to_string();
            if meta.auto_approval_ignored {
                warn_auto_approval_ignored(tool_name);
            }

            Some(Arc::new(ScriptPluginTool {
                executor,
                metadata: meta,
                script_path,
                args: args.clone().unwrap_or_default(),
            }) as Arc<dyn ToolSpec>)
        }
        ToolOverride::Command { command, args } => {
            // Build a description that includes the command.
            let description = format!("Override for '{tool_name}' — runs: {command}");
            let cmd_args = args.clone().unwrap_or_default();

            Some(Arc::new(CommandPluginTool {
                executor,
                name: tool_name.to_string(),
                description,
                input_schema: serde_json::json!({"type": "object"}),
                command: command.clone(),
                args: cmd_args,
                approval: ApprovalRequirement::Suggest,
            }) as Arc<dyn ToolSpec>)
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const DEADLOCK_CHILD_ENV: &str = "CODEWHALE_PLUGIN_DEADLOCK_CHILD";

    #[test]
    fn test_parse_frontmatter_full() {
        let content = "\
#!/usr/bin/env sh
# name: my-tool
# description: A useful custom tool
# schema: {\"type\":\"object\",\"properties\":{\"input\":{\"type\":\"string\"}}}
# approval: required
echo hello
";
        let meta = parse_frontmatter(content);
        assert_eq!(meta.name, "my-tool");
        assert_eq!(meta.description, "A useful custom tool");
        assert_eq!(meta.approval, ApprovalRequirement::Required);
        assert_eq!(
            meta.input_schema,
            serde_json::json!({"type":"object","properties":{"input":{"type":"string"}}})
        );
    }

    #[test]
    fn test_parse_frontmatter_accepts_compact_and_spaced_markers() {
        let content = "\
#!/usr/bin/env node
#name:compact-name
//  description:  spaced description
-- schema : {\"type\":\"object\",\"properties\":{\"ok\":{\"type\":\"boolean\"}}}
# approval: auto
";

        let meta = parse_frontmatter(content);

        assert_eq!(meta.name, "compact-name");
        assert_eq!(meta.description, "spaced description");
        assert_eq!(meta.approval, ApprovalRequirement::Suggest);
        assert!(meta.auto_approval_ignored);
        assert_eq!(
            meta.input_schema,
            serde_json::json!({"type":"object","properties":{"ok":{"type":"boolean"}}})
        );
    }

    #[test]
    fn test_parse_frontmatter_minimal() {
        let content = "# name: mini";
        let meta = parse_frontmatter(content);
        assert_eq!(meta.name, "mini");
        assert_eq!(meta.description, "User-provided plugin tool");
        assert_eq!(meta.approval, ApprovalRequirement::Suggest);
    }

    #[test]
    fn test_parse_frontmatter_missing_name() {
        let content = "# description: no name here";
        let meta = parse_frontmatter(content);
        assert_eq!(meta.name, "unnamed-plugin");
        // read_script_metadata would return None for this.
    }

    #[test]
    fn test_read_prefix_collects_multiple_short_reads() {
        struct OneByteReader {
            bytes: Vec<u8>,
            pos: usize,
        }

        impl std::io::Read for OneByteReader {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.pos >= self.bytes.len() {
                    return Ok(0);
                }
                buf[0] = self.bytes[self.pos];
                self.pos += 1;
                Ok(1)
            }
        }

        let reader = OneByteReader {
            bytes: b"# name: short-read\n# description: ok\n".to_vec(),
            pos: 0,
        };

        assert_eq!(
            read_prefix_to_string(reader, 4096).as_deref(),
            Some("# name: short-read\n# description: ok\n")
        );
    }

    #[test]
    fn test_resolve_interpreter_handles_absolute_shebang_by_platform() {
        let dir = TempDir::new().unwrap();
        let script = dir.path().join("tool");
        std::fs::write(
            &script,
            "#!/opt/custom/bin/tool-runner --safe\n# name: tool\n",
        )
        .unwrap();

        let (interpreter, args) = resolve_interpreter(&script);

        if cfg!(windows) {
            assert_eq!(interpreter, "tool-runner");
        } else {
            assert_eq!(interpreter, "/opt/custom/bin/tool-runner");
        }
        assert_eq!(args, vec!["--safe"]);
    }

    #[test]
    fn test_script_command_parts_does_not_pass_direct_script_as_own_arg() {
        let dir = TempDir::new().unwrap();
        let script = dir.path().join("direct-tool");
        std::fs::write(&script, "# name: direct\n").unwrap();

        let (interpreter, args) =
            script_command_parts(&script, &["--flag".to_string(), "value".to_string()]);

        assert_eq!(interpreter, script.to_string_lossy());
        assert_eq!(args, vec!["--flag", "value"]);
    }

    #[test]
    fn test_script_command_parts_passes_script_to_external_interpreter() {
        let dir = TempDir::new().unwrap();
        let script = dir.path().join("script.py");
        std::fs::write(&script, "# name: py\n").unwrap();

        let (interpreter, args) = script_command_parts(&script, &["--flag".to_string()]);

        assert_eq!(interpreter, "python");
        assert_eq!(
            args,
            vec![script.to_string_lossy().to_string(), "--flag".to_string()]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_run_plugin_child_drains_stdout_while_writing_large_stdin() {
        let mut cmd = tokio::process::Command::new(std::env::current_exe().unwrap());
        cmd.arg("plugin_deadlock_child_process")
            .arg("--nocapture")
            .env(DEADLOCK_CHILD_ENV, "1");

        let input = serde_json::json!({ "payload": "y".repeat(1024 * 1024) });
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            run_plugin_child_raw(&mut cmd, "deadlock child", input),
        )
        .await
        .expect("plugin execution should not deadlock")
        .expect("plugin child should succeed");

        assert!(result.success);
        assert!(result.content.len() > 64 * 1024);
    }

    /// A cancelled (or timed-out) plugin call kills what the plugin started,
    /// not just the interpreter.
    #[cfg(unix)]
    #[tokio::test]
    async fn dropped_plugin_call_kills_the_plugin_process_tree() {
        let tmp = TempDir::new().unwrap();
        let pid_file = tmp.path().join("grandchild.pid");
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.arg("-c")
            .arg("sleep 300 & echo $! > grandchild.pid; wait")
            .current_dir(tmp.path());
        let run = run_plugin_child_raw(&mut cmd, "hanging plugin", serde_json::json!({}));
        let grandchild =
            codewhale_runtime::process_tree::drop_once_pid_written(run, &pid_file).await;
        assert!(
            codewhale_runtime::process_tree::wait_for_pid_exit(grandchild, Duration::from_secs(5)),
            "a process started by the cancelled plugin is still running"
        );
    }

    #[test]
    fn plugin_deadlock_child_process() {
        if std::env::var_os(DEADLOCK_CHILD_ENV).is_none() {
            return;
        }

        use std::io::{Read, Write};

        let mut stdout = std::io::stdout();
        stdout.write_all(&vec![b'x'; 1024 * 1024]).unwrap();
        stdout.flush().unwrap();

        let mut stdin = Vec::new();
        std::io::stdin().read_to_end(&mut stdin).unwrap();
        writeln!(
            stdout,
            "{{\"content\":\"read {} bytes\",\"success\":true}}",
            stdin.len()
        )
        .unwrap();
        std::process::exit(0);
    }

    #[test]
    fn test_scan_plugin_dir_finds_scripts() {
        let dir = TempDir::new().unwrap();

        // Valid plugin
        std::fs::write(
            dir.path().join("my-plugin.sh"),
            "# name: my-plugin\n# description: test\n",
        )
        .unwrap();

        // Hidden file — should be skipped
        std::fs::write(
            dir.path().join(".hidden.sh"),
            "# name: hidden\n# description: should skip\n",
        )
        .unwrap();

        // README — should be skipped
        std::fs::write(dir.path().join("README.md"), "# Tools\n").unwrap();

        // No frontmatter — should be skipped
        std::fs::write(dir.path().join("random.sh"), "echo hi\n").unwrap();

        let discovered = scan_plugin_dir(dir.path());
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].1.name, "my-plugin");
    }

    #[test]
    fn test_scan_plugin_dir_returns_files_sorted_by_name() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("z-plugin.sh"),
            "# name: z-plugin\n# description: z\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("a-plugin.sh"),
            "# name: a-plugin\n# description: a\n",
        )
        .unwrap();

        let discovered = scan_plugin_dir(dir.path());

        let names: Vec<_> = discovered
            .iter()
            .map(|(_, meta)| meta.name.as_str())
            .collect();
        assert_eq!(names, vec!["a-plugin", "z-plugin"]);
    }

    #[test]
    fn test_load_plugin_tools_creates_tools() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("greet.sh"),
            "# name: greet\n# description: Say hello\n# schema: {\"type\":\"object\",\"properties\":{\"name\":{\"type\":\"string\"}},\"required\":[\"name\"]}\n",
        )
        .unwrap();

        let tools = load_plugin_tools(dir.path());
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name(), "greet");
        assert_eq!(tools[0].description(), "Say hello");
    }

    #[test]
    fn runtime_surface_hardening_override_uses_configured_name() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("wrapper.sh"),
            "# name: custom-shell\n# description: Audit wrapper for exec_shell\n",
        )
        .unwrap();

        let override_cfg = ToolOverride::Script {
            path: "wrapper.sh".to_string(),
            args: None,
        };

        let tool = tool_from_override("exec_shell", &override_cfg, dir.path());
        assert!(tool.is_some());
        assert_eq!(tool.unwrap().name(), "exec_shell");
    }

    #[test]
    fn test_tool_from_override_disabled() {
        let dir = TempDir::new().unwrap();
        let override_cfg = ToolOverride::Disabled;
        let tool = tool_from_override("code_execution", &override_cfg, dir.path());
        assert!(tool.is_none());
    }

    #[test]
    fn test_tool_from_override_command() {
        let dir = TempDir::new().unwrap();
        let override_cfg = ToolOverride::Command {
            command: "my-custom-reader".to_string(),
            args: Some(vec!["--format".to_string(), "json".to_string()]),
        };
        let tool = tool_from_override("read_file", &override_cfg, dir.path());
        assert!(tool.is_some());
        assert_eq!(tool.unwrap().name(), "read_file");
    }

    #[test]
    fn test_tool_from_override_script_absolute_path() {
        let dir = TempDir::new().unwrap();
        let script_path = dir.path().join("audit.sh");
        std::fs::write(&script_path, "# name: exec_shell\n# description: Audit\n").unwrap();

        let override_cfg = ToolOverride::Script {
            path: script_path.to_str().unwrap().to_string(),
            args: None,
        };

        let tool = tool_from_override("exec_shell", &override_cfg, dir.path());
        assert!(tool.is_some());
    }

    #[test]
    fn test_approval_variants() {
        let check = |content: &str, expected: ApprovalRequirement| {
            assert_eq!(parse_frontmatter(content).approval, expected);
        };

        // D4: a script cannot approve itself; `auto` gets the default.
        check("# name: x\n# approval: auto", ApprovalRequirement::Suggest);
        check("# name: x\n# approval: AUTO", ApprovalRequirement::Suggest);
        check(
            "# name: x\n# approval: required",
            ApprovalRequirement::Required,
        );
        check(
            "# name: x\n# approval: suggest",
            ApprovalRequirement::Suggest,
        );
        check(
            "# name: x\n# approval: unknown",
            ApprovalRequirement::Suggest,
        );
        check("# name: x", ApprovalRequirement::Suggest);
    }
}
