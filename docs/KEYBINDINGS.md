# Keybindings

> 阅读简体中文版：[zh_hans/KEYBINDINGS.md](zh_hans/KEYBINDINGS.md)

This is the source-of-truth catalog of every keyboard shortcut the TUI recognizes. Bindings are grouped by **context** — the focus or modal state they fire in. A binding listed under "Composer" only takes effect when the composer is focused; one under "Transcript" only when the transcript has focus; and so on.

Global key chords are not yet user-configurable — tracked for a future release (#436, #437). Hotbar slot actions are configurable with `[[hotbar]]` and `/hotbar`; the Hotbar activation chord remains `Alt-1` through `Alt-8`.

## Global (any context)

| Chord                | Action                                                        |
|----------------------|---------------------------------------------------------------|
| `/help`, `F1` or `Ctrl-/` | Toggle the help overlay. `/help` is what the chrome advertises: it is the one route every terminal delivers, where `F1` is eaten by tmux and some emulators and `Ctrl-/` encoding varies |
| `F2`                 | Toggle the typed Settings editor                              |
| `F3`                 | Open the provider/model picker (same as `/provider`)          |
| `Ctrl-K`             | Open the command palette (slash-command finder)                |
| `Ctrl-C`             | Cancel current turn / dismiss modal / arm-then-confirm quit    |
| `Ctrl-B`             | Release the current shell wait so the turn can continue; foreground commands move into `/jobs`, and background tasks keep running. Use `/jobs` or `Bash` with `action: "wait"` to inspect output |
| `Ctrl-D`             | Quit (only when the composer is empty)                         |
| `Tab`                | When the composer is empty, cycle TUI mode: Plan → Work → Operate → Plan |
| `Shift+Tab`          | Cycle permission posture: Ask → Auto-Review → Full Access. Live regardless of composer contents or whether a turn is running (suppressed only while a modal other than Config is open) |
| `Ctrl-T`             | Cycle reasoning effort for the active model. Walks the same ladder as `/model` and `/effort` (catalog or documented route dialect). Always-thinking models omit `off`; Grok 4.6 includes `xhigh`. |
| `Ctrl-Shift-T`       | Toggle live transcript overlay (sticky-tail auto-scroll)                       |
| `Ctrl-R`             | Open the resume-session picker                                 |
| `Ctrl-L`             | Compact the conversation context (status line shows progress; no-op while a compaction is already running) |
| `Ctrl-O`             | Open the reasoning detail for the selected or current turn, regardless of composer contents |
| `Ctrl-Alt-O`         | Open the whole-turn Turn Inspector, regardless of composer contents |
| `Alt-V` / `Option-V` (macOS) | Open the details pager for the selected tool/sub-agent card; with nothing selected, the live inspector of the newest running shell, else the visible or most recent card; terminals that emit the legacy Option-V glyph are also handled |
| `Ctrl-Shift-E` / `Cmd-Shift-E` | Toggle the file tree in the workbar                   |
| `Ctrl-]` or `Ctrl-Tab` | Cycle the work dock: TODO → Agents → Jobs → Background. `Ctrl-]` is the portable chord — `Ctrl-Tab` only arrives under the kitty keyboard protocol |
| `Ctrl-Shift-Tab`     | Cycle the work dock backwards                                  |
| `Alt-G` / `Alt-Shift-G` | Scroll transcript to top / bottom when the composer is empty |
| `Alt-1`-`Alt-8`      | Dispatch Hotbar slots 1-8 when no modal or inline picker is open |
| `Alt-!` / `Alt-@` / `Alt-#` / `Alt-$` | Select the workbar panel: Tasks / Agents / Context / Files |
| `Ctrl-Alt-0`         | Toggle the workbar off / back to the bottom placement          |
| `Alt-L`              | Open the pager for the last message (composer empty)             |
| `Alt-P` / `Alt-A` / `Alt-Y` | Jump to Plan / Work, or request Full Access (`Alt-Y` is the legacy permission channel — Work + Full Access — not a separate mode; it honors a locked approval policy) |
| `Ctrl-X` (Activity workbar) | Cancel all running background shell jobs                  |
| `Esc`                | Close topmost modal · cancel slash menu · dismiss toast. During a compact that is serving an in-flight turn, Esc stops the turn (the compact is collateral). |

## Composer

Editing the message you're about to send.

| Chord                       | Action                                                  |
|-----------------------------|---------------------------------------------------------|
| `Enter`                     | Send when idle; queue while busy; with an empty composer, send the next queued follow-up now |
| `Shift-Enter` / `Alt-Enter` / `Ctrl-J` | Insert a newline without sending (idle or busy) |
| `Ctrl-Enter` / `Cmd-Enter`  | Send into the current turn; send normally when idle (when the terminal reports it distinctly) |
| `Ctrl-U`                    | Clear the whole draft (recoverable — see `Ctrl-Z`)      |
| `Ctrl-Z`                    | Restore the cleared draft (only while the composer is empty) |
| `Ctrl-W` / `Ctrl-Backspace` / `Alt-Backspace` | Delete previous word        |
| `Ctrl-A` / `Home`           | Move to start of input / start of line (readline convention) |
| `Ctrl-E` / `End`            | Move to end of input / end of line                      |
| `Ctrl-←` / `Alt-←`          | Move backward one word                                  |
| `Ctrl-→` / `Alt-→`          | Move forward one word                                   |
| `Shift-←` / `Shift-→`       | Extend the selection one grapheme at a time             |
| `Ctrl-Shift-←/→` / `Alt-Shift-←/→` | Extend the selection one word at a time          |
| `Shift-Home` / `Shift-End`  | Extend the selection to the start / end of the line     |
| `Ctrl-Shift-Home` / `Ctrl-Shift-End` | Extend the selection to the start / end of the draft |
| `Ctrl-Shift-A` / `Cmd-A`    | Select the whole draft (see note below)                 |
| `Ctrl-Shift-U`           | Run `/update install` from the keyboard: check for and install the latest Codewhale release without leaving the TUI. Managed installs (Homebrew/npm/cargo) keep their package-manager gate; when already current the updater's "Already up to date." result is shown and nothing changes |
| Mouse drag                  | Select composer text; click moves the cursor            |
| `Cmd-V` / `Ctrl-Shift-V`    | Terminal-local paste (arrives as bracketed paste when supported) |
| `Ctrl-V`                    | Direct graphical clipboard paste; rich composer content becomes Markdown |
| `Ctrl-Y`                    | Yank (paste) from kill buffer                           |
| `Ctrl-X`                    | Cut the selection. The text is deleted only when a native clipboard confirmed the copy; over SSH or tmux, where only the terminal clipboard (OSC 52) takes it and never confirms, the text is copied and kept |
| `↑` / `↓`                   | Cycle composer history (also selects popup/attachment items) |
| `Shift-↑` / `Shift-↓`       | Browse conversation history                              |
| `Ctrl-P` / `Ctrl-N`         | Navigate slash-command menu entries; `Ctrl-P` opens the file picker when the menu is empty |
| `Ctrl-G` / `Ctrl-S`         | Stash current draft (`/stash pop` restores it); never sends or queues |
| `Alt-R`                    | Search prompt history (Alt-R to exit)                  |
| `Tab`                       | Slash-command / `@`-mention completion (popup-aware)    |
| `Ctrl-Shift-O` / `F4`       | Open the composer draft in `$VISUAL` / `$EDITOR`; F4 works when the terminal cannot distinguish Ctrl-Shift-O from Ctrl-O |
| `! command`                 | Run a shell command through normal approval, sandbox, and output surfaces |

Direct composer paste preserves headings, lists, links, tables and code from
HTML clipboard content. Plain text stays literal, and configuration fields
always paste literal text. SSH without a forwarded graphical display uses the
terminal's text paste. Copying a whole answer preserves its original Markdown,
without terminal wrapping or decorative rails; the destination app decides how
to render it. Transcript drag selections copy the same way by default:
Markdown source, whole cells joined with blank lines (see the Transcript
section below), with `tui.selection_copy_markdown = false` restoring rendered
text. Empty or oversized HTML falls back to plain text.

Set `composer_multiline_mode = true` to swap the portable `Enter` and
`Shift-Enter` behaviors: `Enter` inserts a newline and `Shift-Enter` sends.
`Alt-Enter`, `Ctrl-J`, and supported `Ctrl-Enter` / `Cmd-Enter` behavior stays
unchanged.

### Selection semantics

Typing, pasting, `Backspace`, or `Delete` with an active selection replaces or
removes the selected text, like any GUI editor. Plain movement keys (arrows,
`Home`/`End`, word motions) collapse the selection. When a selection covers the
whole draft, deleting or typing over it stashes the outgoing text the same way
`Ctrl-U` does, so `Ctrl-Z` (on an empty composer) or `Alt-R` draft recovery can
bring it back.

Cursor movement and deletion are grapheme-aware: one `←`/`→` step or one
`Backspace` covers a full emoji ZWJ sequence, flag pair, or combining-mark
cluster — never half of one. CJK text moves and deletes per character as
expected.

**Why select-all is not `Ctrl-A`:** the composer follows the readline
convention, where `Ctrl-A` jumps to the start of the input (paired with
`Ctrl-E`). Select-all is `Ctrl-Shift-A` on every platform (like
`Ctrl-Shift-O` / `Ctrl-Shift-E`, it needs a terminal with an enhanced keyboard
protocol). On macOS terminals that forward the Command key (kitty, WezTerm,
iTerm2 with Command remapping), native `Cmd-A` also selects all; `Cmd-Shift-A`
works everywhere on macOS because Cmd normalizes to Ctrl.

### Hotbar

Hotbar trigger semantics are intentionally `Alt-1` through `Alt-8` only. On macOS keyboards this is the Option/Alt key plus the number row. Bare `1`-`8` is normal text input in the composer and remains owned by pickers, onboarding, approval prompts, and modal views.

Function keys and `Cmd-1` through `Cmd-8` are not the primary Hotbar chords. Many terminals reserve those keys for tabs, windows, or OS shortcuts, and some never forward them to terminal apps. If a terminal is configured to send `Alt-1` for a custom shortcut, the Hotbar receives the same reliable chord.

Since #3807 a missing `hotbar` key renders **no bar** — fresh configs show no Hotbar until you configure `[[hotbar]]` slots (an explicit `hotbar = []` also disables it). When configured, a bar looks like:

| Slot | Chord   | Default action     | Label     |
|------|---------|--------------------|-----------|
| 1    | `Alt-1` | `slash.workflow`   | `wf`      |
| 2    | `Alt-2` | `slash.goal`       | `goal`    |
| 3    | `Alt-3` | `slash.auto`       | `auto`    |
| 4    | `Alt-4` | `mode.plan`        | `plan`    |
| 5    | `Alt-5` | `mode.agent`       | `agent`   |
| 6    | `Alt-6` | `mode.operate`     | `operate` |
| 7    | `Alt-7` | `palette.open`     | `palette` |
| 8    | `Alt-8` | `sidebar.toggle`   | `side`    |

| Focus state | Hotbar behavior |
|-------------|-----------------|
| Composer empty, text, or whitespace | `Alt-1`-`Alt-8` dispatches a configured slot |
| Workbar focused, hidden, or auto | `Alt-1`-`Alt-8` still dispatches a configured slot |
| Slash menu or history search open | Blocked; the inline selector owns the key event |
| Command palette, help, approval, file picker, session picker, Fleet setup, or any modal stack | Blocked; the modal owns the key event |
| Onboarding | Blocked; onboarding owns numeric choices |

### `@` mentions

Type `@<partial>` to open the file mention popup. `↑`/`↓` cycle the entries, `Tab` or `Enter` accepts. `Esc` hides the popup. As of v0.8.10 (#441), completions are re-ranked by mention frecency — files you mention often + recently float to the top.

Two mentions resolve to curated git context instead of a path (v0.9.2, #4067):

| Mention | Inlines | Byte budget |
|---------|---------|-------------|
| `@git`  | `git status --short --branch` for the workspace | 8 KB |
| `@diff` | The working-tree diff, staged and unstaged (`git diff HEAD`) | 32 KB |

Both appear in the completion popup alongside paths, and both show up in the context inspector with their resolved size and, when the diff exceeds its budget, the truncation marker. When git is missing, the workspace is not a repository, or there is nothing to show, the turn carries an explicit `<git-unavailable>` note rather than silently contributing nothing. A path that merely starts with the token (`@diff.txt`, `@git/config`) stays a file mention.

### `#` quick-add (memory)

When `[memory] enabled = true`, typing `# foo` and pressing `Enter` appends `foo` as a timestamped bullet to your memory file *without* sending a turn. See `docs/MEMORY.md`.

## Transcript (when transcript has focus)

| Chord                | Action                                              |
|----------------------|-----------------------------------------------------|
| `↑` / `↓`            | Scroll one line (bare arrows scroll when the composer is empty) |
| `Alt-↑` / `Alt-↓`    | Scroll transcript (works with a draft in the composer)  |
| `PgUp` / `PgDn`      | Scroll one page                                    |
| `Alt-G` / `Alt-Shift-G` | Jump to top / bottom                            |
| `Ctrl-Home` / `Ctrl-End` | Jump to top / bottom (also works from the composer)  |
| `Alt-[` / `Alt-]`    | Jump between tool output blocks                     |
| `Esc Esc`            | Backtrack to a previous user message (`←`/`→` steps, `Enter` rewinds). Rewinds the conversation only; files are not changed, and `/undo` puts them back |
| `Esc`                | Return focus to composer                           |
| `Ctrl-Y`             | With an empty composer, copy the focused transcript cell |
| `Alt-V`              | Open raw detail for the focused tool or message    |
| `Ctrl-O`             | Open reasoning detail for the focused turn         |
| `Enter`              | Open the focused transcript block fullscreen       |
| Mouse drag           | Select transcript text in Codewhale                |
| `Ctrl-C`             | Copy an active Codewhale selection                 |
| `Cmd-click` (macOS) / `Ctrl-click` (Linux/Windows) | Open an OSC 8 link in a supporting terminal (terminal-owned) |

Bare printable keys are never transcript shortcuts. Under TUI-DOG-002 a
printable character always belongs to the composer — `j`, `k`, `g`, `G`, `y`,
`Y`, `r`, `v`, and `l` type themselves in every focus state, including
transcript selection. Earlier revisions of this table documented them as
navigation; they were never wired, and the help catalog has a test
(`transcript_navigation_catalog_does_not_advertise_bare_typing_keys`) that
keeps them out.

Releasing a drag selection, or pressing `Ctrl-C` with one active, copies the
intersected cells as Markdown source by default: each selected cell is
serialized the same way `Ctrl-Y` and `/copy` serialize it, partial
intersections round out to whole cells, cells join with blank lines, and the
toast names the copied cell count. Set `[tui] selection_copy_markdown = false`
to copy the rendered text as displayed instead.

For terminal-native selection, hold `Shift` while dragging (terminal support
varies), then use the terminal's own copy command: usually `Cmd-C` on macOS or
`Ctrl-Shift-C` on Linux/Windows. Those commands are handled by the local
terminal and are intentionally separate from Codewhale's `Ctrl-C` selection
binding. Over SSH, Codewhale sends copy requests back through OSC 52, or via
tmux's `load-buffer -w` path when running inside tmux.

## Pointer (mouse)

The mouse is a first-class input. Every row here is wired in
`crates/tui/src/tui/mouse_ui.rs`.

| Gesture              | Action                                              |
|----------------------|-----------------------------------------------------|
| Wheel up / down      | Scroll the transcript; over the composer, move the cursor a line at a time |
| Click                | Activate the row, chip, or tool block under the pointer |
| Drag                 | Select transcript text, or drag the scrollbar       |
| Right click          | Act on what is under the pointer: copy, paste, open details or a `file:line` in your editor, focus or stop an agent. Letters and digits shown in the menu run their row. A stop is confirmed in place: the first Enter or click arms it, and Enter or a click after the double-click window runs it. A letter only arms it, and the second click of a double-click is ignored |

Hover feedback follows the same rule: anything that responds to a click
highlights under the pointer. A row that highlights but does nothing, or acts
without highlighting first, is a bug worth reporting.

## Work bar (after `Alt-W` claims focus)

| Chord                | Action                                              |
|----------------------|-----------------------------------------------------|
| `↑` / `↓`            | Move selection                                     |
| `Home` / `End`       | Jump to the first / last row                       |
| `PageUp` / `PageDown`| Move selection a viewport at a time                |
| `Enter`              | Open the selected row's world (work inspector / agent details); on an already-open row, close it |
| `Esc`                | Close the open detail, else return focus to the composer |
| any printable key    | Return focus to the composer (typing always wins)  |

Mouse parity: clicking any work-bar row does what `Enter` does, in every
panel and placement. `Alt-!`/`Alt-@`/`Alt-#`/`Alt-$` switch panels.

## Slash-command palette (after `Ctrl-K` or typing `/`)

| Chord                          | Action                                              |
|--------------------------------|-----------------------------------------------------|
| `↑` / `↓` / `Ctrl+P` / `Ctrl+N`| Move selection                                     |
| `Enter` / `Tab`                | Run / complete the highlighted command             |
| `Esc`                          | Dismiss palette                                     |

## Session Picker (`Ctrl-R` or `/sessions`)

| Chord                | Action                                              |
|----------------------|-----------------------------------------------------|
| `↑` / `↓` / `j` / `k`| Move selection in the session list                 |
| `1`-`9`              | Open the visible session history at that list slot |
| `PgUp` / `PgDn`      | Page the history pane                              |
| `Enter`              | Resume the selected session                        |
| `/`                  | Search sessions                                    |
| `s`                  | Cycle sort order                                   |
| `a`                  | Toggle current-workspace scope vs all workspaces   |
| `e`                  | Archive / restore the selected session             |
| `x`                  | Show or hide archived sessions                     |
| `d`                  | Delete selected session after confirmation         |
| `Esc` / `q`          | Close the picker                                   |

Archive (`e`) is undestructive and needs no confirmation: the session stays on
disk and stays loadable, it just leaves the default list and stops being an
auto-resume candidate. Press `e` again to bring it back. Delete (`d`) is the
destructive one and keeps its confirmation.

## Question sheet (`request_user_input`)

When the model asks a question, a **bottom-anchored sheet** opens over the
transcript. The conversation stays visible above it. The sheet grows with the
question and options; if they still do not fit, it scrolls so the highlighted
option and any typed custom response stay on screen. Resize is not required to
reveal content.

| Chord | Action |
|-------|--------|
| `↑` / `↓` / `k` / `j` | Move the highlight |
| `1`–`9` | Quick-pick that option |
| `Enter` | Confirm the highlight (or enter custom-response typing) |
| `Space` | Toggle a multi-select option |
| `←` / `h` | Previous question — change that answer. Esc is not back; it cancels the whole request. |
| `Esc` | Cancel the whole question request |
| `PageUp` / `PageDown` | Scroll the transcript while keeping the sheet open |
| `Home` / `End` | Go to the start / latest part of the transcript |
| `Alt+↑` / `Alt+↓` | Scroll the transcript three lines |
| Mouse wheel above the sheet | Scroll the transcript; side work surfaces keep their own input |
| Mouse wheel over the sheet | Browse the question content without changing the answer or highlight |

While the question sheet is open, `Ctrl+↑` / `Ctrl+↓` and `Shift+↑` / `Shift+↓`
also scroll the transcript three lines. Plain arrows continue to move the option
highlight.

After wheel browsing, option navigation or typing brings the highlighted option
or custom-response text back into view. Transcript scroll keys work in both
option selection and custom-response typing.

**Custom response.** Every question includes an "Other" row. Enter it, type,
then `Enter` to submit or `Esc` to return to the options without sending.

## Approval modal (when a tool requests approval)

| Chord                | Action                                              |
|----------------------|-----------------------------------------------------|
| `y` / `Y`            | Approve once                                        |
| `a` / `A`            | Approve all (auto-approve subsequent calls)        |
| `n` / `N` / `Esc`    | Deny                                                |
| `e`                  | Edit the approved input before running              |

## Onboarding (first-run flow)

| Chord                | Action                                              |
|----------------------|-----------------------------------------------------|
| `Enter`              | Advance to next step (Welcome → Language → API/trust gates → setup checkpoint) |
| `Esc`                | Step back one screen                                |
| `1`–`9`              | Pick a language (Language step)                    |
| `0`–`9`              | Pick a provider (Provider step; SGLang, vLLM, and Ollama are keyless by default) |
| `y` / `Y`            | Trust the workspace (Trust step)                   |
| `n` / `N`            | Skip the trust prompt                              |

## v0.8.29 audit notes

- **`Shift+Enter` / `Alt+Enter` newlines now work in VSCode on Windows (#1359).** crossterm's `PushKeyboardEnhancementFlags` command unconditionally returns `Unsupported` on Windows (`is_ansi_code_supported() == false`), so the Kitty keyboard protocol escape was never written to the terminal. Without it, VSCode's xterm.js stays in legacy mode where `Shift+Enter` is indistinguishable from plain `Enter`, causing the composer to send the message instead of inserting a newline. The fix writes the push/pop escapes (`\x1b[>1u` / `\x1b[<1u`) directly on Windows, bypassing crossterm's capability gate. VSCode integrated terminal and Windows Terminal ≥1.17 both honour the Kitty keyboard protocol; terminals that do not understand the sequences silently discard them.

## v0.8.13 audit notes

- **Ctrl-S is stash, not history search.** Fixed in this revision — `Alt-R` is history search.
- **Phantom `Alt+Up` removed.** The "Edit last queued message" binding was listed in README but never existed in the key dispatch code.
- **Bare Up/Down arrows scroll transcript when composer empty (v0.8.13).** Previously the `should_scroll_with_arrows` gate was hardcoded to false, meaning bare arrows always navigated composer history even when the composer was empty. Users in virtual terminals (Ghostty, Codex, Kitty-protocol) were especially affected because they couldn't use Cmd+Up / Alt+Up shortcuts.
- **Configurable keymap (#436) and `tui.toml` (#437) remain deferred.** The `TuiPrefs` struct and loader exist in `settings.rs` but are not wired at startup. The named-binding registry that would let `~/.codewhale/tui.toml` override individual entries is still pending.
- **No other broken bindings found.** Every other chord listed above resolves to a live handler in `crates/tui/src/tui/ui.rs` (key-event dispatch) or `crates/tui/src/tui/app.rs` (mode + state transitions).

## Live Terminal dock

In `/workbar terminal`, click the dock or press `Alt+W` to focus it. Typing,
Enter, Tab and Ctrl+C reach the selected shell. Esc returns to the composer.
`Alt+Down` / `Alt+Up` switch sessions; `Ctrl+N` starts a fresh shell.
Mouse-wheel and PageUp/PageDown scroll output.

See the [Terminal dock guide](GUIDE.md#live-terminal-dock) for permission
boundaries, paste behavior and supported platforms.
