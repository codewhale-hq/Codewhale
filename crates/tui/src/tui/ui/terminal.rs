//! Terminal lifecycle: raw mode, alternate screen, keyboard-enhancement and
//! bracketed-paste flags, viewport recapture, and the input-event pump's
//! polling primitives.
//!
//! Moved verbatim out of `ui.rs`.

use super::*;

pub(crate) fn next_terminal_event(
    input: &TerminalInputPump,
    pending: &mut VecDeque<ObservedTerminalEvent>,
    timeout: Duration,
) -> io::Result<Option<ObservedTerminalEvent>> {
    if let Some(event) = pending.pop_front() {
        return Ok(Some(event));
    }
    let event = input.recv_timeout(timeout)?;
    if let Some(observed) = event.as_ref() {
        observe_terminal_attention(&observed.event);
    }
    Ok(event)
}

pub(crate) fn try_next_terminal_event(
    input: &TerminalInputPump,
    pending: &mut VecDeque<ObservedTerminalEvent>,
) -> io::Result<Option<ObservedTerminalEvent>> {
    if let Some(event) = pending.pop_front() {
        return Ok(Some(event));
    }
    let event = input.try_recv()?;
    if let Some(observed) = event.as_ref() {
        observe_terminal_attention(&observed.event);
    }
    Ok(event)
}

/// Drain input that Codewhale already read before releasing the terminal.
///
/// Ordinary buffered input is discarded so it cannot leak into the child.
/// Escape and Ctrl+C are different: they are cancellation authority. If one
/// is pending, preserve the complete input sequence and refuse the handoff so
/// the normal event loop can process it.
pub(crate) fn prepare_terminal_input_handoff(
    input: &TerminalInputPump,
    pending: &mut VecDeque<ObservedTerminalEvent>,
) -> io::Result<bool> {
    let mut drained = VecDeque::new();
    while let Some(event) = input.try_recv()? {
        drained.push_back(event);
    }
    let interrupted = pending
        .iter()
        .chain(drained.iter())
        .any(|observed| terminal_event_interrupts_child_handoff(&observed.event));
    if interrupted {
        pending.extend(drained);
        return Ok(false);
    }
    pending.clear();
    Ok(true)
}

fn terminal_event_interrupts_child_handoff(event: &Event) -> bool {
    let Event::Key(key) = event else {
        return false;
    };
    if key.kind == KeyEventKind::Release {
        return false;
    }
    let mut key = *key;
    normalize_raw_ctrl_c(&mut key);
    matches!(key.code, KeyCode::Esc)
        || matches!(key.code, KeyCode::Char('c')) && key.modifiers.contains(KeyModifiers::CONTROL)
}

pub(crate) fn collect_pending_terminal_events(
    input: &TerminalInputPump,
    pending: &mut VecDeque<ObservedTerminalEvent>,
) -> io::Result<()> {
    while let Some(observed) = input.try_recv()? {
        // Focus is notification authority, not merely a render event. Apply
        // it at pump receipt so a queued FocusGained cannot sit behind an
        // engine TurnComplete and produce a false background notification.
        observe_terminal_attention(&observed.event);
        pending.push_back(observed);
    }
    Ok(())
}

fn observe_terminal_attention(event: &Event) {
    match event {
        Event::FocusGained => crate::tui::notifications::set_terminal_focused(true),
        Event::FocusLost => {
            crate::tui::notifications::set_terminal_focused(false);
            crate::tui::hover_layer::clear_pointer();
        }
        _ => {}
    }
}

/// Refuse to enter raw mode unless both interactive streams are TTYs.
///
/// Keeping this check independent from `std::io` makes the launch contract
/// testable without trying to manipulate the test runner's own terminal.
pub(crate) fn require_interactive_terminal(stdin_is_tty: bool, stdout_is_tty: bool) -> Result<()> {
    if stdin_is_tty && stdout_is_tty {
        return Ok(());
    }
    Err(anyhow::anyhow!(
        "Codewhale TUI requires an interactive terminal (stdin and stdout must be a TTY).\n\
         Open a real terminal (Terminal.app, iTerm, Windows Terminal, …) and run `codew` \
         or `codewhale` there — not from a pipe, cron job, or non-TTY launcher.\n\
         For headless prompts use `codewhale exec \"…\"` instead."
    ))
}

/// Refuse to enter terminal modes from a background Unix process group.
///
/// A TTY can still report `isatty(3) == true` after a shell has suspended the
/// process. Reading from that background group triggers `SIGTTIN`; enabling
/// mouse or keyboard protocols before that stop poisons the shell with raw
/// escape reports. Check foreground ownership before the first mode change.
#[cfg(unix)]
pub(crate) fn require_foreground_terminal_owner() -> Result<()> {
    // SAFETY: both calls are read-only process/terminal queries on the
    // controlling stdin descriptor and require no borrowed memory.
    let (terminal_pgid, process_pgid) =
        unsafe { (libc::tcgetpgrp(libc::STDIN_FILENO), libc::getpgrp()) };
    if terminal_pgid < 0 {
        return Err(anyhow::anyhow!(
            "Codewhale TUI could not verify foreground terminal ownership: {}",
            io::Error::last_os_error()
        ));
    }
    validate_foreground_process_group(terminal_pgid, process_pgid)
}

#[cfg(not(unix))]
pub(crate) fn require_foreground_terminal_owner() -> Result<()> {
    Ok(())
}

#[cfg(unix)]
pub(crate) fn validate_foreground_process_group(
    terminal_pgid: libc::pid_t,
    process_pgid: libc::pid_t,
) -> Result<()> {
    if terminal_pgid == process_pgid {
        return Ok(());
    }
    Err(anyhow::anyhow!(
        "Codewhale TUI cannot start from a background or suspended terminal job \
         (terminal foreground process group {terminal_pgid}, Codewhale process group {process_pgid}).\n\
         Run `fg` to foreground the job or launch `codew` in a new terminal. \
         For automated prompts use `codewhale exec \"…\"` instead."
    ))
}

pub(crate) fn subagent_terminal_projection_from_mailbox(
    message: &MailboxMessage,
) -> Option<(&str, SubAgentStatus, Option<String>)> {
    match message {
        MailboxMessage::Completed { agent_id, summary } => Some((
            agent_id.as_str(),
            SubAgentStatus::Completed,
            Some(summary.clone()),
        )),
        MailboxMessage::Failed { agent_id, error } => Some((
            agent_id.as_str(),
            SubAgentStatus::Failed(error.clone()),
            Some(error.clone()),
        )),
        MailboxMessage::Interrupted { agent_id, reason } => Some((
            agent_id.as_str(),
            SubAgentStatus::Interrupted(reason.clone()),
            Some(reason.clone()),
        )),
        MailboxMessage::Cancelled { agent_id } => Some((
            agent_id.as_str(),
            SubAgentStatus::Cancelled,
            Some("cancelled".to_string()),
        )),
        _ => None,
    }
}

pub(crate) fn terminal_input_recovery_relevant(app: &App, has_running_agents: bool) -> bool {
    app.is_loading
        || has_running_agents
        || app.is_compacting
        || app.is_purging
        || matches!(app.runtime_turn_status.as_deref(), Some("in_progress"))
        || active_turn_has_running_tool(app)
}

/// Which screen the live terminal is on, for teardown paths that cannot see
/// `App` (the `TerminalCleanupGuard` drop, the panic hook).
///
/// A runtime `/inline` or `/fullscreen` switch moves the terminal after the
/// guard was built, so the guard must read the current screen rather than the
/// one startup chose — otherwise a rolled-back or switched session emits a
/// `LeaveAlternateScreen` for a screen it is not on (or skips the one it is).
static LIVE_ALT_SCREEN: AtomicBool = AtomicBool::new(false);

fn set_live_alt_screen(on_alt_screen: bool) {
    LIVE_ALT_SCREEN.store(on_alt_screen, Ordering::Release);
}

pub(crate) fn live_alt_screen() -> bool {
    LIVE_ALT_SCREEN.load(Ordering::Acquire)
}

/// Enter the alternate screen and, only once the escape went out, record it
/// as live. Every alternate-screen entry in the crate goes through here so
/// `live_alt_screen()` never says a screen the terminal is not on.
pub(crate) fn enter_alt_screen<W: Write>(writer: &mut W) -> io::Result<()> {
    execute!(writer, EnterAlternateScreen)?;
    set_live_alt_screen(true);
    Ok(())
}

/// Leave the alternate screen; the counterpart of [`enter_alt_screen`].
pub(crate) fn leave_alt_screen<W: Write>(writer: &mut W) -> io::Result<()> {
    if codewhale_tui_kit::mark::kitty_graphics_supported() {
        crate::tui::pet_watch::clear_images(writer)?;
    }
    execute!(writer, LeaveAlternateScreen)?;
    set_live_alt_screen(false);
    Ok(())
}

/// Program mouse capture for the screen the session is now on, from the same
/// rule startup used ([`ScreenMode::mouse_capture`]). Returns whether the
/// terminal's capture state changed.
pub(crate) fn apply_mouse_capture_for_screen<W: Write>(
    app: &mut App,
    writer: &mut W,
) -> io::Result<bool> {
    let wanted = app.screen_mode.mouse_capture(app.mouse_capture_preference);
    if wanted == app.use_mouse_capture {
        return Ok(false);
    }
    if wanted {
        execute!(writer, EnableMouseCapture)?;
    } else {
        execute!(writer, DisableMouseCapture)?;
    }
    app.use_mouse_capture = wanted;
    Ok(true)
}

fn refresh_composer_arrows_scroll(app: &mut App) {
    if !app.composer_arrows_scroll_explicit {
        app.composer_arrows_scroll =
            crate::tui::app::default_composer_arrows_scroll(app.use_mouse_capture);
    }
}

/// Rows a full-height inline viewport should request.
///
/// `Viewport::Inline` clamps to the terminal height anyway; asking for the
/// full height is what makes inline mode a drop-in replacement for the alt
/// screen rather than a shrunken strip.
fn inline_viewport_rows(backend: &ColorCompatBackend<Stdout>) -> u16 {
    ratatui::backend::Backend::size(backend)
        .map_or(24, |size| size.height)
        .max(1)
}

/// Build the ratatui terminal for `mode`.
///
/// Inline is the fallible one: `Terminal::with_options` measures the terminal
/// and appends lines to make room for the viewport, so it is the probe. It is
/// deliberately given the *full* terminal height, which makes the anchoring
/// independent of where the cursor happens to be — the newlines it prints
/// scroll whatever was on screen into the host's real scrollback instead of
/// being painted over.
pub(crate) fn build_app_terminal(
    backend: ColorCompatBackend<Stdout>,
    mode: ScreenMode,
) -> io::Result<AppTerminal> {
    match mode {
        ScreenMode::Fullscreen => Terminal::new(backend),
        ScreenMode::Inline => {
            let rows = inline_viewport_rows(&backend);
            Terminal::with_options(
                backend,
                ratatui::TerminalOptions {
                    viewport: ratatui::Viewport::Inline(rows),
                },
            )
        }
    }
}

/// Move the live terminal to `target` in place, rolling back on failure.
///
/// Stock ratatui cannot change an existing terminal's viewport, so the switch
/// rebuilds one over a fresh backend and only adopts it once the rebuild
/// succeeded. That ordering *is* the rollback: on failure the caller's
/// terminal was never touched, so undoing the alternate-screen escape restores
/// the previous mode exactly.
///
/// Nothing is committed to the host scrollback here. Inline mode paints a
/// full-height viewport, so no transcript row ever leaves the live region and
/// `Terminal::insert_before` has nothing to commit — see
/// `docs/CONFIGURATION.md`.
pub(crate) fn switch_screen_mode(
    terminal: &mut AppTerminal,
    app: &mut App,
    target: ScreenMode,
) -> std::result::Result<(), String> {
    let from = app.screen_mode;
    if from == target {
        return Ok(());
    }

    // Everything the previous mode staged must reach the terminal before the
    // escapes below move the cursor out from under it.
    let _ = terminal.backend_mut().flush();

    let carried = terminal.backend().respawn(io::stdout());
    let outcome = transition_screen(
        terminal,
        from,
        target,
        &mut |on_alt_screen| {
            let mut stdout = io::stdout();
            if on_alt_screen {
                enter_alt_screen(&mut stdout)?;
                #[cfg(windows)]
                codewhale_runtime::logging::set_verbose(false);
            } else {
                leave_alt_screen(&mut stdout)?;
                #[cfg(windows)]
                codewhale_runtime::logging::restore_verbose_state();
            }
            Ok(())
        },
        move || build_app_terminal(carried, target),
    );

    // Either way the screen changed underneath the app: repaint.
    app.needs_redraw = true;
    if outcome.is_ok() {
        app.screen_mode = target;
        // Mouse capture is a per-screen answer (inline leaves selection to
        // the terminal); re-derive it rather than keeping startup's.
        if let Err(err) = apply_mouse_capture_for_screen(app, terminal.backend_mut()) {
            tracing::warn!(?err, "mouse capture could not follow the screen switch");
        }
        refresh_composer_arrows_scroll(app);
        let _ = reset_terminal_viewport(terminal, app.synchronized_output_enabled);
    }
    outcome
}

/// Give an inline session a viewport the size of the terminal it is now in.
///
/// Stock ratatui keeps `Viewport::Inline(rows)` at the rows it was built with,
/// so after the window grows a "full-height" inline viewport would stop at the
/// old height and leave the new rows blank. Rebuild it over the same
/// negotiated backend facts, sized to the event-reported `size` (the
/// `terminal::size()` query can lag a resize — see the `#582` note in the
/// event loop).
///
/// The cursor is parked on row 0 first. A full-height viewport is anchored
/// there, and from row 0 the full height is exactly the room ratatui asks
/// for, so it appends no lines and the host scrollback gains nothing. In
/// inline mode the visible screen is the session's own frame, so nothing of
/// the user's is painted over.
pub(crate) fn refit_inline_viewport(terminal: &mut AppTerminal, size: Size) -> io::Result<()> {
    let _ = terminal.backend_mut().flush();
    let mut backend = terminal.backend().respawn(io::stdout());
    backend.force_size(size);
    backend.set_terminal_size(size);
    ratatui::backend::Backend::set_cursor_position(
        &mut backend,
        ratatui::layout::Position::ORIGIN,
    )?;
    *terminal = build_app_terminal(backend, ScreenMode::Inline)?;
    terminal.backend_mut().clear_forced_size();
    Ok(())
}

/// The fallible half of [`switch_screen_mode`], with the terminal escapes and
/// the rebuild injected so the rollback can be exercised against a fake
/// backend.
///
/// `alt_screen` programs the alternate screen and reports whether the escape
/// went out; `build` is the probe. Both are part of the switch: an escape
/// that failed to write is rolled back (the previous screen's escape is put
/// out again in case the failed write was partial) and the probe is never
/// run; a failed probe never touched `terminal`, so its rollback is the same
/// single call. The live-screen record is only ever moved by an escape that
/// succeeded, so teardown cannot be told a screen the terminal is not on.
fn transition_screen<B, F>(
    terminal: &mut Terminal<B>,
    from: ScreenMode,
    target: ScreenMode,
    alt_screen: &mut dyn FnMut(bool) -> io::Result<()>,
    build: F,
) -> std::result::Result<(), String>
where
    B: ratatui::backend::Backend,
    F: FnOnce() -> io::Result<Terminal<B>>,
{
    if let Err(err) = alt_screen(target.uses_alt_screen()) {
        let _ = alt_screen(from.uses_alt_screen());
        return Err(format!(
            "{} screen escape failed: {err}; staying in {}",
            target.as_str(),
            from.as_str()
        ));
    }
    match build() {
        Ok(rebuilt) => {
            *terminal = rebuilt;
            Ok(())
        }
        Err(err) => {
            if let Err(rollback) = alt_screen(from.uses_alt_screen()) {
                tracing::warn!(?rollback, "alternate-screen rollback escape failed");
            }
            Err(format!(
                "{} viewport probe failed: {err}; staying in {}",
                target.as_str(),
                from.as_str()
            ))
        }
    }
}

pub(crate) fn pause_terminal(
    terminal: &mut AppTerminal,
    use_alt_screen: bool,
    use_mouse_capture: bool,
    use_bracketed_paste: bool,
) -> Result<()> {
    // Focus reporting is about to be disabled. Fail closed to "focused" so
    // a child process or external editor cannot leave stale background state
    // that later emits a surprise Codewhale notification.
    crate::tui::notifications::set_terminal_focused(true);
    // #443: pop keyboard enhancement flags before handing the terminal
    // to a child process so it doesn't inherit a half-configured input
    // mode. Best-effort — terminals that didn't accept the flags
    // silently ignore the pop. Matches the shutdown and panic paths.
    pop_keyboard_enhancement_flags(terminal.backend_mut());
    disable_alternate_scroll_mode(terminal.backend_mut());
    // Every teardown step is attempted even when an earlier one fails: one
    // failed write must not leave mouse capture or raw mode on for the child
    // (U03-09). The first failure is still returned so the caller refuses the
    // handoff.
    let mut first_error: Option<io::Error> = None;
    let mut attempt = |result: io::Result<()>| {
        if let Err(error) = result {
            first_error.get_or_insert(error);
        }
    };
    attempt(execute!(terminal.backend_mut(), DisableFocusChange));
    attempt(disable_raw_mode());
    if use_alt_screen {
        attempt(leave_alt_screen(terminal.backend_mut()));
        #[cfg(windows)]
        codewhale_runtime::logging::restore_verbose_state();
    }
    if use_mouse_capture {
        attempt(execute!(terminal.backend_mut(), DisableMouseCapture));
    }
    if use_bracketed_paste {
        disable_bracketed_paste_mode(terminal.backend_mut());
    }
    match first_error {
        Some(error) => Err(error.into()),
        None => Ok(()),
    }
}

pub(crate) fn resume_terminal(
    terminal: &mut AppTerminal,
    use_alt_screen: bool,
    use_mouse_capture: bool,
    use_bracketed_paste: bool,
    sync_output_enabled: bool,
) -> Result<()> {
    // No trustworthy focus transition exists while reporting is disabled.
    // Resume from the quiet/focused state and wait for a real FocusLost.
    crate::tui::notifications::set_terminal_focused(true);
    enable_raw_mode()?;
    if use_alt_screen {
        enter_alt_screen(terminal.backend_mut())?;
        // Re-entering alt-screen after mode recovery — suppress verbose
        // CLI logging again so eprintln! doesn't leak into the TUI.
        #[cfg(windows)]
        codewhale_runtime::logging::set_verbose(false);
    }
    recover_terminal_modes(
        terminal.backend_mut(),
        use_mouse_capture,
        use_bracketed_paste,
    );
    // Cache the real terminal size *before* resetting the viewport, so that
    // reset_terminal_viewport → terminal.clear() → autoresize() → backend.size()
    // picks up the cached size instead of falling through to
    // crossterm::terminal::size() which may return stale buffer metadata
    // (especially on Windows after a secondary EnterAlternateScreen).
    if let Ok((cols, rows)) = crossterm::terminal::size() {
        terminal
            .backend_mut()
            .set_terminal_size(Size::new(cols, rows));
    }
    reset_terminal_viewport(terminal, sync_output_enabled)?;
    Ok(())
}

pub(crate) fn reset_terminal_viewport(
    terminal: &mut AppTerminal,
    sync_output_enabled: bool,
) -> Result<()> {
    // Reset scroll margins and origin mode before clearing. Some interactive
    // child processes leave DECSTBM/DECOM behind; if ratatui's diff renderer
    // then writes "row 0", terminals can place it relative to the leaked
    // scroll region and the whole viewport appears shifted down. We
    // deliberately do *not* emit CSI 2J/3J here — see TERMINAL_ORIGIN_RESET
    // for why; the immediately-following ratatui `terminal.clear()` flushes a
    // single clear via the diff renderer, which the alt-screen buffer absorbs
    // without visible flicker on the affected terminals.
    //
    // Wrap the reset+clear sequence in DEC 2026 synchronized-output mode
    // (`\x1b[?2026h` … `\x1b[?2026l`) so GPU-accelerated terminals
    // (Ghostty, VSCode, Kitty, WezTerm) defer rendering until the whole
    // frame is staged. Terminals that don't support it silently ignore.
    // The wrap is opt-out via `synchronized_output = "off"` for terminals
    // that mishandle the sequence (Ptyxis 50.x on VTE 0.84.x flashes the
    // whole viewport on each wrapped frame).
    if sync_output_enabled {
        let _ = terminal.backend_mut().write_all(BEGIN_SYNC_UPDATE);
    }

    let result = (|| -> Result<()> {
        terminal.backend_mut().write_all(TERMINAL_ORIGIN_RESET)?;
        terminal.clear()?;
        Ok(())
    })();

    // Always end the synchronized update, regardless of success or failure.
    if sync_output_enabled {
        let _ = terminal.backend_mut().write_all(END_SYNC_UPDATE);
    }
    let _ = terminal.backend_mut().flush();
    result
}

pub(crate) fn push_keyboard_enhancement_flags<W: Write>(writer: &mut W) {
    // crossterm's PushKeyboardEnhancementFlags command unconditionally
    // returns Unsupported on Windows (is_ansi_code_supported() == false), so
    // the ANSI escape is written directly on that platform. Modern Windows
    // terminals (VSCode integrated terminal, Windows Terminal ≥1.17) honour
    // the kitty keyboard protocol but crossterm's event reader does not
    // decode CSI u sequences on Windows (issue #1599). Write \033[>0u to
    // probe the protocol without enabling any flags — Enter stays as \n.
    #[cfg(windows)]
    {
        if let Err(err) = write!(writer, "\x1b[>0u").and_then(|()| writer.flush()) {
            tracing::debug!(
                target: "kitty_keyboard",
                ?err,
                "PushKeyboardEnhancementFlags direct write failed on Windows"
            );
        }
    }
    #[cfg(not(windows))]
    if let Err(err) = execute!(
        writer,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    ) {
        tracing::debug!(
            target: "kitty_keyboard",
            ?err,
            "PushKeyboardEnhancementFlags ignored (terminal lacks support)"
        );
    }
}

pub(crate) fn pop_keyboard_enhancement_flags<W: Write>(writer: &mut W) {
    // Mirror of push_keyboard_enhancement_flags: crossterm's
    // PopKeyboardEnhancementFlags also has is_ansi_code_supported() == false
    // on Windows, so write the pop escape directly to restore the terminal to
    // its pre-launch keyboard mode.
    // pub(crate) so the panic hook in main.rs and external_editor.rs can
    // also call the Windows-aware path instead of using the raw crossterm
    // execute!() macro which silently no-ops on Windows.
    #[cfg(windows)]
    {
        if let Err(err) = write!(writer, "\x1b[<1u").and_then(|()| writer.flush()) {
            tracing::debug!(
                target: "kitty_keyboard",
                ?err,
                "PopKeyboardEnhancementFlags direct write failed on Windows"
            );
        }
    }
    #[cfg(not(windows))]
    let _ = execute!(writer, PopKeyboardEnhancementFlags);
}

pub(crate) fn set_alternate_scroll_mode<W: Write>(writer: &mut W, enabled: bool) {
    let sequence = if enabled {
        ENABLE_ALT_SCROLL_MODE
    } else {
        DISABLE_ALT_SCROLL_MODE
    };
    if let Err(err) = writer.write_all(sequence).and_then(|()| writer.flush()) {
        tracing::debug!(
            ?err,
            enabled,
            "alternate-scroll terminal mode change ignored"
        );
    }
}

pub(crate) fn disable_alternate_scroll_mode<W: Write>(writer: &mut W) {
    set_alternate_scroll_mode(writer, false);
}

/// Best-effort terminal restoration for emergency exit paths
/// (panic hook, signal handlers). Mirrors the normal teardown in
/// `run_event_loop` but tolerates any subset of modes not actually being
/// active — every step is discarded on failure so a half-initialized TUI
/// (e.g. SIGINT during startup before `EnterAlternateScreen`) still gets
/// raw mode + kitty keyboard flags cleared, which is what causes the
/// `^[[>5u` shell pollution reported in #1583.
pub fn emergency_restore_terminal() {
    if codewhale_tui_kit::mark::kitty_graphics_supported() {
        let _ = crate::tui::pet_watch::clear_images(&mut std::io::stdout());
    }
    let mut stdout = std::io::stdout();
    codewhale_tui_kit::cursor_accent::restore_cursor_accent();
    pop_keyboard_enhancement_flags(&mut stdout);
    disable_alternate_scroll_mode(&mut stdout);
    let _ = execute!(stdout, DisableFocusChange);
    disable_bracketed_paste_mode(&mut stdout);
    let _ = execute!(stdout, DisableMouseCapture);
    let _ = disable_raw_mode();
    let _ = leave_alt_screen(&mut stdout);
}

/// On Windows, ensure the console input handle has `ENABLE_WINDOW_INPUT`
/// (0x0008) set. crossterm's `enable_raw_mode()` removes this flag, which
/// breaks IME composition (Chinese/Japanese/Korean input methods cannot
/// commit characters) on some Windows configurations (e.g. Windows Terminal
/// in conhost compatibility mode, or the legacy console with VT input).
///
/// Best-effort and idempotent. Silently ignored if the console handle or
/// mode query fails.
#[cfg(target_os = "windows")]
pub(crate) fn enable_windows_ime_console_mode() {
    use windows::Win32::System::Console::CONSOLE_MODE;
    const ENABLE_WINDOW_INPUT: CONSOLE_MODE = CONSOLE_MODE(0x0008);

    // SAFETY: Win32 console API is safe to call from any thread.
    // Failures (console handle invalid, mode query fails) are silently
    // ignored — this is a best-effort IME compatibility tweak.
    unsafe {
        let Ok(handle) = GetStdHandle(windows::Win32::System::Console::STD_INPUT_HANDLE) else {
            return;
        };
        let mut mode = CONSOLE_MODE(0);
        if GetConsoleMode(handle, &mut mode).is_err() {
            return;
        }
        if mode.0 & ENABLE_WINDOW_INPUT.0 == 0 {
            let _ = SetConsoleMode(handle, mode | ENABLE_WINDOW_INPUT);
        }
    }
}

/// Re-establish terminal mode flags. Idempotent and best-effort: each
/// underlying flag is silently discarded by terminals that don't support
/// it, and a single flag's failure doesn't prevent later flags from being
/// attempted.
///
/// **Canonical location for terminal-mode setup.** If you add a new mode
/// flag at startup or in `resume_terminal`, add it here too — `FocusGained`
/// recovery calls this and will silently fall behind otherwise.
///
/// There are three callers, and they must stay in step: `resume_terminal`
/// (after a child hands the terminal back, and after a job-control suspend),
/// and the `FocusGained` recovery path. A mode enabled in only one of them is a
/// mode that leaks into the shell on the other two paths (#6169).
///
/// Excluded by design: raw mode and the alternate screen — those persist
/// across focus events and are only re-established by `resume_terminal`
/// after a suspension, which always runs a separate path.
///
pub(crate) fn recover_terminal_modes<W: Write>(
    writer: &mut W,
    use_mouse_capture: bool,
    use_bracketed_paste: bool,
) {
    #[cfg(target_os = "windows")]
    enable_windows_ime_console_mode();

    pop_keyboard_enhancement_flags(writer);
    push_keyboard_enhancement_flags(writer);
    // DECSET 1007 converts wheel input into arrow keys. While mouse capture
    // is active, mouse reporting is the authoritative wheel channel and
    // terminals disagree about precedence (iTerm2 converts — #5223), so keep
    // 1007 off; #4026 already leaves it off without mouse capture.
    disable_alternate_scroll_mode(writer);
    if use_mouse_capture && let Err(err) = execute!(writer, EnableMouseCapture) {
        tracing::debug!(?err, "EnableMouseCapture ignored");
    }
    if use_bracketed_paste {
        try_enable_bracketed_paste_mode(writer);
    }
    if let Err(err) = execute!(writer, EnableFocusChange) {
        tracing::debug!(?err, "EnableFocusChange ignored");
    }
}

pub(crate) fn try_enable_bracketed_paste_mode<W: Write>(writer: &mut W) -> bool {
    match execute!(writer, EnableBracketedPaste) {
        Ok(()) => true,
        Err(err) => {
            tracing::debug!(?err, "EnableBracketedPaste ignored");
            false
        }
    }
}

pub(crate) fn disable_bracketed_paste_mode<W: Write>(writer: &mut W) {
    if let Err(err) = execute!(writer, DisableBracketedPaste) {
        tracing::debug!(?err, "DisableBracketedPaste ignored");
    }
}

pub(crate) fn terminal_event_needs_viewport_recapture(evt: &Event) -> bool {
    matches!(evt, Event::FocusGained)
}

/// Next frame-emission gate from one terminal event (#6311).
///
/// GTK3 pauses the frame clock on full occlusion while VTE keeps queuing
/// damage, so every frame emitted while covered becomes flicker backlog on
/// return. Focus loss therefore defers draws (state keeps ingesting;
/// `needs_redraw` stays set); focus gain re-arms with the existing
/// full-repaint recovery. Any key/mouse/paste input also re-arms: input
/// focus means a visible window, and it unsticks a lost `FocusGained`.
pub(crate) fn next_unfocused(unfocused: bool, evt: &Event) -> bool {
    match evt {
        Event::FocusLost => true,
        Event::FocusGained | Event::Key(_) | Event::Mouse(_) | Event::Paste(_) => false,
        _ => unfocused,
    }
}

/// Whether focus loss may defer frame emission at all (#6311).
///
/// Only GTK/VTE terminals (MATE, GNOME Terminal, Tilix, Terminator, ...)
/// queue damage while occluded and replay it on return; they all export
/// `VTE_VERSION`. Everywhere else an unfocused window is usually still
/// visible (side-by-side macOS/Windows windows, split panes), so freezing
/// frames on `FocusLost` made streaming output look stuck until the user
/// clicked, scrolled or typed back into the terminal.
///
/// `VTE_VERSION` only proves VTE is the *immediate* terminal when no
/// multiplexer sits in between: tmux started from GNOME Terminal inherits it,
/// yet tmux reports `FocusLost` for a still-visible split pane. Inside tmux
/// (`TMUX` set) frames keep flowing.
pub(crate) fn focus_loss_defers_frames(vte_version: Option<&str>, tmux: Option<&str>) -> bool {
    let inside_tmux = tmux.is_some_and(|v| !v.trim().is_empty());
    !inside_tmux && vte_version.is_some_and(|v| !v.trim().is_empty())
}

pub(crate) fn terminal_pause_has_live_owner(app: &App) -> bool {
    app.active_cell.as_ref().is_some_and(|active| {
        active.entries().iter().any(|cell| {
            matches!(
                cell,
                HistoryCell::Tool(ToolCell::Exec(exec)) if exec.status == ToolStatus::Running
            )
        })
    })
}

pub(crate) fn active_poll_ms(app: &App) -> u64 {
    if app.low_motion {
        96
    } else {
        UI_ACTIVE_POLL_MS
    }
}

pub(crate) fn idle_poll_ms(app: &App) -> u64 {
    if app.low_motion { 120 } else { UI_IDLE_POLL_MS }
}

/// How long the screen must have been unchanged, with no input and no engine
/// event, before the idle loop relaxes to [`UI_QUIESCENT_POLL_MS`] (#6728).
pub(crate) const UI_QUIESCENT_AFTER: Duration = Duration::from_secs(5);

/// Idle poll once the UI is quiescent. Input, resize and mouse events do not
/// wait for it: they arrive over the input pump's channel and return the loop
/// at once. It only bounds how late a source the loop merely *polls* (an
/// engine event, a background-task cell, the control socket, a remote
/// control event) is noticed while nothing else is happening: at most this
/// interval, instead of [`UI_IDLE_POLL_MS`].
///
/// Known limits: the loop is still polled, not woken. Nothing wakes it from an
/// engine event, a remote-control event, a background-task cell (prompt
/// suggestion, fleet or constitution draft, workspace context) or the control
/// socket, so each of those can land up to this long late once the UI has been
/// quiet for [`UI_QUIESCENT_AFTER`]. Waking from those writers would remove the
/// bound and is not done here.
pub(crate) const UI_QUIESCENT_POLL_MS: u64 = 250;

/// What the loop knows about itself that [`App`] alone does not say.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct IdleFacts {
    /// Live sub-agents (`running_agent_count(app) > 0`).
    pub(crate) has_running_agents: bool,
    /// A spinner, the underwater scene, or a launch animation wants frames
    /// faster than [`UI_QUIESCENT_POLL_MS`]; slower motion is woken by its
    /// own frame deadline.
    pub(crate) animation_active: bool,
    /// A terminal event is already buffered for this iteration.
    pub(crate) input_pending: bool,
    /// A sub-agent list refresh is waiting for room in the engine mailbox.
    pub(crate) pending_engine_op: bool,
}

/// Whether the UI has nothing to do *right now*: no turn, no live work, no
/// animation, no pending redraw, no toast about to expire, no modal ticking.
/// Pure on purpose. A state that is not listed here keeps the 48 ms poll, so
/// the list errs towards "busy".
pub(crate) fn ui_state_is_quiescent(app: &App, facts: &IdleFacts, now: Instant) -> bool {
    let toast_live = |toast: &StatusToast| toast.ttl_ms.is_some() && !toast.is_expired(now);
    let deciding = decision_prompt_waiting(app);
    !((app.is_loading && !deciding)
        || app.is_compacting
        || app.is_purging
        || (app.turn_started_at.is_some() && !deciding)
        || (matches!(app.runtime_turn_status.as_deref(), Some("in_progress")) && !deciding)
        || facts.has_running_agents
        || facts.animation_active
        || facts.input_pending
        || facts.pending_engine_op
        || app.needs_redraw
        || (!app.view_stack.is_empty() && !deciding)
        || app.onboarding != OnboardingState::None
        || app.redaction_gate
        || !app.queued_messages.is_empty()
        || app.queued_draft.is_some()
        || app.mcp_login.is_some()
        || !app.mcp_retries.is_empty()
        || app.quit_armed_until.is_some()
        || app.receipt_started_at.is_some()
        || app.viewport.selection_autoscroll.is_some()
        || app.status_toasts.iter().any(toast_live)
        || app.sticky_status.as_ref().is_some_and(toast_live))
}

pub(crate) fn decision_prompt_waiting(app: &App) -> bool {
    matches!(
        app.view_stack.top_kind(),
        Some(ModalKind::Approval | ModalKind::Elevation | ModalKind::UserInput)
    )
}

/// The idle poll for this iteration: [`idle_poll_ms`] normally, relaxed to
/// [`UI_QUIESCENT_POLL_MS`] once the UI has been quiescent (see
/// [`ui_state_is_quiescent`]) and quiet for [`UI_QUIESCENT_AFTER`]. `quiet_for`
/// is measured by the loop from the last terminal event, engine event or busy
/// state, so any of those restores the short poll on the next iteration.
pub(crate) fn idle_poll_duration(
    app: &App,
    facts: &IdleFacts,
    now: Instant,
    quiet_for: Duration,
) -> Duration {
    if quiet_for >= UI_QUIESCENT_AFTER && ui_state_is_quiescent(app, facts, now) {
        Duration::from_millis(UI_QUIESCENT_POLL_MS.max(idle_poll_ms(app)))
    } else {
        Duration::from_millis(idle_poll_ms(app))
    }
}

#[cfg(test)]
mod idle_poll_tests {
    use super::*;

    fn quiet_app() -> App {
        let mut app =
            crate::tui::test_support::test_app_with_options(crate::tui::app::TuiOptions {
                skip_onboarding: true,
                start_in_agent_mode: true,
                ..crate::tui::test_support::test_tui_options(std::path::PathBuf::from("."))
            });
        app.needs_redraw = false;
        app.low_motion = false;
        app
    }

    fn at(app: &App, facts: &IdleFacts, quiet_secs: u64) -> Duration {
        idle_poll_duration(app, facts, Instant::now(), Duration::from_secs(quiet_secs))
    }

    const FAST: Duration = Duration::from_millis(UI_IDLE_POLL_MS);
    const SLOW: Duration = Duration::from_millis(UI_QUIESCENT_POLL_MS);

    #[test]
    fn a_settled_ui_relaxes_only_after_the_quiet_period() {
        let app = quiet_app();
        let facts = IdleFacts::default();
        assert!(ui_state_is_quiescent(&app, &facts, Instant::now()));
        assert_eq!(
            at(&app, &facts, 0),
            FAST,
            "fresh activity keeps the fast poll"
        );
        assert_eq!(at(&app, &facts, 4), FAST, "inside the quiet period");
        assert_eq!(at(&app, &facts, UI_QUIESCENT_AFTER.as_secs()), SLOW);
        assert_eq!(at(&app, &facts, 600), SLOW);
    }

    #[test]
    fn the_relaxed_poll_is_never_shorter_than_the_reduced_motion_poll() {
        let mut app = quiet_app();
        app.low_motion = true;
        let facts = IdleFacts::default();
        assert_eq!(at(&app, &facts, 1), Duration::from_millis(120));
        assert_eq!(at(&app, &facts, 60), SLOW);
        assert!(SLOW >= Duration::from_millis(120));
    }

    #[test]
    fn any_live_state_keeps_the_fast_poll_however_long_it_has_been_quiet() {
        let now = Instant::now();
        let long = Duration::from_secs(3_600);
        let busy = |app: &App, facts: &IdleFacts, why: &str| {
            assert!(!ui_state_is_quiescent(app, facts, now), "{why}");
            assert_eq!(idle_poll_duration(app, facts, now, long), FAST, "{why}");
        };

        let facts = IdleFacts::default();
        let mut app = quiet_app();
        app.is_loading = true;
        busy(&app, &facts, "a turn is loading");

        let mut app = quiet_app();
        app.is_compacting = true;
        busy(&app, &facts, "compacting");

        let mut app = quiet_app();
        app.is_purging = true;
        busy(&app, &facts, "purging");

        let mut app = quiet_app();
        app.turn_started_at = Some(now);
        busy(&app, &facts, "a turn has started");

        let mut app = quiet_app();
        app.runtime_turn_status = Some("in_progress".to_string());
        busy(&app, &facts, "the runtime reports a turn in progress");

        let mut app = quiet_app();
        app.needs_redraw = true;
        busy(&app, &facts, "a redraw is owed");

        let mut app = quiet_app();
        app.quit_armed_until = Some(now + Duration::from_secs(2));
        busy(&app, &facts, "the quit prompt is armed");

        let mut app = quiet_app();
        app.receipt_started_at = Some(now);
        busy(&app, &facts, "a receipt is on screen and expires on a tick");

        let mut app = quiet_app();
        app.push_status_toast("saved", StatusToastLevel::Info, Some(4_000));
        app.needs_redraw = false;
        busy(&app, &facts, "a timed toast is showing");

        let app = quiet_app();
        for (facts, why) in [
            (
                IdleFacts {
                    has_running_agents: true,
                    ..IdleFacts::default()
                },
                "a sub-agent is running",
            ),
            (
                IdleFacts {
                    animation_active: true,
                    ..IdleFacts::default()
                },
                "something is animating",
            ),
            (
                IdleFacts {
                    input_pending: true,
                    ..IdleFacts::default()
                },
                "input is already buffered",
            ),
            (
                IdleFacts {
                    pending_engine_op: true,
                    ..IdleFacts::default()
                },
                "an engine op is waiting for mailbox room",
            ),
        ] {
            busy(&app, &facts, why);
        }
    }

    #[test]
    fn an_expired_toast_and_a_standing_error_do_not_hold_the_fast_poll() {
        let mut app = quiet_app();
        let facts = IdleFacts::default();
        let mut expired = StatusToast::new("old", StatusToastLevel::Info, Some(1));
        expired.created_at = Instant::now() - Duration::from_secs(60);
        app.status_toasts.push_back(expired);
        // A sticky error without a lifetime ("no model connected") is a
        // static line, not something that ticks.
        app.sticky_status = Some(StatusToast::new("no model", StatusToastLevel::Error, None));
        assert!(ui_state_is_quiescent(&app, &facts, Instant::now()));
        assert_eq!(at(&app, &facts, 30), SLOW);
    }

    #[test]
    fn a_queued_message_or_open_modal_is_not_quiescent() {
        let facts = IdleFacts::default();
        let mut app = quiet_app();
        app.queued_draft = Some(QueuedMessage::new("later".to_string(), None));
        assert!(!ui_state_is_quiescent(&app, &facts, Instant::now()));

        let mut app = quiet_app();
        app.onboarding = OnboardingState::Welcome;
        assert!(!ui_state_is_quiescent(&app, &facts, Instant::now()));
    }
}

#[cfg(test)]
mod screen_mode_tests {
    use super::*;
    use ratatui::backend::TestBackend;

    fn probe_failure() -> io::Error {
        io::Error::other("terminal refused the inline viewport")
    }

    #[test]
    fn failed_probe_rolls_the_screen_back_and_says_why() {
        let mut terminal =
            Terminal::new(TestBackend::new(20, 6)).expect("fullscreen test terminal");
        let mut alt_screen_writes: Vec<bool> = Vec::new();

        let error = transition_screen(
            &mut terminal,
            ScreenMode::Fullscreen,
            ScreenMode::Inline,
            &mut |on_alt_screen| {
                alt_screen_writes.push(on_alt_screen);
                Ok(())
            },
            || Err(probe_failure()),
        )
        .expect_err("a failing probe must not report a switch");

        assert!(
            error.contains("inline viewport probe failed"),
            "message must name the probe that failed: {error}"
        );
        assert!(
            error.contains("terminal refused the inline viewport"),
            "message must carry the terminal's own reason: {error}"
        );
        assert!(
            error.contains("staying in fullscreen"),
            "message must name the mode the user is left in: {error}"
        );
        // Left the alt screen for the probe, then went straight back to it.
        assert_eq!(alt_screen_writes, vec![false, true]);
        // The caller's terminal is the one it started with.
        assert_eq!(terminal.get_frame().area(), Rect::new(0, 0, 20, 6));
    }

    #[test]
    fn successful_probe_adopts_the_rebuilt_terminal() {
        let mut terminal =
            Terminal::new(TestBackend::new(20, 6)).expect("fullscreen test terminal");
        let mut alt_screen_writes: Vec<bool> = Vec::new();

        transition_screen(
            &mut terminal,
            ScreenMode::Fullscreen,
            ScreenMode::Inline,
            &mut |on_alt_screen| {
                alt_screen_writes.push(on_alt_screen);
                Ok(())
            },
            || {
                Terminal::with_options(
                    TestBackend::new(20, 6),
                    ratatui::TerminalOptions {
                        viewport: ratatui::Viewport::Inline(3),
                    },
                )
                .map_err(|err| io::Error::other(err.to_string()))
            },
        )
        .expect("a successful probe must switch");

        assert_eq!(alt_screen_writes, vec![false], "no rollback write");
        assert_eq!(
            terminal.get_frame().area().height,
            3,
            "inline viewport adopted"
        );
    }

    #[test]
    fn failed_screen_escape_rolls_back_before_the_probe_runs() {
        let mut terminal =
            Terminal::new(TestBackend::new(20, 6)).expect("fullscreen test terminal");
        let mut alt_screen_writes: Vec<bool> = Vec::new();
        let mut probed = false;

        let error = transition_screen(
            &mut terminal,
            ScreenMode::Fullscreen,
            ScreenMode::Inline,
            &mut |on_alt_screen| {
                alt_screen_writes.push(on_alt_screen);
                if on_alt_screen {
                    Ok(())
                } else {
                    Err(io::Error::other("stdout closed"))
                }
            },
            || {
                probed = true;
                Err(probe_failure())
            },
        )
        .expect_err("an escape that never went out must not report a switch");

        assert!(
            error.contains("inline screen escape failed") && error.contains("stdout closed"),
            "message must name the escape and the writer's reason: {error}"
        );
        assert!(error.contains("staying in fullscreen"), "{error}");
        assert!(!probed, "the probe must not run after a failed escape");
        // The failed leave, then the previous screen put back.
        assert_eq!(alt_screen_writes, vec![false, true]);
        assert_eq!(terminal.get_frame().area(), Rect::new(0, 0, 20, 6));
    }

    /// The live-screen record only moves on an escape that went out.
    #[cfg(not(windows))]
    #[test]
    fn live_screen_record_ignores_an_escape_that_failed_to_write() {
        struct Closed;
        impl Write for Closed {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("stdout closed"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Err(io::Error::other("stdout closed"))
            }
        }
        let mut sink: Vec<u8> = Vec::new();
        enter_alt_screen(&mut sink).expect("a writable sink takes the escape");
        assert!(live_alt_screen());
        assert!(leave_alt_screen(&mut Closed).is_err());
        assert!(
            live_alt_screen(),
            "a leave that never reached the terminal must not be recorded"
        );
        leave_alt_screen(&mut sink).expect("a writable sink takes the escape");
        assert!(!live_alt_screen());
    }

    #[test]
    fn mouse_capture_is_a_per_screen_answer() {
        // The one rule startup and the switch share: the preference only
        // applies on the alternate screen.
        assert!(ScreenMode::Fullscreen.mouse_capture(true));
        assert!(!ScreenMode::Fullscreen.mouse_capture(false));
        assert!(!ScreenMode::Inline.mouse_capture(true));
        assert!(!ScreenMode::Inline.mouse_capture(false));
    }

    /// Inline start with a capture-on preference, then `/fullscreen`: capture
    /// is recomputed per the rule and programmed on the terminal, and the
    /// way back turns it off again.
    #[cfg(not(windows))]
    #[test]
    fn switching_screens_recomputes_mouse_capture() {
        let mut app = crate::tui::test_support::test_app_with_options(
            crate::tui::test_support::test_tui_options(std::path::PathBuf::from(".")),
        );
        app.screen_mode = ScreenMode::Inline;
        app.mouse_capture_preference = true;
        app.use_mouse_capture = ScreenMode::Inline.mouse_capture(true);
        assert!(!app.use_mouse_capture, "inline start leaves capture off");

        let mut wire: Vec<u8> = Vec::new();
        app.screen_mode = ScreenMode::Fullscreen;
        assert!(apply_mouse_capture_for_screen(&mut app, &mut wire).expect("writable"));
        assert!(app.use_mouse_capture, "/fullscreen re-derives capture on");
        assert!(
            String::from_utf8_lossy(&wire).contains("\x1b[?1000h"),
            "EnableMouseCapture must reach the terminal: {wire:?}"
        );

        wire.clear();
        app.screen_mode = ScreenMode::Inline;
        assert!(apply_mouse_capture_for_screen(&mut app, &mut wire).expect("writable"));
        assert!(!app.use_mouse_capture, "/inline hands selection back");
        assert!(
            String::from_utf8_lossy(&wire).contains("\x1b[?1000l"),
            "DisableMouseCapture must reach the terminal: {wire:?}"
        );

        wire.clear();
        assert!(
            !apply_mouse_capture_for_screen(&mut app, &mut wire).expect("writable"),
            "an unchanged answer writes nothing"
        );
        assert!(wire.is_empty());
    }

    #[test]
    fn switching_screens_recomputes_derived_composer_arrows_only() {
        let mut app = crate::tui::test_support::test_app_with_options(
            crate::tui::test_support::test_tui_options(std::path::PathBuf::from(".")),
        );
        app.composer_arrows_scroll_explicit = false;

        app.use_mouse_capture = false;
        refresh_composer_arrows_scroll(&mut app);
        assert!(
            app.composer_arrows_scroll,
            "inline/no-capture uses arrows to scroll"
        );

        app.use_mouse_capture = true;
        refresh_composer_arrows_scroll(&mut app);
        assert!(
            !app.composer_arrows_scroll,
            "fullscreen/capture uses prompt history"
        );

        app.composer_arrows_scroll_explicit = true;
        app.composer_arrows_scroll = true;
        app.use_mouse_capture = true;
        refresh_composer_arrows_scroll(&mut app);
        assert!(
            app.composer_arrows_scroll,
            "explicit true survives a switch"
        );
        app.use_mouse_capture = false;
        refresh_composer_arrows_scroll(&mut app);
        assert!(
            app.composer_arrows_scroll,
            "explicit true survives the reverse switch"
        );
    }

    #[test]
    fn inline_viewport_asks_for_the_full_terminal_height() {
        // Inline is a drop-in for the alt screen, not a strip: the viewport is
        // the whole terminal, which is also what makes its anchoring
        // independent of where the cursor happened to be.
        let backend = crate::tui::color_compat::ColorCompatBackend::new(
            io::stdout(),
            codewhale_palette::ColorDepth::TrueColor,
            codewhale_palette::PaletteMode::Dark,
        );
        let mut backend = backend;
        backend.set_terminal_size(Size::new(80, 24));
        assert_eq!(inline_viewport_rows(&backend), 24);
    }
}

/// The terminal UI's implementation of the runtime's one terminal port
/// (`codewhale_runtime::host_terminal`). The composition root installs it for every host
/// this binary launches, so runtime code (the shell tools, the dispatcher)
/// reaches raw mode only through it and never links crossterm.
struct TuiHostTerminal;

impl codewhale_runtime::host_terminal::HostTerminal for TuiHostTerminal {
    fn suspend_raw_mode(&self) -> bool {
        let was_enabled = crossterm::terminal::is_raw_mode_enabled().unwrap_or(false);
        if was_enabled {
            let _ = disable_raw_mode();
        }
        was_enabled
    }

    fn resume_raw_mode(&self) {
        let _ = enable_raw_mode();
    }

    fn notify_model(&self, title: &str, body: Option<&str>) -> &'static str {
        crate::tui::notifications::notify_model(title, body)
    }

    fn set_terminal_focused(&self, focused: bool) {
        crate::tui::notifications::set_terminal_focused(focused);
    }

    fn apply_notification_settings(
        &self,
        config: &codewhale_config::notifications::NotificationsConfig,
    ) {
        let _ = crate::tui::notifications::apply_settings(config);
    }

    fn begin_turn_chrome(&self) {
        crate::tui::notifications::set_taskbar_progress_busy();
        crate::tui::notifications::start_title_animation("codewhale");
    }
}

/// Install the TUI as the process's terminal host. Idempotent: the first
/// install wins.
pub(crate) fn install_host_terminal() {
    let _ = codewhale_runtime::host_terminal::install(Box::new(TuiHostTerminal));
}

#[cfg(test)]
mod host_terminal_tests {
    use super::TuiHostTerminal;
    use crate::tui::notifications::{configured_method, install_configured_method};
    use codewhale_notify::DeliveryOutcome;
    use codewhale_runtime::host_terminal::HostTerminal;

    /// The `notify` tool reaches delivery only through the installed host:
    /// the TUI's host must hand the model's text to the delivery path that
    /// honors the installed method, so `method = "off"` stays silent.
    #[test]
    fn tui_host_routes_the_notify_tool_through_the_installed_method() {
        let _lock = crate::test_support::lock_test_env();
        let previous_method = configured_method();
        let config = |text: &str| -> codewhale_runtime::config::Config {
            toml::from_str(text).expect("notifications config should parse")
        };
        // Settings reach the host the way the composition root sends them;
        // `condition = "always"` so the attention policy (checked first)
        // lets the call reach the method check whatever the runner's focus.
        TuiHostTerminal.apply_notification_settings(
            &config("[notifications]\nmethod = \"off\"\ncondition = \"always\"\n")
                .notifications_config(),
        );

        let receipt = TuiHostTerminal.notify_model("done", None);

        TuiHostTerminal
            .apply_notification_settings(&config("[notifications]\n").notifications_config());
        install_configured_method(previous_method);
        assert_eq!(receipt, DeliveryOutcome::SuppressedByMethod.receipt());
    }
}
