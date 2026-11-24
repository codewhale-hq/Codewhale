//! Host terminal window control (Windows): pin-to-top mini-window toggle.
//!
//! The TUI runs inside a terminal emulator, so its window is owned by the
//! OS. This module drives that host window directly through Win32: a window
//! handle is resolved via `GetConsoleWindow` (classic console hosts) or, when
//! that yields nothing — ConPTY hosts such as Windows Terminal and VS Code's
//! integrated terminal have no classic console window — by validating the
//! foreground window and then walking the parent process chain for the
//! nearest visible top-level window. `SetWindowPos` then toggles
//! always-on-top while shrinking/restoring the size: the "pin" action, like
//! a video player's PiP button. On non-Windows platforms every entry is a
//! no-op.
//!
//! The interaction entry is the right-click context menu (see
//! `crate::tui::mouse_ui::build_context_menu_entries`): a single pin item
//! toggles the host window between its normal state and a small
//! always-on-top window.
//!
//! Known limitation: with multiple host windows (several Windows Terminal or
//! VS Code windows) the ancestor-window fallback may resolve to a sibling
//! window rather than the one containing this tab — Win32 exposes no public
//! tab→window mapping. The foreground-window check mitigates this for the
//! common case (the user just right-clicked inside the host).

/// Default pixel size of the pinned (always-on-top) mini window.
/// The user can resize the terminal window while pinned; this is the default.
#[cfg(windows)]
const PINNED_W: i32 = 640;
#[cfg(windows)]
const PINNED_H: i32 = 400;

/// How many parent hops the fallback window walk may take before giving up
/// (guards against pathological process chains / loops).
#[cfg(windows)]
const MAX_ANCESTOR_HOPS: u32 = 8;

#[cfg(windows)]
mod imp {
    use super::*;
    use anyhow::{Context, Result, bail};
    use std::mem::size_of;
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT};
    use windows::Win32::System::Console::GetConsoleWindow;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GW_OWNER, GetForegroundWindow, GetWindow, GetWindowInfo, GetWindowRect,
        GetWindowThreadProcessId, HWND_NOTOPMOST, HWND_TOPMOST, IsWindowVisible, SW_MAXIMIZE,
        SW_RESTORE, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
        SetWindowPos, ShowWindowAsync, WINDOWINFO, WS_EX_TOPMOST, WS_MAXIMIZE,
    };
    use windows::core::BOOL;

    /// Pin state: remembers the pre-pin window rect so unpinning restores it,
    /// plus whether the window was maximized (unpin restores maximized then,
    /// not the ordinary recorded rect).
    struct State {
        host: Option<HostWindow>,
        saved_rect: Option<RECT>,
        was_maximized: bool,
    }

    impl State {
        const fn new() -> Self {
            Self {
                host: None,
                saved_rect: None,
                was_maximized: false,
            }
        }
    }

    // Only the worker touches restore geometry. Rendering never takes this lock.
    static STATE: Mutex<State> = Mutex::new(State::new());
    static PINNED: AtomicBool = AtomicBool::new(false);
    static BUSY: AtomicBool = AtomicBool::new(false);

    struct BusyGuard;

    impl Drop for BusyGuard {
        fn drop(&mut self) {
            BUSY.store(false, Ordering::Release);
        }
    }

    #[derive(Clone, Copy)]
    struct HostWindow {
        handle: isize,
        owner_pid: u32,
    }

    impl HostWindow {
        fn capture() -> Result<Self> {
            let hwnd = console_hwnd().context("no terminal host window found")?;
            let mut owner_pid = 0;
            unsafe {
                GetWindowThreadProcessId(hwnd, Some(&mut owner_pid));
            }
            if owner_pid == 0 {
                bail!("terminal host window no longer exists");
            }
            Ok(Self {
                handle: hwnd.0 as isize,
                owner_pid,
            })
        }

        fn hwnd(self) -> Result<HWND> {
            let hwnd = HWND(self.handle as *mut _);
            let mut owner_pid = 0;
            unsafe {
                GetWindowThreadProcessId(hwnd, Some(&mut owner_pid));
            }
            if owner_pid != self.owner_pid {
                bail!("terminal host window owner changed");
            }
            Ok(hwnd)
        }
    }

    #[derive(Clone, Copy)]
    struct Observation {
        rect: RECT,
        topmost: bool,
        maximized: bool,
    }

    impl Observation {
        fn matches(self, target: Self) -> bool {
            self.topmost == target.topmost
                && self.maximized == target.maximized
                && (target.maximized || self.rect == target.rect)
        }
    }

    fn observe(host: HostWindow) -> Result<Observation> {
        let hwnd = host.hwnd()?;
        let mut info = WINDOWINFO {
            cbSize: size_of::<WINDOWINFO>() as u32,
            ..Default::default()
        };
        let mut rect = RECT::default();
        unsafe {
            GetWindowInfo(hwnd, &mut info)?;
            GetWindowRect(hwnd, &mut rect)?;
        }
        let observed = Observation {
            rect,
            topmost: info.dwExStyle.contains(WS_EX_TOPMOST),
            maximized: info.dwStyle.contains(WS_MAXIMIZE),
        };
        PINNED.store(observed.topmost, Ordering::Release);
        Ok(observed)
    }

    fn wait_for(
        host: HostWindow,
        timeout: Duration,
        applied: impl Fn(Observation) -> bool,
    ) -> Result<bool> {
        let deadline = Instant::now() + timeout;
        loop {
            if applied(observe(host)?) {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(15));
        }
    }

    pub(super) fn start_toggle(
        completion_tx: Option<tokio::sync::mpsc::Sender<crate::tui::app::DispatchApplyFn>>,
    ) -> Result<()> {
        // Reserve delivery before changing the window. A headless test App has
        // no mailbox and cannot accidentally manipulate its real terminal.
        let permit = completion_tx
            .context("window completion mailbox is unavailable")?
            .try_reserve_owned()
            .context("window completion mailbox is full or closed")?;
        BUSY.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| anyhow::anyhow!("a window change is already in progress"))?;
        let busy = BusyGuard;
        // Foreground selection belongs to the user's action, before dispatch;
        // the worker must not pick a different window after focus changes.
        let host = HostWindow::capture()?;
        std::thread::Builder::new()
            .name("window-pin".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(|| toggle_pin(host))
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("window worker panicked")));
                let apply: crate::tui::app::DispatchApplyFn = Box::new(move |app, _, _| {
                    super::show_result(app, result);
                    Ok(())
                });
                permit.send(apply);
                drop(busy);
            })?;
        Ok(())
    }

    /// The host window the user sees, if one can be resolved.
    ///
    /// Classic console hosts (conhost, legacy cmd windows) hand back a real
    /// window from `GetConsoleWindow`. ConPTY hosts (Windows Terminal, VS
    /// Code integrated terminal) have no classic console window, so first
    /// check the foreground window (the user just right-clicked inside the
    /// host, so it is almost certainly the host window) and then fall back
    /// to the parent process chain.
    fn console_hwnd() -> Option<HWND> {
        // ConPTY hosts (Windows Terminal sets WT_SESSION, VS Code sets
        // TERM_PROGRAM) have no meaningful console window: GetConsoleWindow
        // may return a hidden ConPTY window whose SetWindowPos visibly does
        // nothing. Skip it entirely and resolve the real host window.
        let conpty = std::env::var("WT_SESSION").is_ok() || std::env::var("TERM_PROGRAM").is_ok();
        if !conpty {
            // SAFETY: no preconditions; invalid return handled.
            let hwnd = unsafe { GetConsoleWindow() };
            // SAFETY: invalid handles return false.
            if !hwnd.is_invalid() && unsafe { IsWindowVisible(hwnd) }.as_bool() {
                tracing::debug!("window_control: host window resolved via GetConsoleWindow");
                return Some(hwnd);
            }
        }
        tracing::debug!(
            conpty,
            "resolving host window (GetConsoleWindow skipped or unusable)"
        );
        if let Some(hwnd) = foreground_window_in_parent_chain(std::process::id()) {
            tracing::debug!("window_control: host window resolved via foreground check");
            return Some(hwnd);
        }
        if let Some(hwnd) = ancestor_top_level_window(std::process::id()) {
            tracing::debug!("window_control: host window resolved via ancestor walk");
            return Some(hwnd);
        }
        None
    }

    /// The foreground window, if it is visible and its process belongs to
    /// this process's parent chain (i.e. it is the host application's
    /// window). Visibility is required — a hidden foreground window cannot
    /// be the host the user is looking at.
    fn foreground_window_in_parent_chain(pid: u32) -> Option<HWND> {
        // SAFETY: no preconditions; invalid return handled.
        let foreground = unsafe { GetForegroundWindow() };
        // SAFETY: invalid handles return false.
        if foreground.is_invalid() || !unsafe { IsWindowVisible(foreground) }.as_bool() {
            return None;
        }
        let mut fg_pid = 0u32;
        // SAFETY: `fg_pid` is live for the call.
        unsafe {
            GetWindowThreadProcessId(foreground, Some(&mut fg_pid));
        }
        if fg_pid == 0 {
            return None;
        }
        let mut current = parent_process_id(pid);
        for _ in 0..MAX_ANCESTOR_HOPS {
            let p = current?;
            if p == fg_pid {
                return Some(foreground);
            }
            current = parent_process_id(p);
        }
        None
    }

    /// Nearest visible top-level window owned by the given process or any of
    /// its ancestors (parents first, then grandparents, …). Desktop-shell
    /// processes (explorer.exe owns the taskbar/desktop windows) are skipped
    /// — pinning those would be nonsensical.
    fn ancestor_top_level_window(pid: u32) -> Option<HWND> {
        let mut current = parent_process_id(pid);
        let mut hops = 0u32;
        while let Some(pid) = current {
            if hops >= MAX_ANCESTOR_HOPS {
                return None;
            }
            hops += 1;
            if let Some((_, name)) = process_entry(pid)
                && is_desktop_shell(&name)
            {
                current = parent_process_id(pid);
                continue;
            }
            if let Some(hwnd) = visible_top_level_window_for_pid(pid) {
                return Some(hwnd);
            }
            current = parent_process_id(pid);
        }
        None
    }

    /// Skip processes whose top-level windows are the desktop/taskbar or
    /// other shell chrome — never something to pin.
    fn is_desktop_shell(name: &str) -> bool {
        matches!(
            name.to_ascii_lowercase().as_str(),
            "explorer.exe" | "dwm.exe" | "shellexperiencehost.exe"
        )
    }

    /// Look up a process's parent PID and image name from a toolhelp
    /// snapshot. The snapshot handle is always closed.
    fn process_entry(pid: u32) -> Option<(u32, String)> {
        // SAFETY: returned handle is owned here; closed below.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }.ok()?;
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut found = None;
        // SAFETY: `entry` is live with dwSize initialized above.
        let mut ok = unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok();
        while ok {
            if entry.th32ProcessID == pid {
                let name_len = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                let name = String::from_utf16_lossy(&entry.szExeFile[..name_len]);
                found = Some((entry.th32ParentProcessID, name));
                break;
            }
            // SAFETY: `entry` is live with dwSize initialized above.
            ok = unsafe { Process32NextW(snapshot, &mut entry) }.is_ok();
        }
        // SAFETY: `snapshot` is owned here and not used after.
        unsafe {
            let _ = CloseHandle(snapshot);
        }
        found
    }

    fn parent_process_id(pid: u32) -> Option<u32> {
        process_entry(pid).map(|(ppid, _)| ppid)
    }

    /// First visible, unowned (true top-level) window owned by `pid`, if any.
    fn visible_top_level_window_for_pid(pid: u32) -> Option<HWND> {
        struct Ctx {
            target: u32,
            found: Option<HWND>,
        }
        let mut ctx = Ctx {
            target: pid,
            found: None,
        };
        unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
            // SAFETY: lparam carries the live `ctx` below; EnumWindows is synchronous.
            let ctx = unsafe { &mut *(lparam.0 as *mut Ctx) };
            let mut wpid = 0u32;
            // SAFETY: `hwnd` is valid per EnumWindows; `wpid` is live.
            unsafe {
                GetWindowThreadProcessId(hwnd, Some(&mut wpid));
                if wpid == ctx.target && IsWindowVisible(hwnd).as_bool() {
                    // Skip owned popups/child windows; only true top-levels.
                    let owner = GetWindow(hwnd, GW_OWNER).unwrap_or_default();
                    if owner.0.is_null() {
                        ctx.found = Some(hwnd);
                        return BOOL(0); // stop enumeration
                    }
                }
            }
            BOOL(1)
        }
        // SAFETY: `ctx` outlives the synchronous enumeration.
        unsafe {
            let _ = EnumWindows(Some(enum_proc), LPARAM(&mut ctx as *mut Ctx as isize));
        }
        ctx.found
    }

    fn toggle_pin(captured_host: HostWindow) -> Result<bool> {
        let mut state = STATE.lock().unwrap_or_else(|poison| poison.into_inner());
        // An unconfirmed request keeps its original target and restore data.
        // The next request restores that window, even if focus has changed.
        let restoring = state.host.is_some();
        let host = state.host.unwrap_or(captured_host);
        if let Err(error) = host.hwnd() {
            // Retire geometry only when the original owner is gone. A failed
            // observation must preserve it for the next restore attempt.
            *state = State::new();
            PINNED.store(false, Ordering::Release);
            return Err(error);
        }
        let before = observe(host)?;
        if !restoring {
            state.host = Some(host);
            state.was_maximized = before.maximized;
            state.saved_rect = Some(before.rect);
            if before.maximized {
                if !unsafe { ShowWindowAsync(host.hwnd()?, SW_RESTORE) }.as_bool() {
                    bail!("terminal restore request was rejected");
                }
                if !wait_for(host, Duration::from_millis(800), |observed| {
                    !observed.maximized
                })? {
                    bail!("terminal restore was not observed before the deadline");
                }
                state.saved_rect = Some(observe(host)?.rect);
            }
        }

        let saved = state
            .saved_rect
            .context("terminal restore geometry is unavailable")?;
        let target = Observation {
            topmost: !restoring,
            maximized: restoring && state.was_maximized,
            rect: if restoring {
                saved
            } else {
                RECT {
                    left: saved.left,
                    top: saved.top,
                    right: saved.left + PINNED_W,
                    bottom: saved.top + PINNED_H,
                }
            },
        };
        // Both mutations post to the foreign window's input queue. Do not
        // activate it after the user has moved focus while the worker runs.
        let flags = SWP_SHOWWINDOW | SWP_ASYNCWINDOWPOS | SWP_NOACTIVATE;
        for attempt in 0..2 {
            let hwnd = host.hwnd()?;
            unsafe {
                SetWindowPos(
                    hwnd,
                    Some(if restoring {
                        HWND_NOTOPMOST
                    } else {
                        HWND_TOPMOST
                    }),
                    target.rect.left,
                    target.rect.top,
                    target.rect.right - target.rect.left,
                    target.rect.bottom - target.rect.top,
                    if target.maximized {
                        flags | SWP_NOMOVE | SWP_NOSIZE
                    } else {
                        flags
                    },
                )?;
                if target.maximized && !ShowWindowAsync(hwnd, SW_MAXIMIZE).as_bool() {
                    bail!("terminal maximize request was rejected");
                }
            }
            let applied = wait_for(host, Duration::from_millis(400), |observed| {
                observed.matches(target)
            })?;
            tracing::info!(
                applied,
                restoring,
                attempt,
                "window_control: observed window result"
            );
            if applied {
                if restoring {
                    *state = State::new();
                }
                return Ok(target.topmost);
            }
        }
        bail!("terminal window change was not observed before the deadline")
    }

    pub(super) fn pinned() -> bool {
        PINNED.load(Ordering::Acquire)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn desktop_shell_filter_matches_image_names() {
            assert!(is_desktop_shell("ShellExperienceHost.exe"));
            assert!(is_desktop_shell("explorer.exe"));
            assert!(is_desktop_shell("dwm.exe"));
            assert!(!is_desktop_shell("code.exe"));
        }

        #[test]
        fn pin_receipt_requires_observed_geometry_and_topmost_state() {
            let target = Observation {
                rect: RECT {
                    left: 20,
                    top: 30,
                    right: 660,
                    bottom: 430,
                },
                topmost: true,
                maximized: false,
            };
            assert!(target.matches(target));
            assert!(
                !Observation {
                    topmost: false,
                    ..target
                }
                .matches(target)
            );
            assert!(
                !Observation {
                    maximized: true,
                    ..target
                }
                .matches(target)
            );
            assert!(
                !Observation {
                    rect: RECT {
                        right: 1020,
                        ..target.rect
                    },
                    ..target
                }
                .matches(target)
            );
            assert!(
                !Observation {
                    rect: RECT {
                        left: 30,
                        right: 670,
                        ..target.rect
                    },
                    ..target
                }
                .matches(target)
            );
        }

        #[test]
        fn restore_receipt_requires_observed_maximize_and_unpin() {
            let target = Observation {
                rect: RECT::default(),
                topmost: false,
                maximized: true,
            };
            assert!(
                Observation {
                    rect: RECT {
                        left: 0,
                        top: 0,
                        right: 1920,
                        bottom: 1080
                    },
                    ..target
                }
                .matches(target)
            );
            assert!(
                !Observation {
                    maximized: false,
                    ..target
                }
                .matches(target)
            );
            assert!(
                !Observation {
                    topmost: true,
                    ..target
                }
                .matches(target)
            );
        }

        #[test]
        fn renderer_snapshot_does_not_wait_for_worker_state_lock() {
            let state = STATE.lock().unwrap_or_else(|poison| poison.into_inner());
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            let reader = std::thread::spawn(move || tx.send(pinned()).unwrap());
            let observed = rx.recv_timeout(Duration::from_secs(1));
            // Release even on failure, so a regression cannot hang the suite.
            drop(state);
            reader.join().unwrap();
            assert!(observed.is_ok(), "rendering waited for the window worker");
        }

        #[test]
        fn headless_dispatch_rejects_before_resolving_or_changing_a_window() {
            let error = start_toggle(None).unwrap_err();
            assert!(error.to_string().contains("mailbox is unavailable"));
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub(super) fn start_toggle(
        _completion_tx: Option<tokio::sync::mpsc::Sender<crate::tui::app::DispatchApplyFn>>,
    ) -> anyhow::Result<()> {
        anyhow::bail!("window pinning is only supported on Windows")
    }

    pub(super) fn pinned() -> bool {
        false
    }
}

/// Whether host-window control is available on this platform.
/// Only Windows consoles can be driven from inside the TUI.
pub(crate) fn available() -> bool {
    cfg!(windows)
}

/// Request a window change without blocking input or claiming it has applied.
/// Both entry points share the worker and its observed completion receipt.
pub(crate) fn toggle_pin(app: &mut crate::tui::app::App) {
    if let Err(error) = imp::start_toggle(app.dispatch_completion_tx.clone()) {
        show_result(app, Err(error));
    }
}

fn show_result(app: &mut crate::tui::app::App, result: anyhow::Result<bool>) {
    use crate::tui::app::StatusToastLevel;
    use codewhale_localization::MessageId;
    let (message, level) = match result {
        Ok(true) => (MessageId::WindowPinActive, StatusToastLevel::Info),
        Ok(false) => (MessageId::WindowPinReleased, StatusToastLevel::Info),
        Err(error) => {
            tracing::warn!(%error, "window_control: window change failed or unconfirmed");
            (MessageId::WindowPinFailed, StatusToastLevel::Warning)
        }
    };
    app.push_status_toast(app.tr(message).into_owned(), level, Some(8_000));
    app.needs_redraw = true;
}

/// Whether the host window is currently the pinned (always-on-top mini)
/// state. The TUI reads this each frame to switch to the mini-window layout.
pub(crate) fn pinned() -> bool {
    imp::pinned()
}
