//! Clipboard handling for paste support in TUI
//!
//! Supports text and image paste operations. Images on the clipboard are
//! encoded as PNG and persisted under `~/.codewhale/clipboard-images/` so the
//! model can reach them via the existing `@`-mention / file tools (DeepSeek
//! V4 does not currently accept inline image input on its Chat Completions
//! endpoint, so we materialize the bytes to disk instead of base64-embedding
//! them in the request).
//!
//! OpenHarmony deliberately excludes native desktop/Wayland clipboard APIs.
//! Copy falls back to OSC 52 (or tmux `load-buffer -w`), paste arrives through
//! terminal input, and image clipboard reads are unavailable.

#[cfg(all(target_os = "linux", not(target_env = "ohos"), not(test)))]
mod primary;

use std::ffi::OsStr;
#[cfg(any(not(test), all(test, unix)))]
use std::io::Write;
#[cfg(not(test))]
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
#[cfg(any(not(test), all(test, unix)))]
use std::process::{Command, Stdio};
#[cfg(any(
    target_os = "macos",
    target_os = "windows",
    all(target_os = "linux", not(target_env = "ohos"))
))]
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
#[cfg(any(
    target_os = "macos",
    target_os = "windows",
    all(target_os = "linux", not(target_env = "ohos"))
))]
use arboard::{Clipboard, ImageData};
use base64::Engine as _;
#[cfg(any(
    target_os = "macos",
    target_os = "windows",
    all(target_os = "linux", not(target_env = "ohos"))
))]
use image::{ImageBuffer, Rgba};

const OSC52_MAX_BYTES: usize = 100 * 1024;
const PRIMARY_MAX_BYTES: usize = 1024 * 1024;
#[cfg(any(
    test,
    target_os = "macos",
    target_os = "windows",
    all(target_os = "linux", not(target_env = "ohos"))
))]
const MAX_CLIPBOARD_HTML_BYTES: usize = 1024 * 1024;
#[cfg(any(all(test, unix), all(target_os = "linux", not(target_env = "ohos"))))]
const WAYLAND_CLIPBOARD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
#[cfg(any(all(test, unix), all(target_os = "linux", not(target_env = "ohos"))))]
const WL_PASTE_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// Convert rich clipboard content without loading its linked resources. Keep
/// the plain representation as a lossless fallback for empty/oversized HTML.
#[cfg(any(
    test,
    target_os = "macos",
    target_os = "windows",
    all(target_os = "linux", not(target_env = "ohos"))
))]
fn clipboard_markdown(html: &str) -> Option<String> {
    if html.len() > MAX_CLIPBOARD_HTML_BYTES {
        return None;
    }
    let mut markdown = htmd::HtmlToMarkdown::builder()
        .options(htmd::options::Options {
            preformatted_code: true,
            ..Default::default()
        })
        .skip_tags(vec!["script", "style", "head", "iframe", "object"])
        .build()
        .convert(html)
        .ok()?;
    // A standalone H1 must not become the `# note` memory shortcut. Setext
    // is equivalent Markdown and remains multi-line after composer trimming.
    if let Some(heading) = markdown.trim().strip_prefix("# ")
        && !heading.contains('\n')
    {
        markdown = format!("{heading}\n===");
    }
    (!markdown.trim().is_empty() && markdown.len() <= MAX_CLIPBOARD_HTML_BYTES).then_some(markdown)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClipboardEndpoint {
    /// The TUI and desktop clipboard live on the same host.
    NativeHost,
    /// SSH exported a graphical display (X11 or Wayland), so the native
    /// clipboard intentionally addresses that forwarded display.
    ForwardedDisplay,
    /// No graphical endpoint is available over SSH. Clipboard transfer must
    /// be requested from the terminal client instead.
    TerminalClient,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClipboardWriteOrder {
    /// An SSH TUI without an exported graphical display must target the
    /// terminal client. A native clipboard on the remote host can succeed
    /// while writing to the wrong machine.
    TerminalClientOnly,
    /// A local TUI should prefer the native clipboard (including images) and
    /// retain OSC 52 as the terminal fallback.
    NativeHostThenTerminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TerminalClipboardContext {
    endpoint: ClipboardEndpoint,
    in_tmux: bool,
}

impl TerminalClipboardContext {
    fn detect() -> Self {
        let ssh_client = std::env::var_os("SSH_CLIENT");
        let ssh_connection = std::env::var_os("SSH_CONNECTION");
        let ssh_tty = std::env::var_os("SSH_TTY");
        let display = std::env::var_os("DISPLAY");
        let wayland_display = std::env::var_os("WAYLAND_DISPLAY");
        let ssh_clipboard = std::env::var_os("CODEWHALE_SSH_CLIPBOARD");
        let tmux = std::env::var_os("TMUX");
        Self::from_env_values(
            ssh_client.as_deref(),
            ssh_connection.as_deref(),
            ssh_tty.as_deref(),
            display.as_deref(),
            wayland_display.as_deref(),
            ssh_clipboard.as_deref(),
            tmux.as_deref(),
        )
    }

    fn from_env_values(
        ssh_client: Option<&OsStr>,
        ssh_connection: Option<&OsStr>,
        ssh_tty: Option<&OsStr>,
        display: Option<&OsStr>,
        wayland_display: Option<&OsStr>,
        ssh_clipboard: Option<&OsStr>,
        tmux: Option<&OsStr>,
    ) -> Self {
        let in_ssh_session = [ssh_client, ssh_connection, ssh_tty]
            .into_iter()
            .flatten()
            .any(|value| !value.is_empty());
        let has_graphical_display = [display, wayland_display]
            .into_iter()
            .flatten()
            .any(|value| !value.is_empty());
        let forwarded_x11 = display.and_then(OsStr::to_str).is_some_and(|value| {
            ["localhost:", "127.0.0.1:", "[::1]:", "::1:"]
                .iter()
                .any(|prefix| value.starts_with(prefix))
        });
        let use_graphical_display = match ssh_clipboard.and_then(OsStr::to_str) {
            Some("graphical") => has_graphical_display,
            Some("terminal") => false,
            _ => forwarded_x11,
        };

        Self {
            // OpenSSH normally exports SSH_CLIENT and SSH_CONNECTION.
            // SSH_TTY is an additional PTY-only marker and is independently
            // sufficient when wrappers preserve it without the other two.
            endpoint: match (in_ssh_session, use_graphical_display) {
                (false, _) => ClipboardEndpoint::NativeHost,
                (true, true) => ClipboardEndpoint::ForwardedDisplay,
                (true, false) => ClipboardEndpoint::TerminalClient,
            },
            in_tmux: tmux.is_some_and(|value| !value.is_empty()),
        }
    }

    fn write_order(self) -> ClipboardWriteOrder {
        if self.endpoint == ClipboardEndpoint::TerminalClient {
            ClipboardWriteOrder::TerminalClientOnly
        } else {
            ClipboardWriteOrder::NativeHostThenTerminal
        }
    }

    fn permits_native_read(self) -> bool {
        self.endpoint != ClipboardEndpoint::TerminalClient
    }

    fn requires_terminal_paste(self) -> bool {
        self.endpoint == ClipboardEndpoint::TerminalClient
    }
}

// === Types ===

/// Metadata captured for a pasted clipboard image. Used by the composer to
/// render a status hint like `Pasted 1024x768 image (235KB) → <path>`.
#[derive(Clone)]
pub struct PastedImage {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub byte_len: usize,
}

impl PastedImage {
    /// Short human-readable summary, e.g. `1024x768 PNG`.
    pub fn short_label(&self) -> String {
        format!("{}x{} PNG", self.width, self.height)
    }

    /// Approximate file size suffix, e.g. `235KB`.
    pub fn size_label(&self) -> String {
        let kb = (self.byte_len as f64 / 1024.0).round() as u64;
        format!("{kb}KB")
    }
}

/// Clipboard payloads supported by the TUI.
#[cfg_attr(
    all(
        not(test),
        not(any(
            target_os = "macos",
            target_os = "windows",
            all(target_os = "linux", not(target_env = "ohos"))
        ))
    ),
    allow(dead_code)
)]
pub enum ClipboardContent {
    Text(String),
    Image(PastedImage),
}

struct TerminalClipboardWriteRequest {
    text: String,
    in_tmux: bool,
}

type TerminalClipboardWriteCompletion = std::result::Result<(), String>;

/// Serializes terminal-client clipboard writes on a bounded background lane.
///
/// OSC 52 ultimately writes to the terminal output stream, which can block
/// indefinitely under backpressure. tmux transport can likewise wait on a
/// stalled server. Keeping both operations on this worker means copy actions
/// never park the TUI input/render loop, while the single request slot bounds
/// memory and preserves copy order.
struct TerminalClipboardWriter {
    request_tx: std::sync::mpsc::SyncSender<TerminalClipboardWriteRequest>,
    completion_rx: std::sync::mpsc::Receiver<TerminalClipboardWriteCompletion>,
}

impl TerminalClipboardWriter {
    #[cfg(not(test))]
    fn spawn() -> Result<Self> {
        Self::spawn_with(|request| write_text_to_terminal_client(&request.text, request.in_tmux))
    }

    fn spawn_with<F>(write: F) -> Result<Self>
    where
        F: Fn(TerminalClipboardWriteRequest) -> Result<()> + Send + 'static,
    {
        let (request_tx, request_rx) = std::sync::mpsc::sync_channel(1);
        let (completion_tx, completion_rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("terminal-clipboard-writer".to_string())
            .spawn(move || {
                while let Ok(request) = request_rx.recv() {
                    let completion = write(request).map_err(|err| format!("{err:#}"));
                    if completion_tx.send(completion).is_err() {
                        break;
                    }
                }
            })
            .context("spawn terminal clipboard writer")?;
        Ok(Self {
            request_tx,
            completion_rx,
        })
    }

    fn enqueue(&self, text: &str, in_tmux: bool) -> Result<()> {
        let request = TerminalClipboardWriteRequest {
            text: text.to_string(),
            in_tmux,
        };
        self.request_tx.try_send(request).map_err(|err| match err {
            std::sync::mpsc::TrySendError::Full(_) => {
                anyhow::anyhow!("another terminal clipboard write is still queued")
            }
            std::sync::mpsc::TrySendError::Disconnected(_) => {
                anyhow::anyhow!("terminal clipboard writer stopped")
            }
        })
    }

    fn poll_completion(&self) -> Option<TerminalClipboardWriteCompletion> {
        self.completion_rx.try_recv().ok()
    }
}

/// Which transport took a clipboard write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyTransport {
    /// A native clipboard accepted the text before the write returned.
    /// Targets with no native clipboard (e.g. OpenHarmony) never build it.
    #[cfg_attr(
        all(
            not(test),
            not(any(
                target_os = "macos",
                target_os = "windows",
                all(target_os = "linux", not(target_env = "ohos"))
            ))
        ),
        allow(dead_code)
    )]
    Native,
    /// Handed to the terminal (OSC 52, or tmux's buffer). Terminals never
    /// acknowledge these, so success cannot be confirmed; a failure surfaces
    /// later through `poll_write_completion`.
    Terminal,
}

/// Clipboard reader/writer helper.
pub struct ClipboardHandler {
    terminal_context: TerminalClipboardContext,
    terminal_writer: Option<TerminalClipboardWriter>,
    #[cfg(all(target_os = "linux", not(target_env = "ohos"), not(test)))]
    primary: Option<primary::PrimarySelection>,
    #[cfg(test)]
    primary_enabled: bool,
    #[cfg(test)]
    primary_text: Option<String>,
    #[cfg(any(
        target_os = "macos",
        target_os = "windows",
        all(target_os = "linux", not(target_env = "ohos"))
    ))]
    clipboard: Option<Clipboard>,
    #[cfg(any(
        target_os = "macos",
        target_os = "windows",
        all(target_os = "linux", not(target_env = "ohos"))
    ))]
    clipboard_init_attempted: bool,
    #[cfg(test)]
    written_text: Vec<String>,
    #[cfg(test)]
    fail_text_writes: bool,
}

impl ClipboardHandler {
    /// Create a new clipboard handler without connecting.
    ///
    /// The actual clipboard connection is deferred to first use
    /// (`ensure_clipboard`) so that startup on hosts without an X11/Wayland
    /// server (headless, WSL2) never blocks the TUI event loop.
    pub fn new() -> Self {
        Self::with_terminal_context(TerminalClipboardContext::detect())
    }

    fn with_terminal_context(terminal_context: TerminalClipboardContext) -> Self {
        Self {
            terminal_context,
            terminal_writer: None,
            #[cfg(all(target_os = "linux", not(target_env = "ohos"), not(test)))]
            primary: None,
            #[cfg(test)]
            primary_enabled: false,
            #[cfg(test)]
            primary_text: None,
            #[cfg(any(
                target_os = "macos",
                target_os = "windows",
                all(target_os = "linux", not(target_env = "ohos"))
            ))]
            clipboard: None,
            #[cfg(any(
                target_os = "macos",
                target_os = "windows",
                all(target_os = "linux", not(target_env = "ohos"))
            ))]
            clipboard_init_attempted: false,
            #[cfg(test)]
            written_text: Vec::new(),
            #[cfg(test)]
            fail_text_writes: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(in_ssh_session: bool, in_tmux: bool) -> Self {
        Self::with_terminal_context(TerminalClipboardContext {
            endpoint: if in_ssh_session {
                ClipboardEndpoint::TerminalClient
            } else {
                ClipboardEndpoint::NativeHost
            },
            in_tmux,
        })
    }

    /// A clipboard whose writes go to the terminal (OSC 52) and always
    /// succeed, for receipt tests.
    #[cfg(test)]
    pub(crate) fn terminal_only_for_test() -> Self {
        let mut handler = Self::for_test(true, false);
        handler.terminal_writer =
            Some(TerminalClipboardWriter::spawn_with(|_| Ok(())).expect("terminal writer"));
        handler
    }

    /// Construct a deterministic unavailable clipboard for command tests.
    #[cfg(test)]
    pub(crate) fn unavailable_for_test(in_ssh_session: bool) -> Self {
        let mut handler = Self::for_test(in_ssh_session, false);
        handler.fail_text_writes = true;
        // Reads are unavailable too: never fall through to the host's real
        // clipboard from a test.
        #[cfg(any(
            target_os = "macos",
            target_os = "windows",
            all(target_os = "linux", not(target_env = "ohos"))
        ))]
        {
            handler.clipboard_init_attempted = true;
        }
        handler
    }

    /// SSH without a forwarded graphical display cannot synchronously read
    /// the terminal client's clipboard. Paste must be initiated by the local
    /// terminal so it arrives as bracketed paste (or a raw paste burst on
    /// older terminals).
    pub(crate) fn requires_terminal_paste(&self) -> bool {
        self.terminal_context.requires_terminal_paste()
    }

    pub(crate) fn uses_primary_selection(&self) -> bool {
        #[cfg(test)]
        {
            self.primary_enabled
        }
        #[cfg(not(test))]
        {
            cfg!(all(target_os = "linux", not(target_env = "ohos")))
        }
    }

    /// Automatic selection never writes CLIPBOARD or sends OSC 52. A remote
    /// terminal without a forwarded display owns its own selection and paste.
    pub(crate) fn write_primary_text(&mut self, text: &str) -> Result<()> {
        if !self.uses_primary_selection()
            || !self.terminal_context.permits_native_read()
            || text.is_empty()
            || text.len() > PRIMARY_MAX_BYTES
        {
            bail!("PRIMARY selection unavailable");
        }
        #[cfg(test)]
        {
            if self.fail_text_writes {
                bail!("test PRIMARY unavailable");
            }
            self.primary_text = Some(text.to_string());
            Ok(())
        }
        #[cfg(all(target_os = "linux", not(target_env = "ohos"), not(test)))]
        {
            self.primary_selection()?.write(text)
        }
        #[cfg(all(not(test), not(all(target_os = "linux", not(target_env = "ohos")))))]
        {
            bail!("PRIMARY selection unavailable")
        }
    }

    pub(crate) fn read_primary_text(&mut self) -> Option<String> {
        if !self.uses_primary_selection() || !self.terminal_context.permits_native_read() {
            return None;
        }
        #[cfg(test)]
        {
            if self.fail_text_writes {
                None
            } else {
                self.primary_text.clone()
            }
        }
        #[cfg(all(target_os = "linux", not(target_env = "ohos"), not(test)))]
        {
            self.primary_selection().ok()?.read()
        }
        #[cfg(all(not(test), not(all(target_os = "linux", not(target_env = "ohos")))))]
        {
            None
        }
    }

    #[cfg(all(target_os = "linux", not(target_env = "ohos"), not(test)))]
    fn primary_selection(&mut self) -> Result<&primary::PrimarySelection> {
        if self.primary.is_none() {
            self.primary = Some(primary::PrimarySelection::spawn()?);
        }
        Ok(self.primary.as_ref().expect("PRIMARY worker initialized"))
    }

    #[cfg(test)]
    pub(crate) fn enable_primary_for_test(&mut self) {
        self.primary_enabled = true;
    }

    /// Try to connect to the system clipboard, bounded by a short timeout.
    ///
    /// On Linux, `arboard::Clipboard::new()` opens a blocking X11 connection.
    /// When no X server is running (headless, WSL2 without WSLg), the connect
    /// call can hang indefinitely. We spawn the connection attempt on a
    /// temporary thread and give it 500 ms; if it doesn't return in time the
    /// handler stays in fallback/no-op mode and `read`/`write_text` fall
    /// through to their OSC 52 and pbcopy/powershell fallbacks.
    #[cfg(any(
        target_os = "macos",
        target_os = "windows",
        all(target_os = "linux", not(target_env = "ohos"))
    ))]
    fn ensure_clipboard(&mut self) {
        if self.clipboard_init_attempted {
            return;
        }
        self.clipboard_init_attempted = true;

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(Clipboard::new().ok());
        });
        self.clipboard = rx
            .recv_timeout(std::time::Duration::from_millis(500))
            .ok()
            .flatten();
    }

    /// Read the clipboard and return the parsed content.
    ///
    /// `workspace` is used as a fallback location when `~/.codewhale/` cannot
    /// be resolved (e.g. running with a stripped HOME in CI sandboxes).
    pub fn read(&mut self, workspace: &Path) -> Option<ClipboardContent> {
        self.read_content(workspace, false)
    }

    /// Composer paste preserves headings, lists, links, tables and code from
    /// rich applications. Credentials and configuration fields use `read` so
    /// their literal text is never interpreted as Markdown.
    pub fn read_markdown(&mut self, workspace: &Path) -> Option<ClipboardContent> {
        self.read_content(workspace, true)
    }

    fn read_content(
        &mut self,
        workspace: &Path,
        prefer_markdown: bool,
    ) -> Option<ClipboardContent> {
        // With no display exported over SSH there is no synchronously readable
        // clipboard endpoint. A forwarded X11/Wayland display is explicit and
        // remains readable, including its image clipboard.
        if !self.terminal_context.permits_native_read() {
            return None;
        }

        #[cfg(all(target_os = "linux", not(target_env = "ohos"), not(test)))]
        if !prefer_markdown && let Ok(text) = read_text_with_wlpaste() {
            return Some(ClipboardContent::Text(text));
        }

        #[cfg(any(
            target_os = "macos",
            target_os = "windows",
            all(target_os = "linux", not(target_env = "ohos"))
        ))]
        {
            self.ensure_clipboard();
            if let Some(clipboard) = self.clipboard.as_mut() {
                if prefer_markdown
                    && let Ok(html) = clipboard.get().html()
                    && let Some(markdown) = clipboard_markdown(&html)
                {
                    return Some(ClipboardContent::Text(markdown));
                }
                if let Ok(text) = clipboard.get_text() {
                    return Some(ClipboardContent::Text(text));
                }

                if let Ok(image) = clipboard.get_image()
                    && let Ok(pasted) = save_image_as_png(workspace, &image)
                {
                    return Some(ClipboardContent::Image(pasted));
                }
            }
        }

        #[cfg(all(target_os = "linux", not(target_env = "ohos"), not(test)))]
        if prefer_markdown && let Ok(text) = read_text_with_wlpaste() {
            return Some(ClipboardContent::Text(text));
        }

        let _ = (workspace, prefer_markdown);
        None
    }

    /// Write text to the clipboard.
    ///
    /// Native clipboard transports complete before this method returns. OSC 52
    /// and tmux terminal-client writes are validated and admitted to a bounded
    /// background worker; asynchronous transport failures are exposed through
    /// [`Self::poll_write_completion`].
    pub fn write_text(&mut self, text: &str) -> Result<()> {
        self.write_text_status(text).map(|_| ())
    }

    /// [`Self::write_text`], reporting which transport took the text, so a
    /// receipt can say "copied" only when a native clipboard confirmed it.
    /// The transport is only known here: native is tried first and OSC 52 or
    /// tmux is the fallback.
    pub fn write_text_status(&mut self, text: &str) -> Result<CopyTransport> {
        #[cfg(test)]
        {
            if let Some(writer) = self.terminal_writer.as_ref() {
                return writer
                    .enqueue(text, self.terminal_context.in_tmux)
                    .map(|()| CopyTransport::Terminal);
            }
            if self.fail_text_writes {
                bail!("test clipboard unavailable");
            }
            self.written_text.push(text.to_string());
            Ok(CopyTransport::Native)
        }

        #[cfg(not(test))]
        {
            if self.terminal_context.write_order() == ClipboardWriteOrder::TerminalClientOnly {
                return self
                    .enqueue_terminal_write(text)
                    .map(|()| CopyTransport::Terminal)
                    .map_err(|err| anyhow::anyhow!("Clipboard unavailable: {err}"));
            }

            #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
            if write_text_with_wlcopy(text).is_ok() {
                return Ok(CopyTransport::Native);
            }

            #[cfg(any(
                target_os = "macos",
                target_os = "windows",
                all(target_os = "linux", not(target_env = "ohos"))
            ))]
            {
                self.ensure_clipboard();
                if let Some(clipboard) = self.clipboard.as_mut()
                    && clipboard.set_text(text.to_string()).is_ok()
                {
                    return Ok(CopyTransport::Native);
                }
            }

            #[cfg(target_os = "macos")]
            if write_text_with_pbcopy(text).is_ok() {
                return Ok(CopyTransport::Native);
            }

            #[cfg(target_os = "windows")]
            if write_text_with_set_clipboard(text).is_ok() {
                return Ok(CopyTransport::Native);
            }

            self.enqueue_terminal_write(text)
                .map(|()| CopyTransport::Terminal)
                .map_err(|err| anyhow::anyhow!("Clipboard unavailable: {err}"))
        }
    }

    #[cfg(not(test))]
    fn enqueue_terminal_write(&mut self, text: &str) -> Result<()> {
        if !self.terminal_context.in_tmux {
            if text.len() > OSC52_MAX_BYTES {
                bail!("selection is too large for OSC 52 clipboard fallback");
            }
            if !io::stdout().is_terminal() {
                bail!("OSC 52 clipboard fallback requires a terminal");
            }
        }

        if self.terminal_writer.is_none() {
            self.terminal_writer = Some(TerminalClipboardWriter::spawn()?);
        }
        self.terminal_writer
            .as_ref()
            .expect("terminal clipboard writer initialized")
            .enqueue(text, self.terminal_context.in_tmux)
    }

    /// Return one completed background terminal clipboard write, if available.
    ///
    /// Successes are intentionally quiet because callers already show their
    /// normal copy receipt. Failures are drained by the event loop and replace
    /// that optimistic receipt with an actionable error.
    pub(crate) fn poll_write_completion(&self) -> Option<TerminalClipboardWriteCompletion> {
        self.terminal_writer
            .as_ref()
            .and_then(TerminalClipboardWriter::poll_completion)
    }

    #[cfg(test)]
    pub fn last_written_text(&self) -> Option<&str> {
        self.written_text.last().map(String::as_str)
    }
}

#[cfg(all(target_os = "macos", not(test)))]
fn write_text_with_pbcopy(text: &str) -> Result<()> {
    write_text_with_stdin_command("pbcopy", &[], text, "pbcopy")
}

#[cfg(all(target_os = "windows", not(test)))]
fn write_text_with_set_clipboard(text: &str) -> Result<()> {
    write_text_with_stdin_command(
        "powershell.exe",
        &["-NoProfile", "-Command", "Set-Clipboard -Value $input"],
        text,
        "Set-Clipboard",
    )
}

#[cfg(all(any(target_os = "macos", target_os = "windows"), not(test)))]
fn write_text_with_stdin_command(
    program: &str,
    args: &[&str],
    text: &str,
    label: &str,
) -> Result<()> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to run {label}: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(text.as_bytes())
            .map_err(|e| anyhow::anyhow!("Failed to write to {label}: {e}"))?;
    }
    let _ = std::thread::Builder::new()
        .name("clipboard-wait".to_string())
        .spawn(move || {
            let _ = child.wait();
        });
    Ok(())
}

#[cfg(all(target_os = "linux", not(target_env = "ohos"), not(test)))]
fn write_text_with_wlcopy(text: &str) -> Result<()> {
    write_text_with_wlcopy_using_argv("wl-copy", text)
}

#[cfg(all(target_os = "linux", not(target_env = "ohos"), not(test)))]
fn read_text_with_wlpaste() -> Result<String> {
    read_text_with_wlpaste_using_argv("wl-paste")
}

#[cfg(any(all(test, unix), all(target_os = "linux", not(target_env = "ohos"))))]
fn wait_wayland_clipboard_child(
    mut child: std::process::Child,
    program: &str,
) -> Result<std::process::ExitStatus> {
    use wait_timeout::ChildExt;
    let waited = child.wait_timeout(WAYLAND_CLIPBOARD_TIMEOUT);
    if let Ok(Some(status)) = waited {
        return Ok(status);
    }
    let _ = child.kill();
    let _ = std::thread::Builder::new()
        .name("clipboard-wait".to_string())
        .spawn(move || {
            let _ = child.wait();
        });
    match waited {
        Err(e) => bail!("Failed to wait on {program}: {e}"),
        _ => bail!(
            "{program} timed out after {}s",
            WAYLAND_CLIPBOARD_TIMEOUT.as_secs()
        ),
    }
}

#[cfg(any(all(test, unix), all(target_os = "linux", not(target_env = "ohos"))))]
fn read_text_with_wlpaste_using_argv(program: &str) -> Result<String> {
    use std::io::Read;
    let mut child = Command::new(program)
        .arg("--no-newline")
        .arg("--type")
        .arg("text/plain")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to run {program}: {e}"))?;
    let stdout = child.stdout.take();
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let _ = std::thread::Builder::new()
        .name("wl-paste-output".to_string())
        .spawn(move || {
            let mut bytes = Vec::new();
            let read = match stdout {
                Some(stdout) => stdout.take(WL_PASTE_MAX_BYTES + 1).read_to_end(&mut bytes),
                None => Ok(0),
            };
            let _ = tx.send(read.map(|_| bytes));
        });
    let status = wait_wayland_clipboard_child(child, program)?;
    let bytes = rx
        .recv_timeout(WAYLAND_CLIPBOARD_TIMEOUT)
        .map_err(|_| anyhow::anyhow!("{program} output did not close"))?
        .map_err(|e| anyhow::anyhow!("Failed to read {program} output: {e}"))?;
    if bytes.len() as u64 > WL_PASTE_MAX_BYTES {
        bail!("{program} returned more than {WL_PASTE_MAX_BYTES} bytes");
    }
    if !status.success() {
        bail!("{program} exited with {status}");
    }
    String::from_utf8(bytes).context("wl-paste returned non-UTF-8 text")
}

#[cfg(all(target_os = "linux", not(target_env = "ohos"), not(test)))]
fn write_text_with_wlcopy_using_argv(program: &str, text: &str) -> Result<()> {
    let mut child = Command::new(program)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to run {program}: {e}"))?;
    let stdin = child.stdin.take();
    let text = text.to_owned();
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let _ = std::thread::Builder::new()
        .name("wl-copy-input".to_string())
        .spawn(move || {
            let written = match stdin {
                Some(mut stdin) => stdin.write_all(text.as_bytes()),
                None => Ok(()),
            };
            let _ = tx.send(written);
        });
    let status = wait_wayland_clipboard_child(child, program)?;
    rx.recv_timeout(WAYLAND_CLIPBOARD_TIMEOUT)
        .map_err(|_| anyhow::anyhow!("{program} input did not close"))?
        .map_err(|e| anyhow::anyhow!("Failed to write to {program}: {e}"))?;
    if !status.success() {
        bail!("{program} exited with {status}");
    }
    Ok(())
}

#[cfg(not(test))]
fn write_text_to_terminal_client(text: &str, in_tmux: bool) -> Result<()> {
    if in_tmux {
        return write_text_with_tmux(text);
    }
    write_text_with_osc52(text)
}

#[cfg(not(test))]
fn write_text_with_tmux(text: &str) -> Result<()> {
    write_text_with_tmux_using_argv("tmux", &[], text)
}

/// Ask tmux to set both its paste buffer and the attached client's clipboard.
/// Unlike DCS passthrough, `load-buffer -w` works with tmux's default
/// `allow-passthrough off` policy and returns a non-zero status when tmux
/// cannot honor the command.
#[cfg(any(not(test), all(test, unix)))]
fn write_text_with_tmux_using_argv(program: &str, prefix_args: &[&str], text: &str) -> Result<()> {
    let mut child = Command::new(program)
        .args(prefix_args)
        .args(["load-buffer", "-w", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to run tmux load-buffer -w: {e}"))?;

    let write_result = child
        .stdin
        .take()
        .context("open tmux clipboard input")
        .and_then(|mut stdin| {
            stdin
                .write_all(text.as_bytes())
                .context("write tmux clipboard input")
        });
    let output = child
        .wait_with_output()
        .context("wait for tmux load-buffer -w")?;
    write_result?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        let detail = detail.trim();
        if detail.is_empty() {
            bail!("tmux load-buffer -w exited with {}", output.status);
        }
        bail!(
            "tmux load-buffer -w exited with {}: {detail}",
            output.status
        );
    }
    Ok(())
}

#[cfg(not(test))]
fn write_text_with_osc52(text: &str) -> Result<()> {
    let mut stdout = io::stdout();
    if !stdout.is_terminal() {
        bail!("OSC 52 clipboard fallback requires a terminal");
    }

    let sequence = osc52_sequence(text)?;
    stdout
        .write_all(sequence.as_bytes())
        .context("write OSC 52 clipboard sequence")?;
    stdout.flush().context("flush OSC 52 clipboard sequence")
}

fn osc52_sequence(text: &str) -> Result<String> {
    if text.len() > OSC52_MAX_BYTES {
        bail!("selection is too large for OSC 52 clipboard fallback");
    }

    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    Ok(format!("\x1b]52;c;{encoded}\x07"))
}

/// Pinned, no-follow writer for `dir`, which is `<root>/<name>`: `root` may be a
/// user-selected link (a relocated `~/.codewhale`), but nothing below it may be.
#[cfg(any(
    target_os = "macos",
    target_os = "windows",
    all(target_os = "linux", not(target_env = "ohos"))
))]
fn clipboard_image_target(
    dir: &Path,
    file_name: &str,
) -> Result<codewhale_util::files::WorkspaceFile> {
    let root = dir.parent().context("clipboard-images dir has no parent")?;
    let name = dir
        .file_name()
        .context("clipboard-images dir has no name")?;
    let relative = Path::new(name).join(file_name);
    // `dir` sits under the user's own state root unless no home was available
    // and it fell back to the workspace (`clipboard_images_dir_for_home`). Only
    // the state root may be a link the user made to relocate `~/.codewhale`; a
    // caller-supplied workspace root keeps the strict rule.
    let state_root = codewhale_paths::user_home()
        .is_some_and(|home| root == home.join(codewhale_paths::CODEWHALE_APP_DIR));
    let open = if state_root {
        codewhale_util::files::WorkspaceFile::open_resolved_root
    } else {
        codewhale_util::files::WorkspaceFile::open
    };
    open(root, &relative, true)
        .context("open clipboard-images destination (links are not followed)")
}

/// Encode an RGBA `ImageData` from arboard as PNG and persist it. Returns
/// the resulting path along with metadata used to render the paste hint.
#[cfg(any(
    target_os = "macos",
    target_os = "windows",
    all(target_os = "linux", not(target_env = "ohos"))
))]
fn save_image_as_png(workspace: &Path, image: &ImageData) -> Result<PastedImage> {
    save_image_as_png_in(
        &codewhale_runtime::config::clipboard_images_dir(workspace),
        image,
    )
}

/// Lower-level variant that writes into an explicit directory. Exposed so the
/// unit tests don't have to scribble inside the user's real home directory.
#[cfg(any(
    target_os = "macos",
    target_os = "windows",
    all(target_os = "linux", not(target_env = "ohos"))
))]
fn save_image_as_png_in(dir: &Path, image: &ImageData) -> Result<PastedImage> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let width = u32::try_from(image.width).context("clipboard image width too large")?;
    let height = u32::try_from(image.height).context("clipboard image height too large")?;

    // arboard hands us RGBA8 row-major. Copy into an ImageBuffer so we can
    // run it through the `image` crate's PNG encoder. We pad / truncate any
    // mismatched trailing bytes — defensive only, arboard already validates
    // the buffer length on every supported backend.
    let expected = (width as usize) * (height as usize) * 4;
    let mut rgba = image.bytes.as_ref().to_vec();
    if rgba.len() < expected {
        rgba.resize(expected, 0);
    } else if rgba.len() > expected {
        rgba.truncate(expected);
    }

    let buffer: ImageBuffer<Rgba<u8>, _> = ImageBuffer::from_raw(width, height, rgba)
        .context("clipboard image dimensions did not match buffer length")?;
    // Encode in memory and publish through the pinned no-follow writer: the
    // destination is created exclusively and owner-only, and a linked
    // directory or file name is refused rather than written through.
    let mut encoded = Vec::new();
    buffer
        .write_to(
            &mut std::io::Cursor::new(&mut encoded),
            image::ImageFormat::Png,
        )
        .context("encode clipboard PNG")?;
    let file_name = format!("clipboard-{timestamp}.png");
    clipboard_image_target(dir, &file_name)?
        .publish(&encoded)
        .context("write clipboard PNG")?;
    let path = dir.join(file_name);

    let byte_len = encoded.len();
    Ok(PastedImage {
        path,
        width,
        height,
        byte_len,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_transport_is_distinct_bounded_and_never_uses_ssh_host_clipboard() {
        let mut clipboard = ClipboardHandler::for_test(false, false);
        clipboard.enable_primary_for_test();
        clipboard.write_text("regular").unwrap();
        clipboard.write_primary_text("selected").unwrap();
        assert_eq!(clipboard.last_written_text(), Some("regular"));
        assert_eq!(clipboard.read_primary_text().as_deref(), Some("selected"));
        assert!(clipboard.write_primary_text("").is_err());
        assert!(
            clipboard
                .write_primary_text(&"x".repeat(PRIMARY_MAX_BYTES + 1))
                .is_err()
        );
        assert_eq!(clipboard.read_primary_text().as_deref(), Some("selected"));
        let mut remote = ClipboardHandler::for_test(true, false);
        remote.enable_primary_for_test();
        assert!(remote.write_primary_text("private").is_err());
        assert!(remote.read_primary_text().is_none());
        assert!(remote.last_written_text().is_none());
        let mut forwarded = ClipboardHandler::with_terminal_context(TerminalClipboardContext {
            endpoint: ClipboardEndpoint::ForwardedDisplay,
            in_tmux: false,
        });
        forwarded.enable_primary_for_test();
        forwarded.write_primary_text("forwarded").unwrap();
        assert_eq!(forwarded.read_primary_text().as_deref(), Some("forwarded"));
    }

    #[test]
    fn clipboard_markdown_preserves_rich_structure_and_code() {
        let html = r#"<h1>Release plan</h1><p>Keep <strong>authorship</strong> and
<a href="https://example.com/review">review</a>.</p>
<ul><li>Run gates</li><li>Dogfood</li></ul>
<pre><code>fn main() {
    println!("&lt;ready&gt;");
}</code></pre>
<table><tr><th>Gate</th><th>Result</th></tr><tr><td>Tests</td><td>Pass</td></tr></table>"#;
        let markdown = clipboard_markdown(html).expect("rich text converts");
        assert!(markdown.contains("# Release plan"), "{markdown}");
        assert!(markdown.contains("**authorship**"), "{markdown}");
        assert!(
            markdown.contains("[review](https://example.com/review)"),
            "{markdown}"
        );
        assert!(markdown.contains("Run gates") && markdown.contains("Dogfood"));
        assert!(markdown.contains("```"), "{markdown}");
        assert!(markdown.contains("println!(\"<ready>\");"), "{markdown}");
        assert!(
            markdown
                .lines()
                .any(|line| line.split('|').map(str::trim).collect::<Vec<_>>()
                    == ["", "Gate", "Result", ""]),
            "{markdown}"
        );
    }

    #[test]
    fn clipboard_markdown_omits_executable_markup_and_falls_back_losslessly() {
        assert_eq!(
            clipboard_markdown("<script>secret()</script><style>secret</style>"),
            None
        );
        assert_eq!(
            clipboard_markdown(&"x".repeat(MAX_CLIPBOARD_HTML_BYTES + 1)),
            None
        );
        let markdown = clipboard_markdown("<h1>Release plan</h1>").unwrap();
        assert_eq!(markdown.trim(), "Release plan\n===");
        assert!(markdown.trim().contains('\n'), "not a memory quick-add");
    }
    // ImageData from arboard is only available on these platforms.
    #[cfg(any(
        target_os = "macos",
        target_os = "windows",
        all(target_os = "linux", not(target_env = "ohos"))
    ))]
    use std::borrow::Cow;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn terminal_clipboard_write_does_not_wait_for_slow_transport() {
        let (transport_started_tx, transport_started_rx) = std::sync::mpsc::channel();
        let (release_transport_tx, release_transport_rx) = std::sync::mpsc::channel();
        let writer = TerminalClipboardWriter::spawn_with(move |request| {
            assert_eq!(request.text, "copied");
            assert!(!request.in_tmux);
            transport_started_tx
                .send(())
                .expect("announce transport start");
            release_transport_rx.recv().expect("release slow transport");
            Ok(())
        })
        .expect("spawn clipboard writer");
        let mut clipboard = ClipboardHandler::for_test(true, false);
        clipboard.terminal_writer = Some(writer);

        let (caller_returned_tx, caller_returned_rx) = std::sync::mpsc::channel();
        let caller = std::thread::spawn(move || {
            let result = clipboard.write_text("copied");
            caller_returned_tx
                .send((clipboard, result))
                .expect("report caller completion");
        });

        let (clipboard, result) =
            match caller_returned_rx.recv_timeout(std::time::Duration::from_millis(250)) {
                Ok(value) => value,
                Err(err) => {
                    let _ = release_transport_tx.send(());
                    caller.join().expect("join clipboard caller");
                    panic!("clipboard caller waited for slow transport: {err}");
                }
            };
        result.expect("queue clipboard write");
        transport_started_rx
            .recv_timeout(std::time::Duration::from_millis(250))
            .expect("worker started transport");
        assert!(
            clipboard.poll_write_completion().is_none(),
            "transport must remain pending until explicitly released"
        );

        release_transport_tx.send(()).expect("release transport");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if let Some(completion) = clipboard.poll_write_completion() {
                completion.expect("background clipboard completion");
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "background clipboard completion timed out"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        caller.join().expect("join clipboard caller");
    }

    #[test]
    fn terminal_clipboard_write_reports_background_failure() {
        let writer =
            TerminalClipboardWriter::spawn_with(|_| bail!("terminal clipboard transport denied"))
                .expect("spawn clipboard writer");
        writer
            .enqueue("copied", false)
            .expect("queue clipboard write");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if let Some(completion) = writer.poll_completion() {
                let err = completion.expect_err("transport should fail");
                assert!(err.contains("transport denied"), "{err}");
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "background clipboard failure timed out"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[cfg(any(
        target_os = "macos",
        target_os = "windows",
        all(target_os = "linux", not(target_env = "ohos"))
    ))]
    fn solid_rgba(width: u16, height: u16, rgba: [u8; 4]) -> ImageData<'static> {
        let mut bytes = Vec::with_capacity((width as usize) * (height as usize) * 4);
        for _ in 0..(width as usize * height as usize) {
            bytes.extend_from_slice(&rgba);
        }
        ImageData {
            width: width as usize,
            height: height as usize,
            bytes: Cow::Owned(bytes),
        }
    }

    #[test]
    #[cfg(any(
        target_os = "macos",
        target_os = "windows",
        all(target_os = "linux", not(target_env = "ohos"))
    ))]
    fn save_image_as_png_writes_valid_png() {
        let dir = tempfile::tempdir().unwrap();
        let img = solid_rgba(8, 4, [255, 0, 0, 255]);
        let pasted = save_image_as_png_in(dir.path(), &img).expect("encode png");

        assert_eq!(pasted.width, 8);
        assert_eq!(pasted.height, 4);
        assert!(pasted.byte_len > 0);
        assert_eq!(
            pasted.path.extension().and_then(|s| s.to_str()),
            Some("png")
        );

        // The first eight bytes of any PNG file are the magic signature; if
        // we ever regress to PPM or another format this will catch it.
        let header = std::fs::read(&pasted.path).unwrap();
        assert_eq!(&header[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    #[cfg(all(
        unix,
        any(
            target_os = "macos",
            all(target_os = "linux", not(target_env = "ohos"))
        )
    ))]
    fn pasted_images_are_private_and_never_written_through_a_link() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let img = solid_rgba(2, 2, [0, 255, 0, 255]);

        let real = root.path().join("clipboard-images");
        let pasted = save_image_as_png_in(&real, &img).expect("a plain directory works");
        let mode = std::fs::metadata(&pasted.path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "pasted images are owner-only");

        // A linked destination directory is refused and nothing lands behind it.
        let linked = root.path().join("linked-images");
        symlink(outside.path(), &linked).unwrap();
        assert!(save_image_as_png_in(&linked, &img).is_err());
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }

    #[test]
    fn clipboard_images_dir_uses_codewhale_home_directory() {
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();

        assert_eq!(
            codewhale_runtime::config::clipboard_images_dir_for_home(
                workspace.path(),
                Some(home.path())
            ),
            home.path().join(".codewhale").join("clipboard-images")
        );
    }

    #[test]
    fn clipboard_images_dir_falls_back_to_workspace_without_home() {
        let workspace = tempfile::tempdir().unwrap();

        assert_eq!(
            codewhale_runtime::config::clipboard_images_dir_for_home(workspace.path(), None),
            workspace.path().join("clipboard-images")
        );
    }

    #[test]
    fn pasted_image_labels_format_correctly() {
        let p = PastedImage {
            path: PathBuf::from("/tmp/x.png"),
            width: 1024,
            height: 768,
            byte_len: 235 * 1024,
        };
        assert_eq!(p.short_label(), "1024x768 PNG");
        assert_eq!(p.size_label(), "235KB");
    }

    #[test]
    fn ssh_detection_covers_openssh_markers_and_ignores_empty_values() {
        let client = TerminalClipboardContext::from_env_values(
            Some(OsStr::new("192.0.2.10 51234 22")),
            None,
            None,
            None,
            None,
            None,
            None,
        );
        let connection = TerminalClipboardContext::from_env_values(
            None,
            Some(OsStr::new("192.0.2.10 51234 192.0.2.20 22")),
            None,
            None,
            None,
            None,
            None,
        );
        let tty = TerminalClipboardContext::from_env_values(
            None,
            None,
            Some(OsStr::new("/dev/pts/4")),
            None,
            None,
            None,
            None,
        );
        let empty = TerminalClipboardContext::from_env_values(
            Some(OsStr::new("")),
            Some(OsStr::new("")),
            Some(OsStr::new("")),
            Some(OsStr::new("")),
            Some(OsStr::new("")),
            Some(OsStr::new("")),
            Some(OsStr::new("")),
        );

        assert_eq!(client.endpoint, ClipboardEndpoint::TerminalClient);
        assert_eq!(connection.endpoint, ClipboardEndpoint::TerminalClient);
        assert_eq!(tty.endpoint, ClipboardEndpoint::TerminalClient);
        assert_eq!(empty.endpoint, ClipboardEndpoint::NativeHost);
        assert!(!empty.in_tmux);
    }

    #[test]
    fn ssh_without_display_targets_terminal_client() {
        let remote_tmux = TerminalClipboardContext::from_env_values(
            Some(OsStr::new("192.0.2.10 51234 22")),
            None,
            None,
            None,
            None,
            None,
            Some(OsStr::new("/tmp/tmux-1000/default,1,0")),
        );
        let local =
            TerminalClipboardContext::from_env_values(None, None, None, None, None, None, None);

        assert_eq!(
            remote_tmux.write_order(),
            ClipboardWriteOrder::TerminalClientOnly
        );
        assert!(!remote_tmux.permits_native_read());
        assert!(remote_tmux.requires_terminal_paste());
        assert!(remote_tmux.in_tmux);
        assert_eq!(
            local.write_order(),
            ClipboardWriteOrder::NativeHostThenTerminal
        );
        assert!(local.permits_native_read());
        assert!(!local.requires_terminal_paste());
    }

    #[test]
    fn ssh_uses_forwarded_x11_or_explicit_graphical_clipboard_endpoint() {
        let x11 = TerminalClipboardContext::from_env_values(
            None,
            Some(OsStr::new("192.0.2.10 51234 192.0.2.20 22")),
            None,
            Some(OsStr::new("localhost:10.0")),
            None,
            None,
            None,
        );
        let wayland = TerminalClipboardContext::from_env_values(
            Some(OsStr::new("192.0.2.10 51234 22")),
            None,
            None,
            None,
            Some(OsStr::new("wayland-1")),
            Some(OsStr::new("graphical")),
            None,
        );

        for context in [x11, wayland] {
            assert_eq!(context.endpoint, ClipboardEndpoint::ForwardedDisplay);
            assert_eq!(
                context.write_order(),
                ClipboardWriteOrder::NativeHostThenTerminal
            );
            assert!(context.permits_native_read());
            assert!(!context.requires_terminal_paste());
        }

        let ambient_remote = TerminalClipboardContext::from_env_values(
            Some(OsStr::new("192.0.2.10 51234 22")),
            None,
            None,
            Some(OsStr::new(":0")),
            Some(OsStr::new("wayland-0")),
            None,
            None,
        );
        assert_eq!(ambient_remote.endpoint, ClipboardEndpoint::TerminalClient);

        let forced_terminal = TerminalClipboardContext::from_env_values(
            Some(OsStr::new("192.0.2.10 51234 22")),
            None,
            None,
            Some(OsStr::new("localhost:10.0")),
            None,
            Some(OsStr::new("terminal")),
            None,
        );
        assert_eq!(forced_terminal.endpoint, ClipboardEndpoint::TerminalClient);
    }

    #[test]
    fn osc52_sequence_encodes_text_clipboard_write() {
        let sequence = osc52_sequence("hello").expect("sequence");
        assert_eq!(sequence, "\x1b]52;c;aGVsbG8=\x07");
    }

    #[test]
    fn osc52_sequence_rejects_oversized_selection() {
        let text = "x".repeat(OSC52_MAX_BYTES + 1);
        let err = osc52_sequence(&text).expect_err("oversized should fail");
        assert!(
            err.to_string().contains("too large"),
            "unexpected error: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn tmux_helper_reports_command_failure() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("tmux");
        std::fs::write(
            &script,
            r#"#!/bin/sh
cat >/dev/null
echo 'clipboard denied' >&2
exit 42
"#,
        )
        .unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        // Another test thread may fork while this one still had the script
        // open for writing; the child keeps that descriptor until it execs,
        // and running the script meanwhile fails with "Text file busy". That
        // is the test's own race, not the behavior under test, so wait it out.
        let mut attempts = 0;
        let err = loop {
            let err = write_text_with_tmux_using_argv(script.to_str().unwrap(), &[], "copy")
                .expect_err("non-zero tmux status should fail");
            attempts += 1;
            if attempts >= 100 || !err.to_string().contains("Text file busy") {
                break err;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };

        assert!(err.to_string().contains("exited with"), "{err}");
        assert!(err.to_string().contains("clipboard denied"));
    }

    #[cfg(all(unix, not(target_env = "ohos")))]
    #[test]
    fn tmux_load_buffer_w_reaches_attached_client_with_default_passthrough_disabled() {
        use std::io::Read as _;

        // Every subprocess must inherit the same terminal environment, not
        // another fixture's transient PATH/TERM/multiplexer overrides.
        let _env = crate::test_support::lock_test_env();

        let version = match Command::new("tmux").arg("-V").output() {
            Ok(output) if output.status.success() => output,
            _ => return,
        };
        assert!(
            String::from_utf8_lossy(&version.stdout).starts_with("tmux "),
            "unexpected tmux version output"
        );

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let socket = format!("codewhale-clipboard-{}-{nonce}", std::process::id());

        struct TmuxServer(String);
        impl Drop for TmuxServer {
            fn drop(&mut self) {
                let _ = Command::new("tmux")
                    .args(["-L", self.0.as_str(), "kill-server"])
                    .status();
            }
        }
        let server = TmuxServer(socket);
        let started = Command::new("tmux")
            .args([
                "-L",
                server.0.as_str(),
                "-f",
                "/dev/null",
                "new-session",
                "-d",
            ])
            .status()
            .expect("start isolated tmux server");
        assert!(started.success(), "isolated tmux server should start");

        let option = |name: &str| {
            let output = Command::new("tmux")
                .args(["-L", server.0.as_str(), "show-options", "-gv", name])
                .output()
                .expect("read tmux option");
            assert!(output.status.success(), "read tmux option {name}");
            String::from_utf8(output.stdout)
                .expect("tmux option should be utf-8")
                .trim()
                .to_string()
        };
        assert_eq!(option("allow-passthrough"), "off");
        assert_eq!(option("set-clipboard"), "external");

        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system
            .openpty(portable_pty::PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("open attached-client PTY");
        let mut attach = portable_pty::CommandBuilder::new("tmux");
        for arg in ["-L", server.0.as_str(), "attach-session", "-t", "0"] {
            attach.arg(arg);
        }
        attach.env("TERM", "xterm-256color");
        let mut attached_client = pair
            .slave
            .spawn_command(attach)
            .expect("attach tmux client to PTY");
        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .expect("clone attached-client PTY reader");
        let (output_tx, output_rx) = std::sync::mpsc::channel();
        let reader_thread = std::thread::spawn(move || {
            let mut chunk = [0_u8; 4096];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(len) => {
                        if output_tx.send(chunk[..len].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        // Load-tolerant bounds, not the contract under test: on a machine
        // running a full parallel suite, tmux server startup and OSC 52
        // forwarding can both exceed a tight 3s wall clock (#5929). The test
        // still verifies the *content* of what reaches the attached client;
        // only how long it is willing to wait for a loaded machine changed.
        let attach_deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let clients = Command::new("tmux")
                .args(["-L", server.0.as_str(), "list-clients"])
                .output()
                .expect("list attached tmux clients");
            if clients.status.success() && !clients.stdout.is_empty() {
                break;
            }
            assert!(
                std::time::Instant::now() < attach_deadline,
                "tmux client did not attach to the test PTY"
            );
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        // A listed client can precede its terminal startup. tmux discards
        // clipboard requests before TTY_STARTED; actual PTY output establishes
        // that startup reached the terminal before the one request we verify.
        output_rx
            .recv_timeout(attach_deadline.saturating_duration_since(std::time::Instant::now()))
            .expect("attached tmux client should produce terminal startup output");
        while output_rx.try_recv().is_ok() {}

        let copied_text = "copy through default tmux";
        write_text_with_tmux_using_argv("tmux", &["-L", server.0.as_str()], copied_text)
            .expect("tmux-native clipboard request");

        let encoded = base64::engine::general_purpose::STANDARD.encode(copied_text.as_bytes());
        let expected_receipts = [
            format!("\x1b]52;;{encoded}\x07").into_bytes(),
            format!("\x1b]52;c;{encoded}\x07").into_bytes(),
            format!("\x1b]52;;{encoded}\x1b\\").into_bytes(),
            format!("\x1b]52;c;{encoded}\x1b\\").into_bytes(),
        ];
        let receipt_deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut attached_output = Vec::new();
        let receipt_received = loop {
            if expected_receipts.iter().any(|receipt| {
                attached_output
                    .windows(receipt.len())
                    .any(|window| window == receipt)
            }) {
                break true;
            }
            if std::time::Instant::now() >= receipt_deadline {
                break false;
            }
            match output_rx.recv_timeout(std::time::Duration::from_millis(50)) {
                Ok(bytes) => attached_output.extend_from_slice(&bytes),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break false,
            }
        };

        let buffer = Command::new("tmux")
            .args(["-L", server.0.as_str(), "show-buffer"])
            .output()
            .expect("read tmux buffer");
        assert!(buffer.status.success(), "tmux buffer should be readable");
        assert_eq!(buffer.stdout, copied_text.as_bytes());

        let _ = attached_client.kill();
        let _ = attached_client.wait();
        drop(pair.master);
        drop(output_rx);
        let _ = reader_thread.join();

        assert!(
            receipt_received,
            "attached tmux client did not receive the OSC 52 clipboard request: {attached_output:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn wl_paste_helper_reads_text_from_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("wl-paste");
        std::fs::write(
            &script,
            r#"#!/bin/sh
seen_no_newline=0
seen_text_plain=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --no-newline) seen_no_newline=1 ;;
    --type)
      shift
      [ "${1:-}" = "text/plain" ] && seen_text_plain=1
      ;;
  esac
  shift
done
[ "$seen_text_plain" -eq 1 ] || exit 40
if [ "$seen_no_newline" -eq 1 ]; then
  printf 'from-wayland'
else
  printf 'from-wayland\n'
fi
"#,
        )
        .unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        // A freshly written helper script can transiently report ETXTBSY
        // ("Text file busy") when a concurrent test in this parallel suite
        // forks while the write descriptor is still inherited. Retry the
        // exec briefly so this assertion exercises the helper contract
        // rather than the fork/exec window; the bound is load tolerance,
        // not the behavior under test (same class as #5929).
        let mut attempts = 0;
        let text = loop {
            match read_text_with_wlpaste_using_argv(script.to_str().unwrap()) {
                Ok(text) => break text,
                Err(error) => {
                    attempts += 1;
                    let busy = error.to_string().contains("Text file busy");
                    assert!(
                        busy && attempts < 100,
                        "read text through wl-paste helper: {error:#}"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        };

        assert_eq!(text, "from-wayland");
    }
}
