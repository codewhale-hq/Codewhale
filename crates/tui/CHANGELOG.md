# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Runcrate is a bundled provider:** `runcrate` reaches the OpenAI-compatible host at `https://api.runcrate.ai/v1` with `RUNCRATE_API_KEY`, and model ids come from its public `GET /v1/models` listing. Thanks @heydryft for the request and the vendor details. (#7085)
- **MCP connection status reaches the model:** one existing history snapshot now records permitted configured, connecting, failed, authorization-required, unknown and connected servers, with bounded inert names and no raw errors. Stale status and server guidance are withdrawn on contention, configuration/scope changes and restore; observation does not connect servers or promise callable tools. (#6866)
- **Bundled-skill policy:** `[skills.bundled]` separates automatic, authoritative and disabled installation from discovery, with per-name selection, disable and explicit-only controls. Aliases share policy; disabled shipped copies reveal user alternatives. Modified and managed files remain intact, and doctor plus `/skills inspect` show the effective policy. (#7066)
- **Configurable skill routing index:** `[skills] listing_max_description_chars` and `listing_budget_fraction` control the shared parent/child catalogue. Defaults stay 400 characters and 5% of the route window, within the existing total bounds. Load warnings, localized inspect notes and `/context` explain shortening; full skill loading stays available. (#7073)

- **Compact approvals keep their authority visible:** cards show three choices and default to Deny. An exact saved rule requires its scope preview on screen. Shortcuts for additional actions require opening Details; a hidden saved-rule choice never falls back to granting the session.
- **Quicker everyday navigation:** top-level help separates everyday and advanced commands, `doctor --all` expands path/backend details, and the launch screen labels how to resume recent work. Up selects the newest recent session. Whale cameos stay out of transcript text.
- **Session-window diagnostics:** the audit log records when a saved window starts a new journal root while retaining the previous branch, helping distinguish window replacement from lost durable history. `session.window.rerooted` now names a `cause` (`engine_projection`, `session_restore` or `unattributed`) and also fires when a journal with entries has no active path. (#7142)
- **Usage reporting counts models, tokens and tools:** when usage reporting is on, each session now also reports, per provider and model, its turns by outcome, input, output and prompt-cache tokens, and turn-time buckets, plus how many times each built-in tool ran and failed. A model is named only when it is in Codewhale's bundled public model catalog; custom, local and fine-tuned names are sent as `other`, and MCP and plugin tools as `mcp` or `other`. No prompts, outputs, tool arguments or paths are sent. Runs under CI or the GitHub Action report the `ci` session source. The first-run notice is shown once more to describe this. `DO_NOT_TRACK=1` now turns usage reporting off for that run, like `CODEWHALE_TELEMETRY=0`; `codewhale config set telemetry false` still turns it off for good. See `docs/TELEMETRY.md`.
- **`exec` and `doctor` follow the TUI onto local Ollama:** with no cloud key and no explicit route, `codewhale exec` uses a running local Ollama chat model and says so, and `codewhale doctor --probe-local` checks that route. Nothing is saved. (#7115)
- **Thinking starts at auto:** new configurations and the built-in default use `reasoning_effort = "auto"` instead of `max`; an effort you set is never changed.
- **DSH Git review scopes:** the extension host brokers bounded, read-only Git metadata to reviewed DSH plugins through a new `git_review_scope` action (extension-host policy v8).
- **Safer, more complete uninstall:** for a receipted release copy, `codewhale uninstall --yes` now also removes this install's exact three-line `# >>> codewhale PATH >>>` block from your shell profiles and the Codewhale-generated completion files at their documented paths (bash, zsh and fish, honouring `XDG_DATA_HOME` and `XDG_CONFIG_HOME`). It touches nothing else in a profile, leaves a symlinked or unreadable profile for you to edit, keeps the completion files while another Codewhale copy remains on PATH, and exits non-zero if that cleanup fails after the program is already gone. The `codewhale.net/install.sh` hint and the install guide now print the PATH line as that marked block, so there is something provably yours to remove; a bare `export PATH=…` line you added earlier is never touched. Data purge requires a receipted removal in the same confirmed run. Both Unix installers reject unsafe or managed destinations before creating them and still never edit shell profiles themselves. Uninstall instructions match the explicit-home purge scope in English and Chinese. Not yet observed on a real Windows host or an installed release artifact. (Refs #7091)
- **Nested and deletable threads (Runtime API):** a fork (including a backtrack fork) records `parent_thread_id`, `POST /v1/threads` accepts one naming an existing thread, and `GET /v1/threads/summary` returns it so clients can nest child conversations. `DELETE /v1/threads/{id}` removes an idle thread with its turns, items, events and goal, and its saved session unless another thread shares it or a terminal holds it open; a running thread answers 409.
- **Hunk actions in Review (Runtime API):** `POST /v1/git/stage`, `/v1/git/unstage` and `/v1/git/discard` take a `patch` holding hunks of the one path in `paths` and apply only those hunks through `git apply`, under the same `expect` preconditions as whole-path writes. A patch that no longer applies fails with git's message and writes nothing; renames, mode changes and binary patches are refused. The tasks tool can also `suggest` a task, shown as a Start/Hide card, without starting anything.
- **Live `/models` ceilings reach the route:** a provider that states per-model context and output limits in its own `/models` body now keeps them instead of falling back to the legacy 128K window. OpenAI `limit` blocks, flat `context_length` / `context_window` / `max_model_len` / `max_context_length` fields, `max_output_tokens` / `max_completion_tokens`, and a relay `metadata` block (`context_limit`, `context_budget`, `output_budget`) are read; `metadata.context_limit` outranks `context_budget` because the first is the model capability and the second the share one deployment accepts. Each field is parsed on its own, so an unreadable one costs only its own candidate instead of hiding the readable fields below it.
- **Change mode and permissions while the agent works:** Tab, Shift+Tab, `/mode`, the hotbar and `/config mode` no longer refuse while a turn is running. A response that is already streaming is not restarted; the change applies at the agent's next step in the same turn (before its next tool batch, at every approval gate, and before its next model request, which carries the new mode notice). The status line says so until it applies. A call still waiting for approval goes back to the model under the new posture, and a call you already approved runs only if the new posture is equal or broader. Model, thinking level and provider stay locked until the turn ends.
- **Steering interrupts thinking:** a steer (Ctrl+Enter, or Enter twice after queueing) now closes the in-flight model request at once, whether the model is waiting on headers, reasoning, writing text or emitting a tool call, and continues the same turn with your message. Text already shown is kept as interrupted output; unfinished reasoning and partial tool calls are discarded, never run.
- **Modern MCP client:** MCP servers now connect through the official MCP SDK (2.4) behind Codewhale's Rust broker by default (`mcp_backend = "host"`; `"rust"` stays available for one release). Servers that speak the 2026-07-28 revision are discovered with `server/discover`; other stdio and HTTP servers fall back to the legacy `initialize` handshake, and configured SSE servers keep theirs. Rust keeps process, network, credential, catalog, approval and cancellation authority.
- **DeepSeek Harness plugins:** reviewed DSH plugins (definition API pinned to DSH 0.2.1-rc.2 in a shared compatibility table) run in the Native extension host. Tool output is validated against its declared schema, unsupported options are refused instead of ignored, and plugin commands can submit a prompt with visible plugin attribution. Terminal command receipts are kept in thread snapshots without entering model history. Changing the compatibility table invalidates earlier trust. A newly enabled plugin's tools are offered on the first turn: the turn waits up to 10 seconds for the plugin host to finish loading instead of adding them only from the next turn. Plugin UI views stay disabled, and the import receipt says why: WebKit's network restrictions do not stop WebRTC, so plugin UI waits for an isolated renderer. A plugin tool can call `ctx.sessions.flush(exec.agent.session)` to have the Engine save the conversation so far before it continues (Runtime, terminal and `exec` sessions); it reports failure instead of claiming a save. Installing a plugin whose bytes changed after review now returns a 409 conflict instead of a server error. A plugin tool can ask you a question with `ctx.userQuestions.ask` and gets back the options you picked, your own text, or a skip; the question shows like any other Codewhale question in the terminal and through the Runtime API. A plugin tool can start a child agent with `ctx.subagents.startActivation` (the `spawn` provider): the child runs through Codewhale's own sub-agent manager, needs the same approval as the model's `agent` tool, can carry an extra persona section and a narrower tool list set in its `agent/created` handler, and returns its result to the plugin.
- **MCP catalogs stay fresh:** a server's tools, resources, prompts and skills are rediscovered on their own connection when an advertised `ttlMs` expires or the server sends a list-changed notice. A tool resolved against the old catalog is refused with "retry the call", a removed tool cannot run, and a failed refresh reconnects instead of serving stale entries. Catalogs are never shared across servers, credentials or reconnects.
- **MCP Skills extension:** servers that advertise the published Skills extension have their skills listed in `list_mcp_resources`. Listing never activates a skill, and reading a skill file is refused unless its size and sha256 digest match the listing. MCP skills do not yet join `/skills`.
- **Updates install themselves:** release-binary installs check for a new release every 6 hours (was 1), download and checksum-verify it in the background, and apply it the next time `codewhale` starts, never during a session. One line says `Updated to v… · What's new: … · Undo: codewhale update --rollback`. Until releases are signed, the download waits for your OK (one `[y/N]` question at the next start, or `/update install`); a release with a valid Ed25519 manifest signature applies on its own. `codewhale update --rollback` puts back the version the last update replaced and keeps the rolled-back version from returning automatically. Turn it down with `codewhale config set update.mode notify` (or `off`, or `CODEWHALE_UPDATE_MODE`); an organization's root-owned `policy.toml` overrides both. Offline and rate-limited checks retry about an hour later instead of on every launch. Package-manager and desktop-app copies only get their manager's command. Builds with the release key refuse a missing or invalid signature, and re-check the staged binary against the signed manifest at start. The checksum manifest now always comes from GitHub Releases; on Linux x64 the CNB mirror can still serve the binary, but only bytes that GitHub's manifest lists.
- **One view of every Codewhale on the machine:** `codewhale doctor` has an Installations section and `codewhale doctor --installs` (`--json`, `--brief`) lists every `codewhale`, `codew`, `codewhale-tui`, `deepseek` and `deepseek-tui` with its owner: install receipt, package manager (with its exact removal command), the desktop app, a backup, or not ours. Nothing found is ever run. `codewhale doctor --fix` adds one plan to tidy them: old backups move to `~/.codewhale/recovery/<stamp>/` (kept 30 days, then deleted by a later `--fix`), an unreceipted `codew` is brought in line with `codewhale`, and the directory gets an install receipt; each move prints its undo and `codewhale doctor --restore <stamp>` reverses the lot. Package-managed, desktop-app, symlinked and foreign files are never touched, and neither is user data. `codewhale uninstall` now lists other copies with the same classification.
- **Uninstall guidance on every install route:** `codewhale uninstall` previews receipt-owned release files and `--yes` removes only checked files, with the running command last. Both Unix installers and the Windows archive installer record command ownership; self-update renews existing receipts on both. On Windows the running `codewhale.exe` is renamed and deleted once the command exits. Package-managed copies are left alone, and the command prints their exact removal command: npm with its prefix, Cargo with its root, the Homebrew formula, `scoop uninstall codewhale`, `winget uninstall --id HunterBown.CodeWhale`, or the Windows setup's `Uninstall.exe`. Every other `codewhale`/`codew` still on PATH is listed. User data is kept unless you add `--purge-data`. With `--yes` it runs only after a receipted installation was removed in that same run, and then deletes only `~/.codewhale` (or a `CODEWHALE_HOME` named `.codewhale`) and `~/.deepseek/snapshots`, refusing symlinks, home ancestors and directories owned by another user. A copy from an older `curl | sh` installer that wrote no receipt is adopted on `--yes` when it sits in a known older curl-installer directory: the commands byte-identical to the running program get a receipt and are removed, anything else is preserved, and a symlink or hard link among them stops adoption before any receipt is written. Windows removal is compile-checked but not yet exercised on a Windows host. (Refs #7091)
- **GitHub Action reviews:** analysis can export a structured Engine review for a separate trusted publisher job. The publisher checks the current PR revision and line anchors, deduplicates its findings, and keeps uncertain or preexisting claims out of inline comments.
- **DingTalk bridge:** an optional internal-app Stream adapter reuses the same Engine, shared admission and approval helpers. Tenant/staff allowlists, durable acceptance before acknowledgement and retained uncertain deliveries protect recovery. Managed account linking is separate from this advanced self-hosted bridge.
- **Answerless output-limit recovery:** opt into `[reasoning_only] on_output_limit = true` to request an answer again after a length stop with no answer or tool call, including hidden reasoning and ordinary Fleet workers. Retries share `max_reprompts`, retain existing output and control limits, and record request-scoped nudge receipts without adding them to resumed conversation context. RLM keeps its existing stop protocol. Thanks [@M-Maciej](https://github.com/M-Maciej) for the long-proof Fleet report. (#6990)
- **OrcaRouter recommendation:** `/provider` and `/model` offer a shared partner recommendation with the DeepSeek V4.1 Flash 30% top-up-match link, public referral disclosure and localized offer details. F7 or a click opens the existing pager while retaining the current route.
- **Per-scope instruction budgets:** global and project standing instructions now have separate token budgets (100,000 estimated tokens each, capped at 25% of the usable context window), every trim, skip or refusal reaches the model and `/context report`, and `[instruction_budget] policy = "refuse"` stops a request before dispatch. Other tools' instruction files stay opt-in through `project_instruction_imports` or `claude_fallback`, and the table is user config only. Thanks [@7jrxt42BxFZo4iAnN4CX](https://github.com/7jrxt42BxFZo4iAnN4CX) for the design and implementation. (#6526, #7086)
- The TUI Terminal dock now also shows the model's shell commands for the
  session: each running or recent command is a read-only tab with its live
  output, beside the interactive shells. "No live terminals" appears only when
  there are neither.
- Preview: `dispatch --computer` starts an account cloud conversation that can
  also be opened in desktop. `--thread` continues or inspects it; `--stop`,
  `--approve` and `--deny` control its current task. Explicit `--key-stdin`
  installs provider access in the running computer's memory. Retries retain
  one operation key and the account/VM binding. Existing Agent Work dispatch
  remains available when no computer conversation is selected.
- A long draft or paste now grows the composer past its density row limit, up
  to half the screen (never less than that limit) while leaving room for the
  transcript, so the text can be read and edited before sending. While a
  slash, mention or history-search menu is open the composer keeps its usual
  height (#7049).

### Changed

- **Newer model rows in the offline catalog:** the bundled seed now carries `kimi-k3.1-pro` and `kimi-k3.1-pro-highspeed` (Moonshot), `MiniMax-M3.1-Flash` (MiniMax and its Anthropic-compatible endpoint) and `gpt-6.1-terra` (OpenAI), which Models.dev already listed and Codewhale did not pin. Live refreshes from Models.dev were already showing these online; the seed is what a first run, an offline start and a deep-offline install read, so it is now re-locked at 2026-10-30 and rendered. `seed render --check` matches and `defaults audit` reports no stale default.
- **Skills ownership (internal):** skill discovery, provenance, mutation and bundled skill content move into `codewhale-skills`. Native roots still use the existing host manager's selection and lifetime checks. (S20, #5316)

- **Model pricing ownership (internal):** hand-sourced price metadata and recorded-time rules move into `codewhale-models`; cost auditing and live catalog callers use the same owner. (S24p1, #5316)

- **`codewhale-error-taxonomy` crate (internal):** `error_taxonomy` moves out of `codewhale-tui` into a new crate; the `LlmError` envelope conversion stays in the TUI crate until the model-wire split. No behavior change. (N13, #5316)

- **`codewhale-tui-kit` crate (internal):** terminal rendering helpers (markdown, diff, text width, paste burst, OSC-8, streaming) move out of `codewhale-tui` into a new crate, and `syntect` is no longer a direct `codewhale-tui` dependency. No behavior change. (S52, #5316)

- **Terminal kit on design tokens 2.3.0:** the vendored `codewhale-ratatui` now follows design tokens 2.3.0: an `Accent` role carries the one blue for links, focus and selection, and `Primary` is the ink of filled actions (upstream codewhale-hq/codewhale-ratatui 528a9c4). The built-in TUI themes keep their colours; only surfaces drawn in the kit's own roles take the 2.3.0 values.
- **Fleet workers run the same Codewhale:** `codewhale fleet run` and Runtime API Fleet runs (`POST /v1/fleet/runs`) now start workers from the running `codewhale` binary instead of whichever `codewhale` is first on `PATH`, so they work with a minimal `PATH` and never pick up a stale copy. `CODEWHALE_FLEET_CODEWHALE_BINARY` still overrides it. (#7119)
- **Switching provider lands on its current model:** `/provider stepfun` now selects Step 5 Preview instead of the older Step 3.7 Flash; a model you saved for a provider is still kept. `/model` now lists the current provider's own models (bundled and live, newest first, up to 20) instead of only the selected one. Search also matches ids typed without separators (`step5`). While you search, Shift+P and Shift+F type their letters rather than pinning or adding to Fleet, so `Step-5-Preview` can be typed in full; Ctrl+P and Ctrl+F pin or add the highlighted row at any time.
- **First run without a key:** `/provider` now opens on the full provider list (DeepSeek and the other cloud providers) when only a keyless local runtime is set up, no longer shows a disabled Antigravity row that was never configured, and Shift+A toggles the view like `a`. A local Ollama model that is not loaded yet is loaded before the first turn, so the route uses its real context window (for example 32K for `qwen3:4b`) instead of an 8K fallback that refused the second message with "~5120 tokens usable"; the footer's context reading follows the same window. An installed Ollama model is listed in `/model` and `GET /v1/providers/ollama/models` once its own catalog has answered, even when another provider is active. (#7113, #7114, #7116)
- **Computer Use is a separate plugin, not bundled:** the Computer Use plugin is no longer vendored into the binary; it is an experimental macOS plugin with its own release. Install steps go on the Computer Use page once its signed release is published (the marketplace entry is not installable until then). Its consent tools load on first use, and decisions still require a human approval card.
- **First-party marketplace receipt:** the shipped catalog is pinned to marketplace revision `a5f1d210372ff48c9a278a0a39737956ac0f2d93`.
- **Upgrading over older installs:** `codewhale update` writes an install receipt for a copy installed before receipts, and moves a mismatched `codew` to the recovery folder and replaces it instead of refusing the update. The `curl | sh` and archive installers upgrade an earlier Codewhale in the install directory instead of refusing, keep the old files in the recovery folder rather than as `.bak` files beside the commands, and finish with `codewhale doctor --installs --brief`; a symlink or a non-Codewhale file at the destination is still refused. The Windows archive installer keeps the replaced files in the recovery folder too.
- **The desktop app's Engine is never self-updated:** `codewhale update` refuses to write inside `*.app/Contents/MacOS/` or the app's pinned Engine store, and the update notice for that copy says to use Codewhale > Check for Updates.
- **GitHub Action needs an explicit provider:** the Action's `provider` input no longer defaults to the `codewhale` account relay. Name your BYOK provider (or `codewhale` for a separately verified account machine key); a missing provider stops with `configuration_missing` before any download or model call. The repository's own (disabled) review workflow now pins CLI `v0.10.2` by default instead of `v0.10.0`.
- Local Ollama defaults now come from chat-capable tags, and `/provider ollama`
  refreshes the live catalog before switching. Exec and Runtime API defaults use
  the same discovery path. Fresh context observations come from loaded models'
  served windows; unloaded models retain an explicitly unverified fallback (#5848).
- The Runtime API's `turn.usage` receipts now carry each model call's priced
  cost on the route that served it and the prompt size the provider billed, so
  a client can follow a long turn while it runs. The mobile control page shows
  live token usage and a partial cost estimate in its header, keeps the
  estimate after the turn settles, starts fresh for the next turn, and counts a
  replayed receipt once (#7074). Thanks @gaord.
- `--full-access` is now the documented flag for Full Access (`--yolo` stays an
  alias), and `serve` accepts it after the subcommand: `codewhale serve --acp
  --yolo` used to fail with "unexpected argument". `serve --acp` also writes one
  stderr line naming the permission and sandbox it runs with, and the ACP
  permission option's Full Access hint names the working command. A configured
  `sandbox_mode = "danger-full-access"` still never widens Ask or Auto-Review
  (#6992, #4955). Thanks @eugenicum for the request.
- Ctrl+C on an idle composer that holds a draft now clears the draft instead
  of arming exit, and Ctrl+Z restores it. The "press Ctrl+C again" exit prompt
  arms only once the composer is empty, and Ctrl+C during a running turn still
  cancels the turn first. Clearing a draft with Ctrl+C or Esc also resets a
  pending exit prompt (#6237, #7084).
- While a background job runs, the git status probe now backs off after 30
  seconds without input or engine events, as it already did in an idle
  session, instead of starting about a dozen git processes every 2 seconds for
  as long as the job lives (#6728).
- An idle session wakes less often. While an approval, elevation or question
  prompt waits on you, while a background job runs, or while only a
  once-a-second label ticks, the event loop now relaxes to its 250 ms quiet
  poll instead of polling every 24 to 48 ms; animations and the prompt's
  timeout still wake it on their own deadlines. The terminal input thread
  polls every 200 ms instead of 50 ms after 5 seconds without input. Measured
  on macOS with release builds: an approval wait went from 29 to 12 timer
  wakeups per second, a background job from 32 to 16 and an idle composer
  from 25 to 11 (#6728).
- Saving a session no longer copies every message body: journal entries
  share their content, so the snapshot taken on each update copies ids and
  pointers. Resuming a session compares a leftover crash checkpoint against
  the saved document's metadata instead of parsing the whole document first,
  so `--resume` and attach read the document once (#6842).
- Multiline mode now has a send key on every terminal: with
  `composer_multiline_mode = true`, `Enter` inserts a newline and `Alt-Enter`
  sends. `Shift-Enter` also sends where the terminal reports it distinctly; on
  Linux terminals that report it as plain `Enter`, multiline mode previously
  had no way to send. `Ctrl-J` always inserts a newline. Thanks
  [@luestr](https://github.com/luestr) for the report. (#7093)

### Fixed

- **Notes stop swallowing a backslash:** a transcript note is machine-generated and names paths, but it went through the markdown renderer, where CommonMark §2.4 reads a backslash before ASCII punctuation as an escape. A note naming `C:\Users\asus\.codewhale\setup_state.json` therefore printed as `C:\Users\asus.codewhale\setup_state.json` — a directory that does not exist, in the one line a user would copy to find the real one. Notes are now escaped where they are rendered. Fenced blocks, and lines carrying inline code or a table row, are passed through untouched, because the renderer already copies those verbatim and doubling a backslash inside one would print it doubled.
- **Setup state survives a second instance:** two running Codewhale windows share one home, and both rewrite `setup_state.json` when the provider or model changes. The replacing rename is now retried briefly when the destination is held open for a read — Windows refuses it outright while that handle lives — instead of surfacing a write the caller could have made. The failure message also carries the underlying I/O error instead of only the outer context, so the next occurrence is diagnosable.
- **`doctor` stops flagging a user's own temp-named logs:** the `stale_setup_temp_files` check matched any file whose name starts with `.tmp`, so scratch files such as `.tmp-build-release.log` made a clean home report as half-applied. It now matches the shape a staged write leaves when it never lands (`.tmpXXXXXX`, no extension).

- **Approval cards are translated in every locale (#7071):** the "Do you want to proceed?" question, the Why and Stop explanations on destructive actions, the Approve, Edit plan and Cancel choices on workflow plans, the Save row, the details pager's title and the card's impact lines (one sentence per tool category plus the Reads, Writes, Target, Connected app, Agent type and Primary input rows) came from English literals, or from Simplified Chinese only. They now use the shared message catalogs, so all 15 packs carry them. The 14 non-English translations were written without native-speaker review. Still English outside Simplified Chinese: a workflow plan's own impact lines and detail labels, and the delegated-authority rows.
- **Local vLLM models keep their own reasoning ladder (#7124):** a vLLM or SGLang route treated every model as low/medium/high and folded low and medium onto high. Reviewed model-template facts (`crates/config/src/local_reasoning.json`) now give Qwen3.8, Qwen3.6-35B-A3B, Ornith, GLM-5.3-Flash and DeepSeek-V4-Flash-Vision-Exp the controls their template accepts, and the effort picker offers exactly those tiers, so low stays low and medium stays medium where the model has them. Unreviewed models are unchanged. Not yet observed against a real vLLM or SGLang server. Thanks [@gannp](https://github.com/gannp).
- **`/undo`, retry and fork-at-turn work on large and attachment-heavy conversations again (#7147):** a fork was measured by the shape it is stored in (a saved session repeats its conversation while it carries a journal, so the file can be twice the history) against the 8 MiB bound the reader judges by what the conversation carries, so a conversation the runtime kept opening and extending could still refuse `/undo` with "full history exceeds byte transport bound". The saved-history boundary also counted MCP-server notices, shell-completion handoffs, the deferred-toolbox snapshot and `<image>` attachment tags as prompts you typed, so a conversation that had received one refused with "Cannot identify an exact saved-history boundary". Both now use what the conversation carries. Thanks [@gaord](https://github.com/gaord).
- **Markdown shows what was written:** a paragraph nested under a list item keeps its indent (#7146), `~~~` fences render as code blocks and keep backtick lines inside them as code (#7156), asterisks that do not open emphasis (`5 * 60 * 60`, `*args`) stay literal (#7157), and a bare URL ends at full-width punctuation such as `。` and `）` instead of swallowing it (#7158). Thanks [@Lstarsky0](https://github.com/Lstarsky0).
- **Fleet changes no longer put a stray English status in the footer (#7152):** the engine's "Fleet roster refreshed for subsequent turns" arrived after the change's own receipt, stayed for seconds, took over the footer, pushed the Shift+Tab hint off the row and stayed English in every locale. Thanks [@Lstarsky0](https://github.com/Lstarsky0).
- **Simplified Chinese uses one form per term (#7150):** the pack addresses you as 你 throughout (the telemetry notice mixed 你 and 您), says 提供商 for provider and token for usage, and drops two stray spaces after full-width punctuation. Thanks [@luochen211](https://github.com/luochen211).
- **A turn can no longer wedge forever after a tool returns (#6184):** the heartbeat's tool phase carried no bound, on the assumption that per-tool timeouts covered it. They cover a *running* tool, not the engine's own work after a result comes back, so that window had no owner: the in-app watchdog deferred to the engine because the engine still looked like the owner, and the engine watchdog only reported. A real turn sat there for 56 minutes, produced no stall record and never cancelled. The tool phase now has a ceiling, and a watchdog report cancels the turn so it ends with a stated reason instead of hanging.
- **A restored conversation can be undone again:** a saved document keeps each turn's `<turn_meta>` envelope before the prompt while a thread rebuilt from its turn records keeps it after, and only the leading shape was stripped before comparing the two. Every record-side prompt was therefore longer than its transcript twin, so the exact-boundary search never matched and `/undo` on a resumed thread refused with "Cannot identify an exact saved-history boundary" instead of answering. Both shapes are now stripped, so the comparison sees the prompt text alone.
- **MCP servers that reject the modern probe now fall back to the legacy handshake:** a server answering `server/discover` with HTTP 400 and a JSON-RPC error (for example `-32600 Unsupported protocol version`) was treated as a hard failure instead of evidence that it speaks the older handshake, so such a server could not connect at all. Codewhale now reads a parsed JSON-RPC error on the probe as a transport refusal and negotiates the legacy `initialize` handshake, which is what a server that clearly answered in JSON-RPC means. Reproduced against `mcp.deepwiki.com` (three tools after the fix); Linear is unaffected.
- **MCP servers reject requests that carried two framing headers:** Codewhale sent every Streamable HTTP request with a duplicated `Accept` and `Content-Type`, because the extension host forwarded the SDK's headers and the Rust send path added them again. A server that validates those headers answered `415`, which Codewhale read as "this endpoint is not Streamable HTTP" and answered by switching to the deprecated SSE transport; servers that refuse `GET` then returned `405` and the connection failed with only `SSE error: Non-200 status code (405)`. Linear was unreachable this way. The send path now collapses the framing headers to one value each. Verified against Linear: discovery, tool listing (77 tools) and connection succeed.
- **One context window from planning to compaction (#7133):** the window a route resolves (from provider facts, your configuration or a fallback) and where it came from now travel together. Turn planning, automatic compaction, tool-output sizing, output caps, child agents, the context inspector and the model picker all use the same window. Previously a custom model with no context metadata was sent with a 128K fallback while compaction planned against 1M. A model switch or re-sync that keeps the same model and limits keeps that window. A restored Auto session uses the fallback window until its next turn picks a route. The original Windows report has not been reproduced.
- **Session saves no longer fail silently (#7095):** if the session store has stopped, a checkpoint or reset save now reports the failure in the status line instead of being dropped with only a log line.
- **Saves keep your recovery checkpoint (#7095):** a finished turn is saved even when a newer in-progress checkpoint arrives at the same time, and a `/clear` or `/new` reset no longer drops its own snapshot when saves queue up. If the reset snapshot cannot be built or written, the previous recovery checkpoint is kept and the save failure is shown in the transcript and status line instead. Retrying an interrupted session deletion finishes it without removing the shared checkpoint an older Codewhale may still be writing. These fixes do not yet reproduce the original report.
- **Context inspector shows the real compaction trigger (#7133):** `/context` uses the current route's effective threshold, including when it is zero. When automatic compaction is off, it says so, and the Work rail points to manual `/compact`.
- **`doctor --probe-local` changes nothing (#7115):** when a configured route rules out local Ollama, the check skips credential inspection. Otherwise it reads credentials without migrating the legacy secret store; `exec` keeps its normal resolver.
- **Blockquotes keep their rail when wrapped (#7144):** every wrapped row of a blockquote keeps the quote rail, and copying a selection still leaves the rail out. Thanks [@Lstarsky0](https://github.com/Lstarsky0).
- **Localized activity and file receipts:** grouped tool activity, file counts, patch outcomes and tool status use the shared 15-language catalog. Language changes refresh cached summaries; filenames, diffs and execution receipts keep their original data.
- **Automatic compaction stays off when disabled:** preflight and provider context-overflow recovery preserve the conversation and report the setting instead of forcing a summary. Manual compaction remains available, and a rejected recovery request retains its actual cause in both paths. (#7142)

- **VS Code packaging security:** use patched MSAL Node in the extension packager, addressing [GHSA-xcx6-jx8p-x3h5](https://github.com/AzureAD/microsoft-authentication-library-for-js/security/advisories/GHSA-xcx6-jx8p-x3h5).

- **Compaction preserves task state:** a summary that is empty, a placeholder or only leaked reasoning tags (as local models sometimes return) no longer replaces your history. The recent tail is chosen by token budget on whole tool exchanges, the older span and any earlier checkpoint are summarized, and the original history is saved before the swap. Context budgeting learns from provider-reported usage within a turn and reports repeated rapid refills instead of looping. (#7133)

- **Vim Normal mode works on every terminal:** with `composer_vim_mode = "vim"`, Normal-mode keys no longer land in the draft as text on Windows terminals or on any terminal that does not report bracketed paste (everything except ghostty, iTerm, WezTerm and Apple Terminal, tmux and VS Code's terminal included). A held key now reaches Normal mode, shifted keys such as `G` move instead of typing, and `k` history recall repaints at once. Keys that arrive in one burst are still taken as a paste. (#7126, #7130) Thanks [@Lstarsky0](https://github.com/Lstarsky0).
- **Remote-control start failures stay visible:** a refused or failed `/rc` start now leaves the controller in the Failed state with its reason, logs a redacted line, and keeps `/rc status` in the footer after the toast expires instead of reporting "off". (#7127, #7120) Thanks [@laozidabo](https://github.com/laozidabo).
- **`/undo` follows your language:** what `/undo` reports (the request it undid, each file it restored, what it left in place) now comes from the locale catalog in all 15 complete packs instead of English text in the code; English output is unchanged. If a localized report cannot be built, the restore still syncs with the Engine and an English report is shown. (#7121, part of #7071) Thanks [@Aboysky](https://github.com/Aboysky).
- **TSX files get TSX diagnostics:** the language server now opens `.tsx` documents as `typescriptreact`, so valid JSX is no longer reported as a TypeScript syntax error. (#7140) Thanks [@Ricardo-M-L](https://github.com/Ricardo-M-L).
- **Work stays with the session that was saved (#7118):** saving, forking, renaming and rewinding a conversation now publish only the Work (To-do) state that belongs to the session just saved, and a newer update stays pending instead of being overwritten. A rewind that applied but could not be saved says so and puts the retry or replacement prompt where you can copy it. Tasks missing from `/task list` while the tracker shows them are not explained by this change.
- **Interrupted session hint stays visible (#7095):** when a plain `codewhale` launch finds a session whose turn never finished, the "Run `codewhale --continue` to resume it" hint now appears in the TUI's status line instead of being printed to the terminal just before the TUI covered it.
- **Interrupted sessions are kept and listed (#7095):** crash checkpoints are no longer deleted after 24 hours. The resume picker shows a session that exists only as a checkpoint, labelled Interrupted, `--resume <id-prefix>` finds it, and `codewhale sessions` lists it with an `interrupted` mark. A newer checkpoint is promoted into its saved session only under the session lease, and only for the same workspace. Retained checkpoints are not yet capped in number.
- **Closing the terminal saves what was already queued (#7095):** on SIGINT, SIGTERM or SIGHUP, saves that were already queued are flushed for up to two seconds before exit (exit codes 130, 143 and 129); a second signal exits at once. Anything not yet queued when the signal arrives, such as an answer still streaming, is not saved, and SIGKILL, power loss or an antivirus kill depend on the periodic checkpoint. (#7122) Thanks [@laozidabo](https://github.com/laozidabo).
- **Remote control works again:** `codewhale rc` (`/rc`) can attach to the hosted control plane for web Chat again. The control plane had only accepted the retired 0.9.13 runtime identity, so every 0.10.x Engine was refused with `runner_runtime_identity_unavailable`; it now accepts the exact released runtime identity (commit, image and digest) and fails closed when they are not configured.
- **MCP compatibility fixes:** older servers that answer the discovery probe with an error or an unrelated result now connect through the legacy handshake instead of being refused after the probe. A server that rejects sign-in reports its real 401 and the `/mcp login` hint rather than the probe's "method not found". A stale OAuth token is refreshed once when the probe itself is rejected. A server whose command cannot start now says why (for example, command not found) instead of a generic broker refusal.
- **Image-heavy session recovery:** ordinary image reads use a 256 KiB delivery budget. Requests measure serialized bytes; over the local budget, older images are omitted from the outbound copy first, with receipts. An initial HTTP 413 is retried exactly once without image attachments, each replaced by an omission note. Saved images and executed tool actions remain intact.
- **Composer history honors `CODEWHALE_HOME`:** with `CODEWHALE_HOME` set, prompt history reads and writes `$CODEWHALE_HOME/composer_history.jsonl` instead of sharing `~/.codewhale` across isolated homes; an invalid `CODEWHALE_HOME` keeps no history rather than falling back to your real home. (#7069)
- **Slow tools are not reported missing:** a tool whose `--version` probe is still running at its 3-second deadline (for example the `cargo` rustup shim on a busy machine) now counts as installed instead of being cached as missing for the rest of the session.
- **Telegram recovery:** prompts and their initiating identity are durable before the polling cursor advances. Restart recovery reuses the original Engine operation, questions preserve human ownership, and uncertain final replies remain available through `/status` without an automatic resend.
- Long drafts now show a localized external-editor hint when the actual composer
  viewport clips their text. The hint uses the shared F4 binding and preserves
  queue, running-turn, history, and menu hints (#7049). Thanks @Lstarsky0.
- Archived context markdown now uses the active theme's palette when rendered
  in the transcript, preventing unreadable dark text on dark backgrounds (#7043).
  Thanks @Statter.
- Plan-to-Work handoff now names the seeded To-do item in a localized reminder
  to keep it on the list and complete it after the plan (#7068).
- Localized question footers, approval and plan-handoff chrome, tool waiting
  status, and /color receipts use the shared locale packs (#7071).
- Clearing a conversation with no goal no longer shows a misleading "Goal cleared."
  receipt. Clearing an existing or configured goal still reports it (#7089).
  Thanks @Lstarsky0.
- A fatal engine error no longer turns on offline queueing by itself. When the
  error came from a step that never starts a turn (a finished background
  shell, a goal continuation or a child completion that cannot resolve its
  route), every later message used to be queued behind it; it is now sent
  normally. An environment-only authentication failure still reopens provider
  setup and queues until you fix it (#6561).
- `codewhale --provider <name> resume <id>` now moves the saved session to the
  named provider, as `exec --resume` already did, so a session whose provider
  table was deleted can be reopened on a provider you still have. It takes
  that provider's default model unless `--model` is also given. Without
  `--provider` the resume still refuses rather than fall back, and the refusal,
  now also on the `codewhale resume` and `fork` mount path, names this command
  beside `codewhale sessions export <id>` (#6996). Thanks @luestr.
- The model's skills index now says when it shortens skill descriptions.
  When a description hits the 400-char row cap, or rows are cut to fit the
  context budget, the index ends with "descriptions shortened for N of M
  skills" and points to `load_skill` with `name="list"` for the full text;
  `/context report` and `/context json` show the same count on the skills entry.
  `/skills inspect` now
  flags a description by the same rule the index uses (#7073).
- A shell command that spans several lines (a heredoc, a loop, an `&&` chain
  split across lines) now shows those lines on its Live card while it runs and
  after it succeeds, within the card's usual line limit, instead of only the
  first line. Transcript shows the whole command. Single-line cards are
  unchanged (#6436, #7082).
- `codewhale review` no longer accepts leaked tool-call markup as a review: a
  pass whose reply is a DeepSeek DSML (or other) tool-call wrapper outside code
  now fails as incomplete, with no tally and nothing posted (#7072). Reviews no
  longer have fixed per-risk output caps (a Low-risk review used to get 4,000
  tokens, which a reasoning pass could spend entirely on thinking): every pass
  may use the route's own output limit, and the step, tool and money budgets
  still stop a review early as a named partial review rather than truncating
  it. The `review` tool bounds its reasoning level to leave room for the review,
  and an exhausted pass reports the allowance and reserve actually sent and
  says "cut off" when visible text came back (#7067).
- A delegated agent still marked running that has reported no progress for 10
  minutes no longer holds its parent turn open: the turn's stall watchdogs can
  recover again, and the stall record names the agent. A child's running time
  stays unlimited; only silence counts (#6184).
- Code blocks inside reasoning cells now take their syntax colours from the
  active theme instead of the terminal's detected light or dark mode, so a dark
  theme such as Shoreline in a light-profile terminal no longer paints
  near-black code on its dark reasoning surface. The pager and clipboard copies
  still follow terminal detection (#7043).
- `/balance` and the balance status chip work on the default DeepSeek route
  again. The balance request now goes to the DeepSeek API root instead of the
  `/beta` inference path, which failed with "Could not fetch DeepSeek account
  balance". A failed balance request now names its reason, such as HTTP 401
  Unauthorized (#7055).
- An `AGENTS.md` or global instruction file over 100 KiB is no longer
  skipped (which could let a sibling file load in its place): Codewhale loads
  its first 100 KiB, ends it with a marker giving the bytes omitted, and names
  the file in `/context report`. The shared 48 KiB instruction budget still
  applies after that, and the marker it leaves when it drops broader scopes
  now gives the bytes dropped and the total (#6526).
- `write` over an existing file now says when it kept that file's CRLF or LF
  line endings, so the agent no longer reports a line-ending conversion that
  did not happen (#7070).
- The To-do that a Plan-to-Work handoff seeds is now titled with the plan's
  first non-heading line outside code fences, without list or emphasis
  markdown, instead of the whole plan run together
  (#7068).
- Right-click "Open in editor" on a transcript cell now also offers a bare
  workspace file path with no line number, opening it at line 1 (#2342).
- Composer history is no longer rewritten from a partial read: when the
  history file cannot be read, new entries are not saved over it and a
  warning is logged. An empty declarative workflow is refused ("workflow must
  contain at least one node") instead of succeeding with no work (#6559).
- A `SKILL.md` that is not a regular file is reported as a skill load warning
  instead of being read, so a FIFO in a skills directory can no longer stall
  discovery (#6554).
- `app-server`: a turn whose `response_start` could not be written to stdio
  is now interrupted instead of running on with nothing able to stop it
  (#6558).
- Session saves no longer fail with "Fleet artifact path must stay within the
  workspace" when the Codewhale state root is a symlink or Windows junction to
  another volume: the state root is resolved before session-side state is
  opened. Thanks [@SparkofSpike](https://github.com/SparkofSpike). (#7097)

### Security

- **Pinned marketplace archives:** a Codewhale or Claude marketplace entry whose tarball source carries a `sha256` now installs only when the downloaded bytes match it; a mismatch, a malformed pin or a pin on a non-archive source refuses the install with nothing written. The pin also applies to direct installs as `https://…/x.tar.gz#sha256=<hex>`.
- **Sandboxed commands cannot plant git hooks or edit git config:** in workspace-write mode, `.git/hooks` and `.git/config` (and a linked worktree's shared `hooks` and `config`) are read-only to sandboxed commands, so a command cannot leave a hook or `core.hooksPath` that later runs outside the sandbox. The workspace's `.codewhale/` is protected even before it exists (on Linux it is created first, because bubblewrap can only protect existing paths). `git commit` and ordinary file writes are unaffected; `git config` writes and hook edits need an approved unsandboxed command.
- **Tool image validation:** an image returned by a tool must decode before it is sent, so truncated or corrupt files are omitted with a receipt even when they are already within the delivery budget.

### Maintenance

- **`codewhale-ratatui` is re-versioned to 0.1.3:** the vendored terminal kit gained the `Accent` role after 0.1.2 was published, and `codewhale-tui` uses it, so crates.io needs the new version before `codewhale-tui` can build from the registry. `crates/tui` and `pet/tui` pin 0.1.3; publishing it stays a release-time step.
- **`codewhale-plugins` crate:** plugin discovery, review, install and marketplace parsing move out of `codewhale-tui` into a new crate; MCP server config types move to `codewhale-mcp`, and the skill parse core and recommendations to `codewhale-skill-install`. Review and trust behavior is unchanged. (S19, #5316)
- **`codewhale-config` modules (internal):** the 8,100-line `lib.rs` is split into 23 modules (provider TOML, permissions, fleet, config resolution, env overrides, state dirs, config store and backup, and others); `codewhale_config::` paths are unchanged. No behavior change. (S57, #5316)
- **`codewhale-app-server` modules (internal):** `lib.rs` is carved into `http`, `stdio`, `turns`, `bridge` and `config_rpc` modules; `BoundedLines` and `runtime_compatibility_router` now live at `codewhale_app_server::stdio` and `::http`. No behavior change. (S49, #5316)
- **`codewhale-account-cli` crate (internal):** the cloud account, cloud computer and dispatch CLI commands move out of `codewhale-cli` into a new crate. No behavior change. (S50, #5316)
- **`runtime_threads` store and manager (internal):** `runtime_threads` is split into the durable store (`runtime_threads.rs`) and the thread manager (`runtime_threads/manager.rs`); callers import from `runtime_threads::manager`. No behavior change. (S35, #5316)
- **Crate split S24a (internal):** the workshop byte-budget slot moves to `codewhale-runtime`, `AutoRouteDecisionEvidence` to `codewhale-models`, and cost-status session persistence to a tui `usage_persistence` module, so billing no longer reaches the session manager or model routing. No behavior change. (#5316)
- **Provider pre-cuts B (internal):** client wire helpers, reasoning-effort mapping and the Ollama catalog parsers move into their own client modules, and test-only provider gates use the shared test-isolation predicate. No behavior change. (#5316)
- **`codewhale-dependencies`, `codewhale-snapshot`, `codewhale-lsp`, `codewhale-repl` crates:** external-tool resolution, workspace snapshots, the language-server client and the Python REPL runtime moved out of `codewhale-tui` into their own crates; `write_context_file` now lives in `codewhale-repl`. No behavior change. (#5316)
- **`codewhale-skill-install` crate:** the community-skill installer, package content digest and `SKILL.md` frontmatter reader move out of `codewhale-tui` into a new crate; the install SSRF guard and timeouts are unchanged in production builds. (#5316)
- **`codewhale-util` crate:** shared utilities, confined workspace file access, link and file-identity checks, bundle file readers, the `.git` name rule and the owner file lock move out of `codewhale-tui` into a new crate; no behavior change. (#5316)
- **Crate-split contract hygiene:** the `canonical_action` policy table now lives in `codewhale-execpolicy`, `required_action`, the file path-alias folding and the `apply_patch` preflight parser in `codewhale-tools`, and `DEFAULT_SPAWN_DEPTH`, `PermissionCeiling` and `ShellCeiling` in `codewhale_protocol::fleet`, so `codewhale-agent-contract` no longer depends on `codewhale-config` or `codewhale-workflow`. Approval and authority behavior is unchanged. (#5316)
- **Provider pre-cuts (internal):** schema sanitizing, prompt-turn classification, image-window capping, tool-projection warning names and the reasoning-effort tier moved into `codewhale-models`; stall records and route-label sanitizing moved into `codewhale-runtime`. No behavior change. (#5316)
- **Linux hardening tests:** the NoNewPrivs child matrices now respect a parent that already holds the irreversible flag, keeping the full assertions on unpinned hosts and checking the kernel ratchet on pinned hosts. Thanks [@asto18089](https://github.com/asto18089) for diagnosing the hardened-container failure. (#7021)
- Removed the internal config re-export facade and its model-constant wrapper;
  provider types, shared constants, credential helpers, and notification types
  now come directly from their shared owners. Configuration and saved settings
  keep their existing behavior.
- Removed superseded home-directory and Fleet-role re-export shims, unused
  tool-context sandbox/MCP fields, and the redundant core tool-parser facade.
  Consumers now call their existing owners directly; tool denial rules, approval
  posture, strict tool schemas, home resolution, and legacy saved-role migration
  retain their behavior. The cleanup removes three dead-code suppressions and
  keeps the engine as the sole owner of human approval attestations.
- Removed unused plan/journal accessors, an unused plan-status glyph helper,
  and a cancellation case that no execution path issued. Active mailbox and
  cancellation wiring stays in place; its stale unused-code suppressions are
  removed. No supported command or feature is retired by this cleanup.
- README translations carry refreshed source stamps after the 0.10.3 batch.
  Thanks [@Aboysky](https://github.com/Aboysky). (#7104)

### Contributors

- **[@7jrxt42BxFZo4iAnN4CX](https://github.com/7jrxt42BxFZo4iAnN4CX)** — per-scope instruction budgets with visible truncation ([#7086](https://github.com/codewhale-hq/Codewhale/pull/7086), [#6526](https://github.com/codewhale-hq/Codewhale/issues/6526)).
- **[@Lstarsky0](https://github.com/Lstarsky0)** — kept the `/diff` pager's place across a resize ([#7087](https://github.com/codewhale-hq/Codewhale/pull/7087)), said "Goal cleared." only when there was a goal ([#7089](https://github.com/codewhale-hq/Codewhale/pull/7089)), and stopped printing an unpriced session cost as `$0.0000` in `/status` ([#7090](https://github.com/codewhale-hq/Codewhale/pull/7090)), kept Normal-mode keys out of the draft on terminals without bracketed paste ([#7126](https://github.com/codewhale-hq/Codewhale/pull/7126)) and let shifted keys such as `G` reach Normal mode ([#7130](https://github.com/codewhale-hq/Codewhale/pull/7130)), kept the quote rail on every wrapped row of a blockquote ([#7144](https://github.com/codewhale-hq/Codewhale/pull/7144)), kept a nested list paragraph's indent ([#7146](https://github.com/codewhale-hq/Codewhale/pull/7146)), rendered `~~~` fences as code ([#7156](https://github.com/codewhale-hq/Codewhale/pull/7156)), left asterisks that do not open emphasis literal ([#7157](https://github.com/codewhale-hq/Codewhale/pull/7157)), ended bare URLs at full-width punctuation ([#7158](https://github.com/codewhale-hq/Codewhale/pull/7158)) and stopped the engine's fleet-roster status from taking over the footer ([#7152](https://github.com/codewhale-hq/Codewhale/pull/7152)).
- **[@gaord](https://github.com/gaord)** — a running turn's cost and context in the Runtime ([#7074](https://github.com/codewhale-hq/Codewhale/pull/7074)) and forks that measure the history they carry ([#7147](https://github.com/codewhale-hq/Codewhale/pull/7147)).
- **[@luochen211](https://github.com/luochen211)** — one form per term in the Simplified Chinese pack ([#7150](https://github.com/codewhale-hq/Codewhale/pull/7150)) and dropped blanket dead-code allows in fleet alerts and the pet worker ([#7149](https://github.com/codewhale-hq/Codewhale/pull/7149)).
- **[@gannp](https://github.com/gannp)** — reported vLLM effort values that ignored the local model template and fixed it with a reviewed template registry ([#7124](https://github.com/codewhale-hq/Codewhale/issues/7124), [#7125](https://github.com/codewhale-hq/Codewhale/pull/7125)).
- **[Sh1Zuku / @SparkofSpike](https://github.com/SparkofSpike)** — resolved a relocated state root before session-side state opens ([#7097](https://github.com/codewhale-hq/Codewhale/pull/7097)).
- **[@ddlw](https://github.com/ddlw)** — repaired main for the 0.10.3 batch: clippy `-D warnings` across protocol, runtime, util and tui (boxed review and MCP-add payloads, scoped dead-code expectations, cfg-gated platform test helpers), rustfmt, split extension-host imports, the provider-registry identity check, Windows-stable DSH receipt bytes, and the first-party marketplace receipt ([#7108](https://github.com/codewhale-hq/Codewhale/pull/7108), [#7109](https://github.com/codewhale-hq/Codewhale/pull/7109)).
- **[@laozidabo](https://github.com/laozidabo)** — localized `/diff` feedback ([#7101](https://github.com/codewhale-hq/Codewhale/pull/7101)), flushed queued saves before signal exit ([#7122](https://github.com/codewhale-hq/Codewhale/pull/7122)) and kept `/rc` start failures visible ([#7127](https://github.com/codewhale-hq/Codewhale/pull/7127)).
- **[Boyce / @Aboysky](https://github.com/Aboysky)** — refreshed the README translation source stamps ([#7104](https://github.com/codewhale-hq/Codewhale/pull/7104)) and localized `/undo` feedback ([#7121](https://github.com/codewhale-hq/Codewhale/pull/7121)).
- **[@Ricardo-M-L](https://github.com/Ricardo-M-L)** — opened `.tsx` documents with the `typescriptreact` language ID ([#7140](https://github.com/codewhale-hq/Codewhale/pull/7140)).
- **[@M-Maciej](https://github.com/M-Maciej)** — reported answerless output-limit stops in long Fleet proofs ([#6990](https://github.com/codewhale-hq/Codewhale/issues/6990)).
- **[@asto18089](https://github.com/asto18089)** — diagnosed the NoNewPrivs child-matrix failure on hardened containers ([#7021](https://github.com/codewhale-hq/Codewhale/issues/7021)).
- **[@Statter](https://github.com/Statter)** — reported black-on-black text on terminals that do not report their background ([#7043](https://github.com/codewhale-hq/Codewhale/issues/7043)).
- **[@eugenicum](https://github.com/eugenicum)** — asked for a visible Full Access flag for `serve --acp` ([#6992](https://github.com/codewhale-hq/Codewhale/issues/6992)).
- **[@luestr](https://github.com/luestr)** — reported the Linux multiline send key ([#7093](https://github.com/codewhale-hq/Codewhale/issues/7093)) and session export guidance ([#6996](https://github.com/codewhale-hq/Codewhale/issues/6996)).

## [0.10.2] - 2026-10-22

Codewhale v0.10.2 adds an animated pet view (`/pet on`), a live Terminal work
dock, per-chat composer colors (`/color`) and a one-time note on the first
start after an update that says what changed. It closes three session
data-loss paths and a `codewhale web` session leak; adds the default Runtime
daemon socket locations to the sandbox deny-list; holds a `curl` or `wget` that uploads a
credential; refuses shell commands containing invisible Unicode; keeps the
Linux sandbox working when a hidden credential directory is a symlink; and
reduces idle TUI redraw work. `/undo` takes back a whole
request, including long and multiline prompts. `exec --auto` explains its
Full Access default, honors an explicitly stricter sandbox, and saves
tool-enabled runs and their failure receipts. Local runtimes appear during
onboarding, `/diff` works outside a Git repository, DeepSeek's Responses route
no longer rejects replayed tool history, and contributor fixes improve token
accounting, relay model lists, localized replies and Windows launcher
guidance. Delegated work continues without model-invented turn or time caps,
and independent workflow steps can continue after another child fails.

### Latest fixes

- Linux sandboxed terminals deliver Ctrl-C to the foreground program without
  killing the bubblewrap wrapper. A program that handles SIGINT can keep running
  and accept more input. PTY jobs and persistent terminals retain their sandbox
  and process-tree cleanup; this path requires util-linux `setsid`.

- Full Engine integrations now point explicitly to `codewhale app-server --http`.
  Compatibility HTTP, stdio and socket turns require `allowed_tools: []` and
  reject interactive execution and goal starts before work begins, closing the approval and
  clarification dead end. ACP new/load rejects unsupported supplied MCP servers
  instead of silently ignoring them; ACP v1 MCP support remains partial. The
  Runtime SDK guide states its Fleet/event coverage and provides authenticated
  HTTP examples for the remaining client controls.

- `tokio` moves to 1.53.3, which retires `RUSTSEC-2026-0386`. The advisory
  covers `tokio::io::unix::AsyncFd`'s safe constructors accepting a file
  descriptor they do not own; the lock now sits in a range the advisory lists as
  patched, and Codewhale never calls `AsyncFd` at all.

- A signed-in ChatGPT account refreshes its model list at launch. The plan roster
  was only fetched at sign-in or on an explicit `models --update`, so an account
  that never re-authenticated kept reporting models it no longer offers and never
  heard about new ones; the launch sweep that refreshes the other catalog layers
  now refreshes a missing, stale or invalid roster too, at the cost of one
  freshness probe when it is current. A roster past its 24-hour limit also lists
  its last known models instead of none, and only a fresh fetch supplies the
  preferred default.

- Two sessions can share one folder again. The embedded TUI runtime mounted its
  delegated-coordination lock exclusively, so a second `codewhale` in a workspace
  that already had one running was refused with `another Codewhale process owns
  delegated coordination for <workspace>`; `codewhale resume <session B>` beside a
  running session hit the same wall. The exclusive holder is now only
  `codewhale serve`, which the architecture doc already reserved it for (`#7042`).

- A reasoning effort the route cannot honor is no longer sent to it. OpenAI wire
  requests resolve the session's effort through the row's declared ladder and
  clamp to the nearest rung upward, because `gpt-5.5-pro` declares only
  `["medium","high","xhigh"]` and was being handed `off` and `low`, while
  `gpt-5.5` and `gpt-5.3-codex` start at `none` and `gpt-5.6*` add `max`. An effort
  the row already declares is passed through untouched (`#6396`).

- ChatGPT-plan routes report their reasoning controls. The plan roster carries
  only ids and labels, so every `openai-codex` row advertised
  `reasoning_effort: unsupported` with no levels at all. Rows are now joined
  against the merged catalog for the fields the roster leaves blank, and
  `gpt-6-astra` reports `["low","medium","high","xhigh","max"]`. Context windows
  are deliberately not filled from that join: the plan snapshot's 272,000
  describes a different wire than the 1,050,000 the catalog lists.

- A boot that never reports back no longer claims to be connecting forever. The
  launch card's prediction for configured MCP servers now carries the moment it
  was made and retires after five minutes without a receipt, so a pass that dies
  before posting its finished event settles on the last known snapshot instead
  of leaving rows that never resolve (`#7035`).

- `python3 scripts/catalog_models_dev.py defaults audit` is the release step
  that staleness needs: it re-reads a models.dev payload and reports every
  provider whose shipped default a newer stable row in its own namespace has
  overtaken, failing the run on an unrecorded one and taking an explained hold
  from `DEFAULT_AUDIT_REASONS` otherwise. And the `anthropic` default moves to
  `claude-sonnet-5-5`: a forced `tool_choice` is now softened to `auto` for
  Opus 5.5 and Sonnet 5.5, which reject it, instead of being sent and refused.

- The model picker's browse views list models you can talk to. `Catalog`, `New`,
  `Coding`, `Cheap` and `LongContext` now skip image, embedding, video, speech,
  transcription, reranking and realtime rows — 445 of the 8,849 live catalog
  entries — instead of threading them between the chat models you were scanning
  for. `Configured` is untouched, so a route you already set up stays visible,
  and a search still reaches everything: typing an image model's id finds it.

- The model picker can be read one provider at a time. `Sort: Provider` now
  prints a provider header for each block instead of one flat run of names,
  and `⇧T` narrows the list to one provider at a time and clears on the next
  cycle; the title names the active filter and the footer advertises `⇧T`
  beside `⇧A`, so a shortened list is never unexplained. Choosing a provider sort
  also browses the whole roster rather than the rows the current view admitted,
  so one keystroke turns the Configured view into a full catalog grouped by
  vendor — which is what "sort by provider to find models" needs to mean.

- `openai-codex` defaults to `gpt-6.1-sol`. Its ChatGPT plan roster served
  seven ids and no bare `gpt-5.6`, which the descriptor defaulted to: a fresh
  roster redirected the choice, but a stale, expired or absent roster sent a
  model the account does not serve and the request failed. `codewhale models`
  also stops claiming "availability not verified" for a route that did use a
  fresh snapshot, and now reports the real snapshot layer, fetch time and row
  count. The offline seed check compares every descriptor default against the
  seed default and refuses to write when they disagree, and
  `seed lock --dry-run` reports providers whose default a newer upstream id
  has overtaken, plus locked rows the spec no longer references.
  `docs/CATALOG_REFRESH.md` states the limit plainly: nothing offline can
  detect that a vendor shipped a model.

- A persisted `/models` partition from an official endpoint no longer hides a
  newer catalog snapshot. `openai` listed a two-generation-old roster while
  the shipped snapshot already carried `gpt-6-astra`, `gpt-6-luna`, `gpt-6-sol`
  and `gpt-6.1-sol`, and refreshed `anthropic` and `deepseek` ids were
  suppressed the same way. A route that owns its live catalog (a relay, a
  private endpoint, a custom base URL) stays authoritative however old the
  fetch; an official endpoint's partition must be fresh to shadow the
  snapshot, and an unfresh one still contributes its own ids as an offline
  tail so a withdrawn model does not vanish by omission.

- One catalog row without a price no longer costs you every model in its
  provider. `models --update` reported `failed / cache_write_failed` with
  `model_count 0` for OrcaRouter while the endpoint answered 468 rows, because
  a row whose price was dropped still carried a live cost stamp and one such
  row rejected the whole refresh as `invalid_response`; the same all-or-nothing
  rule also discarded an entire cached scope on the next read. Rows are now
  judged one at a time, a refresh fails only when rows arrived and none
  survived, and a cached scope that loses every row to validation is still
  dropped rather than loaded. A failed refresh names its typed reason instead
  of `cache_write_failed` and still reports the rows you have left.

- A new OrcaRouter session with no saved wire model defaults to
  `deepseek/deepseek-v4.1-flash-1017`. Every OrcaRouter catalog row has
  `default_for_provider` false, so the descriptor was the only thing deciding
  the default and it named an older model. The `deepseek-v4-pro` and
  `deepseek-v4pro` shorthands still select `deepseek/deepseek-v4-pro` on this
  provider, so naming a pro model never silently lands on Flash.

- `POST /v1/threads` is replay-safe. A retry under the same `operation_key`
  returns the original thread — after a restart too — instead of minting a
  second one and orphaning the conversation; a changed request under the same
  key is refused with 409 rather than replacing or duplicating. The
  capability is advertised as `thread_create_idempotency`, and clients must
  require it.

- A sandboxed shell can read a tool's own auth config again. The credential
  deny-list still refuses those files to Codewhale's file tools, but it no
  longer refuses them to the programs that own them, so `gh`, `glab`, `aws`,
  `npm`, `cargo`, `docker`, `kubectl` and `git` can authenticate from a
  `read-only` posture. Key material, keychains, other agents' credential
  stores and Codewhale's own secret stores stay denied to subprocesses.

- The website and README link the supplied Codewhale × OrcaRouter offer
  graphic to its redemption page, with readable link text and referral disclosure.

- Development test builds use a boxed future at the shared Engine event-loop
  boundary, keeping its large concrete async type out of callers. One local
  warm-build comparison fell from 29m 31s to 2m 24s with the same compiler,
  profile and features; this is not a cold-build or cross-platform guarantee.

- Delegated workers no longer inherit model-selected turn, time or output
  caps, and the default 30-minute child deadline is removed. Launch receipts
  identify ignored legacy limits and show
  the limits that actually apply. The 75% child "wrap up" and 80% parent
  "stop exploring" prompts are removed, as is the instruction to give up after
  two failed tool calls. Explicit owner limits are no longer silently shortened
  to 100,000 turns or 24 hours. The stopship example also drops forced response
  counts and its two-call discovery limit. Children can use the final allowed
  turn for useful work; the host preserves partial results without reserving
  time or issuing an extra model request for a forced summary. Default child
  and workflow nesting caps are removed; explicit owner bounds still apply.
  Existing installations receive the updated delegation guidance while keeping
  customized or deleted skills intact (#7044).

- Workflows can continue independent steps after a child fails, with dependent
  steps recorded as skipped. The cumulative 1,000-child cutoff is removed;
  owners can still configure a total child limit. Recursive work no longer
  stops at 25 rounds, three replies without code or printed output, or an
  invented enclosing or subquery deadline. Code-mode scripts can make more
  than 50 admitted tool calls. Cancellation,
  permissions and inherited owner deadlines still apply. Long-running live
  children are no longer treated as abandoned merely because 35 minutes passed.

- Read-only workflow children receive their response schema in the first
  request; write-capable children keep the verification report contract.
  Unmetered workflow usage is shown as unknown instead of zero. Mode history
  follows changes between Plan and Operate, and `/goal resume` can restart an
  idle active goal without replacing its identity or clearing stall evidence.

- ChatGPT sign-in opens your regular browser and returns to Codewhale with
  credentials and a model list for the account you selected. The unsupported
  `originator` parameter that caused `invalid_authorize_request` is removed
  (#6925). PKCE, account binding and granted-scope validation remain in place.
  A real browser sign-in, account model list and model response were verified
  for this release candidate.

- Renaming a session in `/sessions` shows its title prompt once, preserving
  the validation and saved-title messages (#7054). Thanks @Lstarsky0.

- Deferred-tool admission measures schemas after the provider's wire conversion.
  Search results show the active toolbox, soft-limit overflow and readmission
  state. Per-request receipts join those wire measurements with the current
  prefix change and reported usage, including separate transparent retries.
  Missing cache usage remains unknown; these receipts do not add charges.

- The community website uses Next.js 16.4.1, which includes the upstream
  security fixes for cache deserialization, image optimization and Server Actions.

- Haiku 4.5 metadata preserves its image input and extended-thinking support,
  keeping review output reserves consistent with its provider capabilities.

- Clearing or replacing an OrcaRouter key cancels pending browser sign-in,
  including a code exchange already underway. A late callback can no longer
  restore the cleared key, and disconnecting the client does not interrupt
  credential cleanup.

- Cloud Runtime provider keys can stay in process memory. Session-only mode
  disables saved-key, environment and OAuth fallback, keeps keys bound to their
  exact endpoint, and advertises support for memory-only set and clear.
  Clearing affects subsequent turns; already running turns retain their key.
  In-process subagents remain available; external Fleet execution is refused
  in this mode because its processes do not share the session-key overlay.
  Supervised HTTP servers can adopt a validated inherited loopback listener,
  preserving supervisor ownership across Engine restarts.

- Opening a folder in the desktop Runtime no longer fails because a terminal
  session is already using it, including when attaching a second folder.
  Delegated workers still respect the folder's writer lock, including after
  a restart. If the Runtime starts without its own coordination lock, stop and
  restart its process after the other session exits to regain delegated writes.
  It cannot overwrite newer coordination records with the snapshot it opened earlier.

- Credentials in tool-call arguments are now redacted before a session is
  saved. A command the model wrote with a key in it (`curl -H
  'Authorization: Bearer …'`, `API_KEY=… ./run`, a `.env` write) was stored
  verbatim in the saved session and journal and replayed on resume; only tool
  output was redacted. `codewhale sessions scrub-secrets` now also cleans tool
  arguments in sessions already on disk. `[redaction] model_bound = false`
  still lets the model see exact file bytes for edits, but no longer turns off
  redaction of what is written to disk.

- Provider requests follow redirects only on the same host. A cross-host
  redirect or a downgrade from HTTPS to HTTP now stops the request, so an open
  redirect on a configured `base_url` can no longer collect an `x-api-key`
  header. The network guard also refuses IPv6 forms that reach private
  addresses: NAT64 (`64:ff9b::/96`, checked as its embedded IPv4 address),
  6to4, Teredo, site-local and IPv4-compatible addresses. An MCP server can no
  longer grow the resource-template cache without bound, and a prefix allow
  rule no longer matches a command that adds shell metacharacters after the
  allowed prefix.

- On Windows, a session in Russian or another language written in a
  non-Latin script no longer crashes the TUI with `start byte index N is not a
  char boundary` when Codewhale checks a shell command before running it
  (#7048). Thanks @8sero.

- Computer Use can be authorized in Full Access. Asking for an app grant was
  refused before it ran, so the most permissive mode was the one mode where
  Computer Use could never work. Ask and Full Access now show the approval
  card; Auto-Review and Never still refuse, because nobody is there to answer
  it. A grant still always needs your answer and is never approved
  automatically.

- A plugin state file larger than 1 MiB no longer makes every plugin fail to
  load. The limit is now 16 MiB, and the read stays bounded.

- The OrcaRouter model list now shows only models OrcaRouter serves over Chat
  Completions, the protocol Codewhale uses with it. Models offered only through
  another protocol, such as Gemini-only image models, no longer appear and fail
  when chosen. OrcaRouter price estimates now include its long-prompt price
  tiers instead of quoting every prompt at the base rate; a model whose tiers
  cannot be read is shown without a price.

- The first `/pet on` in a new session no longer shows `Source changed;
  review the current source before interacting`. The pet view acted on a frame
  older than the source it had just selected and sent a second, stale
  selection.

- Resuming a session after Codewhale was killed mid-turn no longer undoes a
  rename or archive made while it was down, for example from the desktop app
  or the Runtime API. The interrupted turn is still restored. A restored
  session also no longer shows Codewhale's internal note about sub-agents
  still running in the background as a message you typed.

- The `Web` tool now tells the model which JSONPath its `fields` option
  accepts (`$`, `.field`, `[index]`, `[*]`, `['field']`, for example
  `$.items[*].name`) and that recursive descent (`..`) and filters are not
  supported, so it stops sending projections that fail. Thanks @zhuowp
  (#7046).

- MCP servers are started, reconnected and stopped more carefully:
  - Editing your MCP config, or changing a plugin's MCP servers, restarts
    only the servers whose own entry changed. `/mcp reload` still restarts
    all of them.
  - `/mcp retry` no longer freezes the session while a slow server connects.
  - A tool call that loses its stdio server stops the dead process before
    reconnecting, so no second copy runs beside it. Sub-agents no longer
    start or supervise the parent's servers a second time. A Streamable HTTP
    server whose connection was cancelled reconnects.
  - `list_mcp_resources`, the resource-template list and `/mcp validate`
    (`/mcp doctor`) without a name report the servers already connected and
    the failures already recorded, instead of connecting every configured
    server.
  - `/mcp login` is no longer offered when the server's authorization server
    has no dynamic client registration and no `oauth.client_id` is set; the
    row says to set `bearer_token_env_var` or `oauth.client_id` instead. A
    token refresh no longer undoes a logout made in the meantime, and a token
    rotated by another process is picked up.
  - ACP sessions no longer start MCP servers they cannot use. With
    `[features] mcp = false`, MCP tools are neither listed nor run. The
    Runtime API's MCP management uses your `[network]` policy and OAuth
    callback settings. `codewhale exec --resume` starts MCP servers once
    instead of twice.

- `@git` and `@diff` mentions and the review Git-config check stop after 10
  seconds instead of freezing the TUI on a stalled `git`. They say "git
  command timed out" instead of "not a git repository", and one slow check no
  longer leaves later reviews asking you to upgrade Git. On Wayland,
  `wl-paste` and `wl-copy` get 2 seconds and are stopped if they hang, so a
  stuck clipboard no longer freezes the interface. Refs #6561.

- The provider lists in first-run setup and `/provider` line up their status
  column; before, each status started wherever the provider name ended.
  Thanks @Lstarsky0 (#6984).

- Runtime API: each `pending_approvals[]` entry, and `approval.required` on the
  authenticated thread surface, now carries the captured tool `input`. An
  omitted `input` means nothing was captured; an explicit `null` is a
  captured value.

- The WeChat bridge (`integrations/weixin-bridge`) now stays inside what
  Tencent's ClawBot allows. A bot may reply only within 24 hours of your last
  message and a handful of times per message, so the bridge sends at most 8
  replies per message you send (`WEIXIN_MAX_SENDS_PER_MESSAGE`). A longer
  answer ends with a "继续" prompt: reply 继续 for the rest. An answer that
  finishes after the window waits for your next message. An expired WeChat
  session (`errcode -14`) now pauses the bridge for an hour, as Tencent's own
  client does, instead of retrying forever. A rejected or rate-limited reply
  is held or backed off rather than resent every second. A reply that WeChat
  accepts without a message ID is reported by `/status` as possibly filtered.

- WeChat approvals are now answered with a short code: reply `1 k7m2fz` to
  allow or `2 k7m2fz` to deny. Codes are single-use, expire after 30 minutes,
  and `/status` issues fresh ones; `/allow` and `/deny` still work. Questions
  the agent asks now reach WeChat, and your next message answers them, by
  option number or in words. A typing indicator shows while the agent
  writes. Chinese command aliases (`/状态`, `/新对话`, `/停止`, …) and `/stop`
  are available.

- The WeChat bridge now accepts only the WeChat account that scanned the
  login QR code unless `WEIXIN_CHAT_ALLOWLIST` says otherwise. Other chats get
  one refusal that names their user ID, where they used to be dropped
  silently. A lock file stops two bridges from sharing one state directory,
  and `dev-up.sh` no longer allows every user by default.

- Skill and plugin downloads and skills registry sync now check the first
  request and every redirect. They refuse `localhost`, loopback, private,
  link-local and cloud-metadata IP addresses and hosts the network policy
  denies, refuse a redirect from HTTPS to HTTP, and stop after five
  redirects. Without a proxy, Codewhale resolves each host itself, refuses
  private answers and connects only to the address it checked. With
  `HTTPS_PROXY` or `HTTP_PROXY` set (lowercase works too), these downloads
  go through the proxy unless `NO_PROXY` matches the host; Codewhale then
  leaves host-name checks to the proxy and checks only IP addresses written
  in the URL. `ALL_PROXY` is not used. If your `[network]` default is deny,
  allow `codeload.github.com` as well as `github.com` for GitHub skill and
  plugin installs.

- `NO_PROXY` now accepts space-separated entries and `*` for MCP servers you
  configure and for skill and plugin downloads, an empty `HTTPS_PROXY` no
  longer hides `HTTP_PROXY`, and a `socks5://` or `socks4://` proxy is asked
  to resolve host names itself instead of Codewhale resolving them locally.

- Local state no longer follows links planted inside a workspace or session
  directory. The continual-harness lock file must not be a link, workspace
  notes are appended without following links below the workspace, session
  snapshots refuse a linked or multiply linked session file, and saving a
  project Fleet profile checks existing project identities without
  following a linked profile directory or file. New lane records and lane
  logs are readable only by their owner.

- The command splitter behind the destructive-command checks treats a
  backslash-escaped quote outside quotes as an ordinary character, as the
  shell does.

- Outside Full Access, background and headless runs now stop for review
  before a disk- or filesystem-destroying command placed inside a subshell
  or a command substitution, including one inside a `sh -c` script.

- Read-only agents and Fleet workers can now reach the network. In Full
  Access, `bash` with `read_only: true` allows outbound connections, so
  `curl`, `git ls-remote` and HTTPS clients work, while all local file writes
  stay blocked. `read_only` does not stop requests that change things on
  remote services. Ask and Auto-Review keep the mode offline, as do a
  `[network]` policy that defaults to deny and a role without the network
  grant. On macOS it reaches internet hosts over IPv4 and a local proxy set
  in `HTTP_PROXY`, `HTTPS_PROXY` or `ALL_PROXY`, but not IPv6 or any other
  port on the Mac's own addresses. On Linux it shares the host network
  namespace, including loopback and abstract unix sockets, like a networked
  workspace-write shell. A networked run is marked `sandbox_network: true`
  in its tool result, and `codewhale run sandbox run --policy read-only
  --network` now honors `--network`. Refs #7040, #5904.

- Plugin suggestions no longer treat Codewhale's own vocabulary as a reason
  to offer a plugin. A message about MCP servers stuck at connecting, or one
  that mentions agents, code, data, models, sessions or tools, no longer
  offers an unrelated vendor plugin; distinctive names, phrases and domains
  still match. The first-party marketplace check rejects the same words as
  manifest keywords. Refs #7036.

- When a route declares its own output limit in `[[custom_models]]`, on a
  named custom (`openai-compatible`) route or a built-in provider such as
  OpenRouter pointed at your own endpoint, `CODEWHALE_MAX_OUTPUT_TOKENS` can
  now raise a request up to that limit instead of stopping at the bundled
  limit for a model with the same name. Automatic requests keep their usual
  size. Without a declared limit the bundled limit still applies, and on a
  named custom route run receipts label it unverified rather than
  documented. Refs #6396.

- When a route publishes its own reasoning levels, Codewhale no longer
  refuses a level the route lists. It sends the level you chose, or the
  nearest level the route publishes, on Chat Completions routes and on
  named custom endpoints that use the Responses or Anthropic Messages
  wire, including `/cache warmup`. When a level still cannot be sent, the
  error names it and lists the levels the route publishes. Refs #6396.

- `codewhale resume` and `codewhale fork` now size a saved session the way
  it is handed over: compact JSON, with the conversation counted once
  instead of also as its derived message copy. A session that is over 8 MiB
  on disk only because of that copy, indentation, abandoned branches or
  finished shell operations opens again. A session whose current
  conversation and Work state alone exceed 8 MiB is still refused before
  anything is written: its file and archive are left unchanged and the
  refusal gives the size it would hand over. Reading the history through the
  Runtime API or the desktop app still applies the 8 MiB limit to the whole
  document. Refs #6955. Thanks @7jrxt42BxFZo4iAnN4CX.

- Saved sessions and crash checkpoints are now written as compact JSON
  instead of indented JSON. Files are smaller (16–27% on the two measured
  sessions from the report), and a session that was over the 8 MiB resume
  limit only because of indentation resumes again. Refs #6955. Thanks
  @7jrxt42BxFZo4iAnN4CX.

- A `codewhale resume` or `codewhale fork` that is refused before it
  reserves anything no longer tells you to recover it with an operation key.
  Refs #6954.

- The input, output and context limits your Codewhale account states for a
  model now apply to its requests. The desktop app and interactive sessions
  take them from the account's model listing, and `codewhale exec`,
  `codewhale review` and the workflow tool read that listing once before
  their first request; if it cannot be read, the run continues with built-in
  limits and says so on stderr.

- When a route states an output limit larger than half its context window,
  as a server that shares one window between input and output does,
  Codewhale compacts the conversation to keep a quarter of the window (at
  most 64K tokens, and never more than the route's request cap) free for the
  reply, instead of reserving the whole request cap, which on a small window
  left about a thousand tokens for the conversation. A request is still
  admitted while at least 128 tokens of output fit, so Codewhale's own
  instructions and tools can fill more of a small window when there is
  nothing to compact. Every request on such a route asks for no more output
  than the window has left after its input, up to the route's usual request
  cap, so a long prompt gets a shorter reply allowance instead of
  overflowing the window. When too little is left, nothing is sent: a turn
  compacts or stops with the usual context message, a compaction summary
  leaves out its oldest history until at least half the reserved allowance
  fits, and any other request fails with a context-length error. Other
  routes are unchanged.

- Once a session file passes 8 MiB, autosave moves all of its abandoned
  branch history to the session's journal archive, instead of keeping up to
  4,096 off-branch entries in the document. `/branch` can still restore
  archived entries. Refs #6842. Thanks @7jrxt42BxFZo4iAnN4CX.

- When a tool result is longer than the share of the context window one
  result may take, the model sees its head and tail and is told to read the
  rest with `retrieve_tool_result`. That tool is now offered whenever a
  result names a saved copy, and `codewhale exec --allowed-tools` no longer
  filters it out. A model on a small window, where most tool output is cut,
  is no longer told to recover output through a tool it does not have.

- A window that cannot save its queued messages because it holds no queue
  lease for the current session now logs both session ids, and failed
  session, checkpoint and queue writes log the raw OS error code. Refs
  #6978. Thanks @SergiuSSoluS.

- When the Linux sandbox cannot be set up (a bubblewrap mount error), the
  shell result says the command did not run, quotes the bubblewrap error and
  gives the ways forward, instead of the generic escalation hint. Nothing
  runs outside the sandbox, and each sandboxed call still fails while the
  cause remains. Refs #7007. Thanks @rafacouto.

- Auto-Review stops a turn after three guardian denials in a row, or ten of
  the turn's last fifty reviews, and the status line says why; the remaining
  calls in that batch are refused without another review. Timeouts and
  failed reviews do not count. Refs #6478.

- Runtime API: `POST /v1/threads/{id}/read` records the newest item a client
  has shown, and `GET /v1/threads/summary` now reports `last_read_item_id`
  and `unread` for each conversation. Marking a conversation read does not
  change `updated_at` or list order, and a marker never moves backwards.
  Refs #6164.

- On Linux, a Runtime owner record from before the last reboot no longer
  blocks recovery when its process id has been reused.

- The HarmonyOS guide now says the Linux ARM64 release runs inside the
  HarmonyOS PC Linux subsystem, an openEuler virtual machine, and lists
  Huawei's documented limits for it. It no longer describes a
  "glibc-compatible userspace", and native HarmonyOS stays on the source
  build. Refs #1072. Thanks @SakuraNeko.

- On native HarmonyOS, npm still installs the Linux build as a best effort.
  If it cannot start, Codewhale prints directions to use the Linux subsystem
  or build from source. The install script also warns that the build is
  untested there; npm 7 and later show that warning only with
  `--foreground-scripts`. Refs #1072.

- Fetching a page that builds its content with JavaScript no longer ends in
  a "no readable page content" error when the page has a title or
  description. `Web` fetch and `web.run` return those, marked `partial`,
  with a note that says why and what to try next. When a Playwright,
  Puppeteer, Chrome DevTools or Computer Use browser tool is connected, the
  note names it so the agent can render the page there. Refs #7038.

- Web search through a model provider's own search tool now checks each
  cited link before returning it, so Codewhale contacts the cited sites
  directly. The check uses the same protections and `[network]` policy as
  `Web` fetch, runs at most four checks at a time for five seconds each, and
  never requests a link that repeats your search query. Links that do not
  load are dropped and listed in the search receipt. The provider's written
  answer moves to a separate `unverified_synthesis` field, and the result
  says how many of the requested sources passed. When none pass, or when
  you ask for recent results only, the search falls back to your configured
  search provider. A cited site your `[network]` policy denies is dropped;
  one the policy would ask you about stays in the results, marked as not
  checked, until you allow its host. A backend that returns fewer results than
  asked is reported as a shortfall instead of an honored result cap, and a
  cited page the provider left untitled is named by its path or host rather
  than shown as a bare domain. Refs #7039.

- Searching for tools no longer unloads tools the agent is using. When the
  deferred toolbox is full, `tool_search` makes room from tools that were
  never called, then the least recently called, and keeps any tool called in
  the current or previous turn. Matches that do not fit, and tools it
  removes, come back with the exact `tool_search` input that loads them
  again. Refs #6969.

- Rendered markdown keeps nested list indentation, a to-do row keeps its
  status word when its title is long, and the approval footer's separators
  sit between its hints. Thanks @Lstarsky0 (#7030, #7033, #7034).

- Fleet tasks can choose an execution environment by declared data,
  capabilities, capacity, persistence and cost, with a preview explaining
  each decision (experimental). Configured Tailscale workers use a pinned
  node identity; placement and transport receipts preserve the selected
  environment and attempt. Declared resources do not reserve capacity.

- A runnable WhaleNet example uses real DeepSeek tasks, Python and a
  TypeScript MCP server to create a bounded analysis capability and reuse
  it in a fresh task after the first worker exits. The example records
  usage, process lifecycles, content hashes and a one-hour capability lease.

- ACP session history uses shared message recognition without depending on
  terminal rendering. Malformed archived-context text stays visible instead
  of panicking during history restoration.

- Linux and OpenHarmony credential storage can traverse directories that allow
  access without directory listing. The private credential directory still
  requires current-user ownership and protected permissions; symlinks remain
  refused. Filesystem-root errors now include the OS cause.

- Each file read and search keeps its own details when activity is grouped.
  Expanding the group and opening a selected item shows that call’s input and
  output instead of an arbitrary sibling’s.
- Local Skills setup creates its example in the project folder Codewhale
  discovers, and its setup and doctor output explain the workspace trust
  requirement. Skills help now points to the searchable Skills view and
  distinguishes it from management.

- Stopped shell jobs read as stopped instead of failed. Their detail view stays
  available after completion, a missing terminal exit code reads as unavailable,
  and delegated jobs identify their named agent. Refs #6931.
- `model list` respects the selected route's catalog instead of adding unrelated
  bundled models. `app/models` reads the captured Runtime's current catalog,
  including named custom routes, regional routes, limits and capability facts.
  Model listing remains available during a running task and reports an
  unavailable owner instead of returning a stale static list. Refs #6396.
- The model picker distinguishes stale catalogs and stale, unverified or
  unavailable pricing instead of calling them live or priced. Unknown
  reasoning support stays unknown, and unusable rates do not drive price sorting.
- Automatic Rust formatting no longer inherits Engine or provider credentials.
  The formatter keeps its normal toolchain and project configuration.
- If an exec session cannot be saved, text, JSON and streaming JSON now
  report the failure and exit unsuccessfully. The answer remains in the output,
  and the lifecycle receipt also records failure. This is not a retryable
  provider error: actions may already have completed.
- Unrelated or malformed requests to the local OAuth callback no longer abort
  sign-in. The listener keeps waiting within the original deadline; state and
  issuer checks still apply, and an authenticated provider denial still ends
  the attempt.
- Automatic goal turns no longer start with an obsolete objective when the
  goal is paused, completed, cleared or replaced while the turn is preparing.
- Updated certificate validation, MCP, and web dependencies to address
  dependency security advisories.
- Shell cards in the default view drop their `output:` and `time:` labels.
  Output rows follow without a label, and a command that ran for a second or
  more shows its duration after its status. Hidden output reads
  `… (+27 lines) · Alt+V:output`, translated, instead of `27 lines omitted`.
  A thought shorter than a second reads `Thought`. A notice that mentions
  "cycle" is no longer drawn as a bold divider.
- A shell card in the default view leads with the model's description, as
  written, and shows the command on its own `$` row underneath, in every state
  and at every width. A card without a description leads with its `$`
  command. A finished, successful command shows only `✓` (ASCII `Y`), without
  `done`; running, waiting for approval, queued, failed and "loaded, not run"
  keep their word, and a narrow terminal shortens the headline before the
  status. A failed card shows its whole command, up to six rows. The `▶ run`
  marker, the `note:` label and the `╭ │ ╰` rail shared by neighbouring tool
  cards are gone in this view; card rows are indented under their headline
  instead. `docs/ACCESSIBILITY.md` now says that every status carries a glyph
  and every state except this one also carries a word. With
  `show_tool_details = true` and `calm_mode = false` the labeled card is
  unchanged.
- ACP editors loading a saved session now receive its earlier messages and tool
  results instead of an empty conversation. Sessions saved by `codewhale exec`
  can also resume through ACP and the Runtime API. Saved reasoning and images
  are not replayed to the ACP editor.
- Replayed ACP tool results and Runtime receipts created from saved history
  now use the live receipt masking rules: private question answers become
  `User input submitted`, and configured credentials are redacted. This also
  covers history import, fork and rewind. Saved model history and the owner's
  session-export view keep their existing behavior.
- `serve --acp` refuses a second ACP owner in the same Codewhale home while
  still allowing an editor to attach to the desktop or HTTP Runtime. If that
  Runtime exits, the ACP guest exits with a recovery message instead of waiting
  for another line from the editor. An unsupported `approval_policy = "never"`
  is refused at startup, before it can fail every new session.
- A web fetch stopped at `max_bytes` returns the readable text received so far,
  marked as truncated. If the cap arrives before readable text, the error names
  the cap and the available recovery instead of making a second request and
  misdiagnosing the page as JavaScript-only. Complete pages retain their normal
  extraction rules.
- Windows state-file publication allows a longer retry window for transient
  sharing, locking and access-denied errors, reducing failures when another
  process briefly holds the destination. Permanent errors are still reported;
  the retry window is now about 1.27 seconds.
- `codewhale doctor --help` lists the doctor's actual options. Account and
  dispatch help no longer promise unavailable account-key storage or cloud
  execution. `codewhale review` rejects `--post` and `--repo` with local-diff
  options before loading credentials or contacting a model.
- `/model` reuses provider catalog data and route-readiness results instead of
  rebuilding them for every row. Its Auto hint no longer constructs the full
  model inventory. Thanks @SparkofSpike (#7026).
- `/feedback` marks shortened descriptions, including with `...` in ASCII-safe
  mode; `/statusline` shows a scroll rail when its list does not fit and then
  drops its padding rows, so a short terminal shows more items; and wide
  `/keys` layouts leave descriptions in the detail pane instead of repeating
  clipped fragments beside it. Thanks @Lstarsky0 (#7025, #7027, #7028, #7029).
- DeepInfra lists newer DeepSeek model IDs alongside its existing defaults.
  The defaults remain unchanged pending a live route check, and the new
  selections do not reserve bare model names that other providers accept.

- `/undo` takes back the latest request and its file changes without requiring
  trust mode, while refusing ambiguous snapshot matches. Trailing whitespace
  in a prompt label no longer prevents a match, and an empty first line in
  another request no longer blocks the whole conversation.
- `exec --auto` help and documentation now state its default Full Access:
  automatic tool approvals, no Codewhale shell sandbox, and trust mode on.
  Selecting any sandbox other than `danger-full-access`, by flag or config,
  keeps trust mode off. This includes `external-sandbox`, which relies on
  host-managed isolation and no longer grants file tools trust-mode access.
  Stream metadata reports whether the requested sandbox is actually enforced.
- Exec flags go before the prompt. A known long flag or `--flag=value` after
  the prompt now produces an error instead of silently becoming prompt text;
  use `--` for literal flag text or a prompt starting with `-`.
- Exec runs that offer tools save sessions in every output format. Exec and
  interactive sessions each keep their own retention allowance, so CI runs
  cannot archive interactive conversations. Live and saved receipts include
  retained terminal failures, and conversation rewinds discard outcomes from
  removed turns so they cannot be attributed to later requests.
- Onboarding lists Ollama, LM Studio, and vLLM even before a local runtime is
  detected, so they can be selected and configured. The `L` filter is available
  during onboarding after local-runtime detection.
- `/fork <id>` now shows the forked conversation. Before, the screen kept
  the previous conversation while the model already had the fork's history.
- With `/translate` on, each translated thinking block lands in its own
  cell. Before, two translations finishing out of order could swap them.
- Pasting text that contains an escape sequence written out, such as
  `print("\x1b[?25l")`, keeps it as pasted. Before, part of it was removed
  from the message box as if it were a stray mouse report.
- A question that appears while you are typing no longer takes an Enter you
  pressed for your message as its answer.
- `/new` and `/clear` start without the previous conversation's goal.
  Before, a paused goal followed you into the new conversation, where
  `/goal resume` picked it up again. The old session keeps its saved goal.
- After `/edit`, sending the revision with Ctrl+Enter replaces the earlier
  message as Enter does. Before, it was appended instead, and the next
  message then undid the wrong exchange.
- In `/resume`, pressing `d` and then scrolling with the mouse before
  answering `y` could delete the session you scrolled to instead of the one
  you chose. The prompt now ignores the mouse, and `y` deletes only the
  session `d` was pressed on.
- Typing a stop word (`stop`, `wait`, `pause` by default) while a turn is
  running cancels it at once. Before, Enter queued the word behind the turn
  it was meant to stop.
- Runtime API fixes:
  - `POST /v1/threads/{id}/undo` accepts `{"turn_id": "turn_…"}` to
    remove that user turn and everything after it. A client that edits a
    message can name it, instead of counting messages back, so a turn
    another client appends meanwhile can no longer make it edit the wrong
    message.
  - Goal events on a thread's event stream now follow the order the goal
    changed in. A set and a clear racing each other could leave the stream
    reporting an active goal after it was deleted.
  - `PUT /v1/sessions` for a thread with a queued or running turn now
    answers 409. Before, it waited for that turn to finish while blocking
    new turns, steering and compaction on every thread.
  - `PATCH /v1/threads/{id}` with a new `system_prompt` now takes effect
    on the next turn. Before, a thread whose engine was already loaded kept
    the old prompt. The change is refused while a turn is running.
  - `POST /v1/threads/{id}/retry` with a blank prompt now answers 400
    without first creating a fork.
  - Connecting a remote computer stops reading the probe response at
    64 KiB instead of buffering all of it first.
  - The computer event cursor can no longer move past an event that has
    not been published yet.
  - `POST /v1/stream` ends when the turn's own record fails to save,
    instead of sending keepalives forever.
- `web_search` can use DeepSeek's new Search API: set
  `[search] provider = "deepseek"` and give it your DeepSeek key through
  `[search] api_key` or `DEEPSEEK_API_KEY`. It returns raw results without
  a model round trip. DeepSeek bills $0.75 per 1,000 searches from
  November 1, 2026.
- In the desktop and web apps, a conversation stays compacted after
  Codewhale restarts or unloads it. Before, reloading replayed every earlier
  turn and then added the summary, so the next request carried everything
  compaction had removed. Each compacting turn now stores the history the
  model continued from, and forks, `/undo` and session exports start from
  it as well.
- When a provider stream fails before any content and Codewhale quietly
  resends the request, the tokens the failed attempt reported are now added
  to the session, goal and sub-agent totals instead of being merged away.
  The context meter still reflects only the request that succeeded.
- Esc, or a Runtime interrupt, now stops a `tool_call_before` hook that is
  still running: its process tree is killed and reaped, hooks queued behind
  it are skipped, and the turn stops at once. Before, the turn waited for the
  hook to finish, and the hook could still change files after the turn was
  reported as stopped.
- With a sub-agent open in the main pane, scrolling and redraws reuse its
  rendered transcript instead of re-rendering every message on every frame,
  and a finished sub-agent's transcript file is no longer reread from disk
  every 400 ms.
- While a slash command is in the composer, the TUI rereads your command
  folders and `config.toml` at most once a second instead of on every frame.
  A command file you add still shows up in completion within about a second.
- In a session long enough to fold its oldest transcript cells (past
  5,000), tool details, expanded tool runs, sub-agent cards and a streaming
  reply keep pointing at the right rows. Each fold had shifted them one row
  too far, so the inspector opened the previous cell's details.
- A sub-agent started with the parent's context that ends without writing
  anything no longer reports the parent's last message as its own result.
- When one reply carries several text blocks, as the Responses route sends
  for separate message items, the conversation now keeps all of them. Before,
  each new block replaced the previous one in the history sent back to the
  model, so it forgot what it had just said even though you saw it. Text a
  provider puts in a block's opening event is also shown now.
- `codewhale serve --insecure` and `codewhale app-server --insecure-no-auth`
  answer only requests addressed to `127.0.0.1`, `::1` or `localhost`.
  Without auth, a website could point its own domain at 127.0.0.1 (DNS
  rebinding) and drive the Runtime API from your browser; such requests now
  get `421 Misdirected Request`. Servers with auth are unchanged.

- Sandboxed commands can no longer reach Codewhale's Runtime daemon socket.
  On Linux with bubblewrap, and on macOS with sandbox network access on, a
  command the agent ran (a test script or a postinstall hook, say) could read
  the owner receipt and attach to the socket that `codewhale serve`, the
  desktop app, the VS Code extension or an ACP session holds. From there it
  could change the sandbox mode, the approval policy or a provider's base URL
  in your config, or start an unsandboxed turn. The daemon's default run
  directories are now on the sandbox's default deny-list, and on macOS no
  socket under a denied path can be connected to. Turning the default
  deny-list off (`sandbox_read_denylist_defaults = false`) also turns this
  off.
- `codewhale web` no longer gives its request proof to whoever holds the
  session cookie. The cookie is scoped to the host, not the port, so a
  listener on another `127.0.0.1` port could receive it, and `GET /` then
  returned the proof that unlocks the whole Runtime API. The proof now stays
  in the tab that opened the session, which keeps working across reloads
  when the browser allows session storage; a new tab opened by typing the
  address needs a fresh `codewhale web`.
- The `codewhale web` approval card shows the request summary, Codewhale's
  reason for asking (such as a sandbox escalation) and every argument of the
  call in full, with the model's own description below, labeled as such.
  Before, when the model had written anything, the card showed only that
  text and never the command, so a prompt-injected model could describe a
  harmful command as a harmless one.
- `/settings` choice lists that don't fit, such as Language and Theme, show
  a scroll rail. Thanks @Lstarsky0 (#7024).
- During an `/mcp` search that finds nothing, the status line says "No
  matches" like the list does, instead of "Nothing on this tab". Thanks
  @Lstarsky0 (#7023).
- Read and list cards show a path inside the workspace relative to it
  ("Reading src/lib.rs") instead of the full absolute path the model sent.
- `/statusline` shows its last two items, Workspace and Git branch, without
  scrolling. The picker had not counted its padding rows. Thanks @Lstarsky0
  (#7022).
- A shell or gate command that the safety check will refuse, such as one
  spanning several lines, is refused at once instead of first asking you to
  approve it.
- Diffs in the TUI number each line only on its own side: a removed line
  shows its old line number and an added line its new one. Unchanged
  context lines keep their indentation instead of starting at the left
  edge.
- A shell card shows a multiline command one line per row instead of running
  the lines together.
- A shell card whose command is waiting on your approval now says
  "waiting for approval" with a still mark, instead of "running (Ns)" with
  a spinner. A command queued behind it in the same step says "queued".
  Elapsed time starts when a command actually begins to run.
- `/color` gives a chat's composer box its own color. Bare `/color` cycles
  through red, orange, amber, green, cyan, blue, purple and pink; `/color
  <name>` picks one and `/color off` returns to the theme. The color is
  saved with that session and survives resume; other chats and the theme are
  untouched. It shows while you are typing, so the focus signal stays.
- Importing a session container or starting a new chat from the launch
  screen no longer carries the previous chat's `/title` onto the new one.
- Shell cards in the TUI:
  - A tool card that finished inside a turn that is still running opens its
    raw detail (tool id, input, output) with Alt-V or a click, instead of a
    rendered message view.
  - The work dock no longer shows a ✓ for a background job that was killed
    or timed out. Killed jobs get a neutral mark; timed-out jobs get the
    failure mark.
  - The keybinding docs now say what Alt-V opens with nothing selected, and
    that clicking a card keeps Alt-V on it while newer cards arrive.
- The TUI uses less CPU while it waits. With an approval, elevation or
  question prompt open, or with only background shells still running after a
  turn, the status spinner and elapsed-time labels refresh once a second
  instead of five times a second.
- On Windows, Codewhale processes that start at the same moment no longer
  lose the built-in plugins. The rename that publishes the built-in plugin
  snapshot now retries a brief sharing or access refusal, as the other atomic
  writers already did, instead of starting without built-in plugins.
- Six fixes from @asto18089, several ported from the Pinvou/CodeWhale fork
  with @zhuowp:
  - Claude 5 models and Haiku 5.5 no longer receive `temperature`/`top_p` on
    routes without exact catalog facts (custom or proxy base URLs, dated
    snapshot ids, Messages-wire gateways), which those models reject with a
    400 (#7014).
  - Kimi keeps a tool whose schema has an empty `items`: the empty node is
    dropped, as Moonshot's schema rules allow, instead of excluding the whole
    tool (#7015).
  - In a session that already has a stamped compaction checkpoint, a user turn
    that pastes a legacy summary header stays an ordinary turn: it anchors the
    round, is kept verbatim and no longer counts as a duplicated summary
    (#7017).
  - Turn-complete events carry their own `turn_id`, `submission_id` and
    `compaction_id`. A send whose route fails to install, and a manual
    compaction whose route is rejected, now end with a failed terminal event
    instead of only an error, so waiting hosts no longer hang (#7018).
  - Anthropic `redacted_thinking` blocks are captured and replayed byte-exact
    on native Anthropic routes, so a tool loop after a redacted block no
    longer fails with the same 400 on every retry. Other hosts withhold the
    block (#7019).
  - A named `[providers.<name>]` table with `wire = "responses"` or
    `"anthropic"` now uses that wire on every turn and across model switches,
    not only for the first request. The app-server's legacy
    `[providers.custom]` table now refuses a non-Chat wire with
    `provider_wire_format_unsupported` instead of forwarding a Chat body
    (#7020; Refs #6997).
- A background shell job that finishes while the agent is still working now
  reaches the model before its next step, instead of only when the turn ends.
  In `codewhale exec`, which stops at the end of the turn, that output used to
  be lost. The background notice now says that a job still running when the
  turn ends is stopped unseen in a headless run, so the model knows to wait
  explicitly when its answer depends on it. Sub-agents no longer pick up the
  parent's finished jobs, and the parent no longer consumes a sub-agent's.
- `codewhale review --base <ref>` gives the reviewer the `base..HEAD` commit
  messages as the author's stated intent, in a field of their own that it
  checks against the diff and reports on when the two disagree. They are
  treated as claims, not instructions, and capped at 8,000 characters.
- `codewhale --resume <id>` after the TUI was killed mid-turn keeps the
  interrupted turn. Before, the resume stamped the older saved document as
  newer than the crash checkpoint, and the checkpoint, the only copy of
  that turn, was then discarded as stale. `--continue` was not affected.
- Three session data-loss paths are closed:
  - `/load` of an older exported copy of a session no longer replaces the
    newer saved conversation with the same id. It refuses and points to
    `/resume`. Before, the next autosave deleted every turn made after the
    export.
  - A journal archive append that was cut short (disk full, or a kill)
    is trimmed back to its last complete line before the next append.
    Before, the next save glued new entries onto the torn line. That made
    the whole archive unreadable, and the document had already dropped
    those entries.
  - Launching with a legacy crash checkpoint keeps it when migrating it to
    the per-session format fails, instead of deleting it.
- Sub-agents stay within a turn's tool allow-list. `exec --allowed-tools`, a
  Runtime API `allowed_tools`, and a command's `allowed-tools:` frontmatter
  narrowed only the parent. A sub-agent it started got the full tool
  surface, including `bash`, `write` and `edit`.
- A `workflow` plan written with `nodes` (the declarative shape in
  `docs/WORKFLOW_AUTHORING.md`) is now judged for approval from its compiled
  plan, the same one the run uses. The old reader missed fields nested inside
  `agent` and `branch` nodes. A plan with write children could then start
  with no approval card, the card could say "Read-only plan", and the
  receipt recorded the run as approved.
- The other whole-file writers now refuse to replace an existing file that
  is unread or stale, as `write` does: `apply_patch` with `replace`, and the
  `write_file` and `File action=write` paths that older prompts still call.
  A successful patch counts as a read, so patch-then-write still works.
  `fim_edit` honors the read deny-list. It also refuses to write when the
  file changed while its completion was running, which used to overwrite
  your edit.
- `grep_files` (also `File search_content`) aimed at a single file, `rlm`
  `open` with `file_path`, and the cache-maximal working set now honor the
  read deny-list. Each read files such as `.env` past the check and returned
  their lines to the model. A deny-listed working-set file is skipped with a
  note.
- A `tool_call_before` hook that answers `ask` now always shows the approval
  prompt, as `docs/HOOKS.md` promises. Before, an earlier "approve for
  session" or a conversation grant for the same command answered it
  silently, so a hook could not re-gate a command the person had already
  approved once.
- `validate_data`, `review` and `verify` honor the read deny-list, as `read`
  does. They read files such as a workspace `.env`, or in trust mode
  `~/.aws/credentials`, without the check. `validate_data`'s TOML parse
  error could quote a line of such a file back to the model, and `review`
  and `verify` sent the whole file to the provider.
- Commands that start outside the shell tool now get the same sandbox as
  shell commands:
  - `terminal_*` PTY sessions used a default sandbox with no credential
    deny-list, so on macOS they could read `~/.ssh` or `~/.aws` while `bash`
    could not. On Linux they skipped bubblewrap, so workspace-write refused
    them. They now use the session's shell sandbox, and they refuse to start
    when commands run in an external sandbox backend.
  - Runtime API thread jobs (`POST /v1/threads/{id}/jobs`) started before the
    thread's first turn ran without bubblewrap and without the deny-list.
    They now get both, and they are refused when an external sandbox backend
    is configured, since a job starts a local process.
  - `codewhale run sandbox run` ignored `prefer_bwrap` and the deny-list.
    When no OS sandbox could enforce the policy, it ran the command
    unconfined. It now refuses.
  - On Windows, `bash` with `interactive: true` skipped the read-only check
    that every other command path applies. It is now refused like the
    others.
  - A sub-agent's posture line could say the local OS sandbox applied when
    its shell did not use one. It now reports what that shell actually
    uses.
- `write` refuses to replace an existing file that has not been read in this
  session, or that changed after it was read. The File `edit` action already
  made this check. Before, a whole-file write could overwrite contents the
  model never saw, or a change you or another agent made after the model's
  last read. Creating a new file is unchanged, and a file Codewhale wrote
  itself counts as read. The refusal names the recovery: read the file, then
  write again.
- `codewhale exec` warns again when the sandbox posture asks for confinement
  but no OS sandbox can enforce it, for example on a Linux host or container
  where bubblewrap cannot start. With no external sandbox backend configured,
  which is the default, the check read the absent backend as present, so the
  warning never printed and commands ran unrestricted with no notice on
  stderr. Found while proving the #7007 fix end to end in a container.
- The middle working mode is called Work everywhere in the TUI: the footer
  chip, the mode name and description, the hotbar action, settings errors,
  `/context` and exports, and the matching locale strings, which now use each
  language pack's existing word for it. The hook contract is unchanged:
  `DEEPSEEK_MODE` and `DEEPSEEK_PREVIOUS_MODE` still report `ACT`, and saved
  settings still read and write `agent` (Refs #6466). The `/statusline` Mode
  hint says Work too. Thanks @Lstarsky0 (#7012).
- The model catalog now carries current models from Anthropic, Google and
  Meta. Anthropic: `claude-opus-5-5`, `claude-sonnet-5-5`, `claude-haiku-5-5`
  and `claude-fable-5-1`; the Auto router's cheap tier and the Scout companion
  stay on `claude-haiku-4-5`. The `anthropic` default moves to `claude-sonnet-5-5`, and no default
  uses `claude-sonnet-4-5`, which retires on 2026-11-30. OpenAI's GPT-6
  family (`gpt-6-sol`, `gpt-6.1-sol`, `gpt-6-luna` and `gpt-6-astra`) is left
  out of the direct route because OpenAI does not support normal tool calling
  for them over Chat Completions. Google: `gemini-4-argon` and
  `gemini-3.8-flash`. Z.ai: `GLM-5.3-FlashX` is not offered because the
  Coding Plan endpoint the zai route defaults to does not serve it. Meta:
  `muse-spark-1.3`, `muse-spark-1.3-contributor` and the `muse-wonder-1.0`
  preview join the route, which stays on its previous `muse-spark-1.2`
  default.
  Several ids are retired. Xiaomi MiMo now defaults to `mimo-v2.6-pro`, and
  configs naming `mimo-v2.5-pro` or `mimo-v2.5` send `mimo-v2.6-pro` and
  `mimo-v2.6-flash`, because Xiaomi rejects the old names from 2026-10-21.
  A `[vision_model]` that names either old id gets the same mapping.
  `gemini-3.7-flash` and `gemini-3.5-flash`, which Google already serves as
  `gemini-3.8-flash` and `gemini-3.6-flash`, are now selector aliases of those
  models. `qwen3.8-max-preview` is gone from the Model Studio rosters. MiniMax
  M2.7 and M2.5 (including highspeed) leave the fallback picker before
  MiniMax closes them to new users on 2026-10-19, and keep working for
  existing configs until they retire on 2026-11-18.
- Direct DeepSeek routes now send `deepseek-flash` for `deepseek-chat`,
  `deepseek-reasoner`, the `flash`, `deepseek-v4flash`, `flash-vision` and
  `deepseek-v4flashvisionexp` shorthands, and the Auto router's fast tier.
  `codewhale model set` and `/provider` now write `deepseek-flash` for those
  shorthands too, matching what `model = "flash"` and `/model flash` resolve
  to.
  DeepSeek has retired the V4 Flash and V4 Flash vision models. It still
  accepts `deepseek-v4-flash` and `deepseek-v4-flash-vision-exp`, but serves
  them with V4.1 Flash, bills them at the Flash price, and calls that routing
  temporary. Turns on either name from 2026-09-10 04:00 UTC are now priced at
  the Flash rates ($0.003 cache hit, $0.15 cache miss, $0.60 output per
  million tokens off-peak, doubled at peak) instead of the old V4 Flash rates,
  which overstated a cache hit by about 2.3 times. The vision name was not
  priced at all before. Earlier turns keep the rates they were billed at,
  including turns recorded under a shorthand. On NVIDIA NIM, OpenRouter,
  Novita, Fireworks, SiliconFlow, SGLang, vLLM, DeepInfra and Together,
  `deepseek-flash` selects that host's V4 Flash model, so the `flash`
  shorthand keeps working there. On OpenCode Go, `deepseek-flash` and the
  `flash` shorthand select Go's `deepseek-v4.1-flash`; until this fix,
  `/provider opencode-go flash` was rejected as an invalid model.
- When `$SHELL` is zsh, the `bash` tool now runs commands with zsh's NOMATCH
  and EQUALS options off. Common Bash idioms such as `grep --include=*.rs`
  and `[ "$a" == "$b" ]` now run instead of failing with `no matches found`
  or `= not found`. In 310 local DeepSeek sessions, 334 commands had failed
  this way, which silently dropped searches the model believed it had run.
- `task_shell_wait` is now in the first-turn toolbox. A `bash` command that
  moves to the background now tells the model to call it directly. Before,
  every long build or test cost a `tool_search` round trip and a tool-catalog
  re-pin, and models often polled with `sleep` instead.
- `codewhale exec` receipts no longer report an empty final answer when a
  background shell finished after the model answered. This happened in 52
  of 185 DeepSeek exec runs, and a fleet worker then recorded no answer.
- A reasoning-only response cut off at the output limit is no longer re-sent
  up to two more times on Responses routes such as `deepseek-flash`. It now
  fails with the output-limit message. The engine's own check missed the
  `incomplete:` prefix that the Responses parser adds.
- `tool_choice: "none"` now reaches the provider on Responses routes and on
  DeepSeek Chat routes with thinking on. Compaction and final-report
  requests rely on it; DeepSeek documents it as valid in thinking mode.
  `required` and named choices are still not sent while DeepSeek is
  thinking, because that returns a 400.
- A DeepSeek Chat response that ends with `insufficient_system_resource` or
  `aborted` is now treated as incomplete, not as a finished answer.
- On DeepSeek, the Auto-Review guardian now asks for low reasoning effort.
  It used to leave thinking at DeepSeek's default (high), so the reasoning
  could use up the guardian's 4,096-token answer budget, report the review
  as unavailable, and stall auto-approval.
- Durability and blocking-call fixes from the #6553 and #6555 audits:
  - On Windows the updater syncs the staged binary, tries an atomic replace
    first, and reports where the previous binary is if restoring it fails.
  - Config writes sync the temp file before the rename.
  - The task store, Fleet run and receipt reads, and session listing now
    run off the async runtime.
  - Worktree names that are empty, start with `-`, or contain `..`, `\` or
    control characters are refused. `git worktree add` gets a `--`
    separator before the path.
  - The boot snapshot prune repairs a HEAD that names a missing commit
    before pruning, instead of logging "bad object HEAD" (Refs #6978).
- Typing into a terminal dock that has no sessions no longer shows the
  "reset or lost" error. Thanks @Lstarsky0 (#7010).
- The `/mcp` Extensions panel's detail pane grows to fit its wrapped text,
  up to a third of the panel, so names, descriptions and details are no
  longer cut off at 80×24. Thanks @Lstarsky0 (#7011).
- When a `tool_search` activation pushes an earlier deferred tool out of the
  bounded conversation toolbox (8 tools, 16 KiB of schemas), the search
  result now names it under `evicted_tool_references`. Before, the tool
  silently disappeared from the next request, and a model could go on
  calling it. A tool called in the current or previous turn is never
  evicted, so the toolbox can briefly hold more than that. Thanks @asto18089 for the cost and correctness analysis
  (Refs #6969).
- An ACP client that asks how to turn on Full Access is now told the
  routes that work: start the server with `codewhale --yolo serve --acp`,
  or set `yolo = true` in config.toml. The hint used to recommend
  `--approval-policy full-access` or `approval_policy = "full-access"` in
  config.toml. Both are refused when Codewhale starts, because config.toml's
  `approval_policy` only accepts on-request, untrusted, never, auto and
  suggest. Thanks @harryvizcaino-oss for the Paseo report (Refs #6992).
- On Linux, a credential directory the sandbox hides no longer breaks every
  sandboxed command when it is reached through a symlink, such as `~/.kube`
  linked into a synced dotfiles folder or a `~/.config` that links
  elsewhere. Each command used to abort before running with a bubblewrap
  mount error ("Can't mount on symlink destination"), because the mask was
  placed on the link. The mask now goes on the directory the link resolves
  to, so the credentials stay hidden through the link. Thanks @rafacouto for
  the report and the standalone bubblewrap reproduction (Refs #7007).
- `/help` keeps its head-noun shortening to English. In German and
  Indonesian it had kept the first and last words of a long description,
  which can say something else (`Periksa penyedia aktif` instead of
  `Periksa saldo akun`). Other languages now shorten to whole words from the
  start. Thanks @Lstarsky0 (#6995).
- `/theme` right-aligns its row numbers, so names, swatches and taglines in
  rows 10 and later line up with rows 1–9. Thanks @Lstarsky0 (#6998).
- `GET /v1/settings/schema` accepts `?locale=<tag>` and resolves row labels,
  descriptions and option labels in that locale, with the configured
  `locale` as the default. The desktop sends the interface language it
  shows, so a desktop opened from the Finder or the Dock on a Chinese
  system, which has no `LANG`, no longer lists its Settings rows in English
  under Chinese headings.
- `codewhale --continue` reopens a session whose terminal was closed or
  killed mid-turn. It used to print "Recovered interrupted session" and then
  exit with "session … is open in an interactive Codewhale session": the
  recovery had registered the session as this process's own live session,
  and the resume step that follows refused it. Recovery now holds the
  session's lease only while it promotes the checkpoint, as `--resume <id>`
  does.
- Queued messages restored from an earlier launch or another session are no
  longer sent automatically. They wait in the queue until `/queue send <n>`
  sends one or `/queue clear` discards them. The restore notice says so and
  is now also written to the transcript, because the "Resumed session" status
  used to hide it. Messages queued in the current session still send in order after each turn,
  ahead of the held ones. Before, restored messages went out one by one after
  your next message. On a machine where the queue file could not be cleared,
  that re-sent the same old messages at every launch (#6978). Editing a held
  message with ↑ and cancelling the edit puts it back as an ordinary queued
  message. A failed queue write is logged with the OS error and session id.
  Thanks @SergiuSSoluS for reporting the re-delivered turns (#6978).
- A long session whose history holds more images than its model accepts in
  one request no longer becomes permanently unsendable. When a provider
  refuses the number of images (for example vLLM's "At most 3 image(s) may
  be provided in one prompt", "too many images" or "maximum number of
  images"), Codewhale learns that limit for the provider and model (the
  number the provider states, otherwise half the images sent) and resends
  with only the newest images attached. Each older image is replaced by a
  fixed note telling the model it remains in the session and not to guess
  what it showed; a tool image's note says to take a fresh observation, such
  as a new screenshot, rather than repeat the action. Each resend carries
  strictly fewer images, there are at most three per turn and never an
  unchanged one; after that the turn ends with an `image_count_exceeded`
  error and the next message uses the learned limit. A stated limit of 0
  sends images as text, as for a model without image input. Before, a count
  rejection either ended every later turn the same way or was misread as
  "no image input" and stripped every image. The window moves in steps of a
  quarter of the limit, so the cached history prefix changes only once every
  few new images. Each change is recorded as a `request_image_window` event
  (runtime threads store it as a status item with that code, `exec
  --output-format stream-json` prints it and the TUI shows it), and
  `/preview-request` and the compaction summary request apply the same
  window. A summary request refused for its image count teaches the limit
  too, and the context recovery that sent it retries under that limit. A
  sub-agent no longer retries a count rejection as a transient
  failure. Known limits: nothing changes until a provider has refused once,
  and no documented per-model limits are applied; the learned limit is kept
  in memory per engine and per sub-agent, so a restart learns it again with
  one refused request; the oldest images go first, user attachments
  included; a refusal that arrives mid-stream ends that request and only the
  next send uses the limit; a gateway that reports the refusal as a 5xx is
  retried unchanged by the transport first; a request-body size (HTTP
  413) refusal on an ordinary turn still has no recovery; and the context
  budget estimate still counts every stored image, including those the
  window sends as notes, so a small context window can still trip context
  recovery.
- Switching provider is now refused while a turn is running, as model,
  thinking and permission changes already were. The refusal covers
  `/provider <id>`, the provider picker, a custom provider, setup and login
  flows, and adopting a local Ollama. The status line reads "Provider is
  locked while a turn is running — press Esc to interrupt first". Before, a
  mid-turn `/provider <id>` replaced the engine under the running turn
  (Refs #2982).
- `/init` now asks the model to write `AGENTS.md` in the interface language
  (for example Simplified Chinese when the TUI runs in zh-Hans), headings
  included, keeping commands, file paths and code identifiers as they are.
  Before, the prompt always said "Write in English". The non-interactive
  `codewhale init` template is unchanged and still English
  ([#6964](https://github.com/codewhale-hq/Codewhale/issues/6964)). Thanks
  @Asteria1028.
- A shell command, shell input or verifier that contains an invisible or
  bidirectional Unicode character is now refused before any approval card or
  reviewer call in Ask, Auto-Review and Never. This covers zero-width spaces
  and joiners, bidi marks, overrides, embeddings and isolates such as U+202E,
  tag characters, stray variation selectors and terminal control characters;
  ASCII control keys sent as input to a running program, such as Ctrl-C or an
  arrow key, stay allowed. The refusal names the code point and its position,
  because the approval card and other clients cannot show the character: the
  terminal drops it and other renderers may reorder the line around it. To
  use the character on purpose, resend the command with it written as a shell
  escape, for example `$(printf '\342\200\256')`. Emoji sequences, keycaps,
  subdivision flags and one variation selector after CJK or emoji text are
  unaffected; a joiner or mark inside Persian, Arabic-script, Indic or Hebrew
  words is refused too.
  The executed command is never rewritten, and Full Access is unchanged.
- A `curl` or `wget` whose request body uploads a credential is now held by
  the built-in safety gate. This covers `-d`/`--data*`/`--json`/`-F`/
  `--url-query`/`-T`/`--upload-file`/`-H @file`/`--post-file`/`--post-data`
  (and wget's abbreviations of them) carrying `.env`, `~/.ssh/id_*` (public
  `.pub` keys excepted, judged per comma or brace alternative),
  `~/.aws/credentials`, `.netrc`, a key file, a secret-named variable such as
  `$GITHUB_TOKEN` or `$OPENAI_API_KEY`, or stdin fed from one of those, from
  `env`/`printenv`, or from `echo`/`printf`/`<<<` of a secret-named variable.
  A shell command, shell input or verifier over 64 KiB or 4,096 words whose
  text contains `curl` or `wget` anywhere, or one nested too deeply to read,
  is held the same way under its ordinary heading, with a reason saying it
  was too large or too deeply nested to check. Ask shows a card headed
  "Upload credentials (…) with `…`" that a session grant or saved allow rule
  cannot answer; Auto-Review and Never deny the call. Runtime clients receive
  the same heading as the approval `summary`. A literal request header
  (`-H "Authorization: Bearer $TOKEN"`) is not affected, and other upload
  tools (scp, rsync, nc, httpie, `aws s3 cp`), secrets inside an ordinary
  file, here-documents, `$(printenv NAME)` inside a value and a credential
  name split across a brace or quotes (`.net{rc,}`, `.n""etrc`) are not
  detected. Full Access is unchanged.
- Switching provider, model route, profile or workspace, or changing plugins,
  while a `/compact` is running or queued no longer leaves the session stuck.
  The pass on the replaced engine is stopped and the transcript says "Making
  room stopped; the conversation was not changed". Before, `/new`,
  `/new --force` and `/resume` were refused with "context compaction is
  running" or "a manual compaction is queued", and `/compact` answered
  "Already making room.", until Codewhale was restarted. A `/compact` still
  waiting for a free engine slot is carried to the new engine as before. A
  context purge caught by a switch cannot be stopped: the replaced engine may
  still finish and bill its request, and the transcript now says "Session
  cleanup was abandoned with the replaced engine; the conversation was not
  changed" instead of blocking `/new` with "session cleanup is running". A
  turn still running when the engine is replaced is not covered and can keep
  `/new` refused. Thanks @luestr for reporting the refusal in #6981; this is
  one way to reach it, and the report stays open until the blocker that
  0.10.2 names confirms its cause (Refs #6981).
- DeepSeek on the Responses route (the `deepseek-flash` default) no longer
  fails with `400 reasoning_text must be passed back` when a session replays a
  tool call that has no stored DeepSeek reasoning: a turn that was interrupted,
  ran with thinking off, came from another provider, or where the model wrote
  text before its tool calls without visible reasoning. Codewhale now sends each
  assistant turn's reasoning first, ahead of its text and tool calls, using the
  same `(reasoning omitted)` placeholder the Chat route already uses when there
  is none. Live probes on 2026-10-19 returned 400 for text → reasoning → tool
  call and for no reasoning, and 200 for reasoning → text → tool call. The same
  multi-step task that failed in 2 of 4 runs on the first 0.10.2 candidate
  passed 6 of 6 with the fix. On Chat, DeepSeek enforces this only for
  tool-call turns, which were already covered.
- At reasoning effort `max`, the first-party DeepSeek route now asks for up to
  131,072 output tokens, DeepSeek's documented default at that effort, instead
  of 65,536. Every other provider and effort keeps 65,536, and explicit
  overrides and route limits still apply.
- `codewhale exec --output-format stream-json` adds the provider's
  `finish_reason` to each `turn_usage` record, so a tool-call stop can be told
  apart from an output-limit stop or a provider abort. The field is omitted
  when the provider reports none.
- The `/statusline` picker's item hints are localized in all 15 UI languages
  instead of always showing English.
- A custom OpenAI-compatible relay's model list is now discovered. Four
  separate defects kept it out of `/model`: a roster answering both `data` and
  `models` was rejected as a duplicate field; only the active route was probed;
  `/v1/models` was authenticated with `x-api-key` whenever the route declared
  `wire = "anthropic"`, which a relay that rejects that header answered with
  `401 missing_api_key`; and the picker's default view then hid the relay's own
  rows. The startup probe now covers every configured relay, a built-in
  provider name pointed at a private endpoint (`[providers.openai]` against a
  relay) counts as a relay, and the provider panel no longer reports its
  catalog missing while listing it. Thanks @SparkofSpike (#7013).

- The footer preview in `/settings` now uses the footer's own words and
  colours, in the UI language: Startup mode Work previews `● ask   work`
  and Plan previews `● read only   plan`, as the footer under the composer
  shows them. Thanks @Lstarsky0 (#6993, Refs #6466).

- `/resume`, `/load`, and a provider, profile or plugin change keep
  configured stdio MCP servers running, as `/new` already did: the replacement
  engine adopts each live process when its workspace, MCP backend and server
  config are unchanged. A workspace switch still restarts every server, and
  servers added at runtime still close.
- Provider lists no longer show two "MiniMax" rows or four "Alibaba Cloud
  Model Studio" rows. The variant endpoints are labelled "MiniMax (Anthropic
  API)", "Alibaba Cloud Model Studio (Token Plan, Anthropic API)", "Alibaba
  Cloud Model Studio (Coding Plan)" and "Alibaba Cloud Model Studio (Coding
  Plan, Anthropic API)"; the default MiniMax and Model Studio rows keep their
  names.
- Compaction no longer fails with "Chat API response missing choices" on
  gateways that wrap a non-streaming Chat Completions response as
  `{"data": {"choices": ...}, "success": true}`, such as Sub2API in front of
  Cline. A response with top-level `choices` parses as before, and an envelope
  without `success: true` still fails. Thanks @luestr for the response headers
  and sanitized body in #6981; the `/new` refusal reported there is not
  changed by this fix.
- Experimental extension host: a plugin can no longer provide the core
  `subagents` service. Its activation fails with "extension may not provide
  core service `subagents`", as it already did for `agents`, `approval` and
  the other core names. Codewhale's own sub-agents and the tools they can use
  are unchanged.

- OpenHarmony builds on Windows select Ninja, with an SDK-bundled fallback and
  a clear error when it is unavailable. The CLI keeps its download and Work
  context integrity checks available on OHOS by sharing an ungated SHA-256
  helper (thanks @shenjackyuanjie, #6989; reported in #6988).

- Windows mouse-wheel scrolling is restored after returning from the external
  editor. The handoff now releases both Windows console mouse capture and
  terminal mouse modes. Thanks @SparkofSpike for distinguishing terminal
  scrollback from transcript scrolling in #6652; the separate hours-long
  scrolling slowdown still needs reproduction.
- Shell classification accepts PowerShell’s call operator, quoted ampersands
  and combined output redirection without losing destructive commands in
  nested shell payloads or after command separators. Thanks @SparkofSpike
  for the diagnosis and fix in #6929, and @jayanthvee for the detailed
  Windows report in #6963.
- Windows commands that cannot be classified use the normal approval path
  with a direct explanation. Ask can request approval and Auto-Review can
  consult its reviewer; an unclassified command is no longer reported as an
  npm-launcher kill. Actual unbounded `node.exe` image kills keep their hold
  in reviewing modes; Full Access continues to bypass that floor. Thanks
  @jayanthvee for separating these cases in #6963.
- Pasting while a modal is open no longer changes a history-search query
  hidden underneath it. Existing modal key ownership and hidden-caret guards
  remain in place. Windows Terminal IME preedit is still controlled by the
  terminal; native IME retesting remains open (#2323).

- `codewhale model list`, `model resolve`, the model picker, sub-agent model
  validation and Runtime facts read endpoint-scoped catalog facts. `model set`
  saves the exact route selection, and `app/models` projects the captured
  Runtime's catalog. Compatibility aliases, advisory route checks, the agent
  `ModelInfo` capability flags and HTTP app-server route admission still read
  the bundled curated overlay; admission uses it only to reject a model the
  catalogue knows for a *different* provider. `model resolve` now prints
  `limit_overrides` whenever the resolver applied a sourced override, so a
  context or output limit names its origin instead of looking catalog-native.
  Cost estimates
  preserve complete price tiers, calculate exact decimal amounts per request,
  and freeze rates at dispatch.
  Missing rates or required usage breakdowns stay unpriced; public prices do
  not establish account access. A model priced only by a live models.dev
  refresh gets a cost estimate at the same confidence as bundled models.dev
  prices instead of showing as unpriced. The offline seed records its source snapshot
  and keeps absent default wire IDs as dated compatibility fallbacks. Refs #6396.

- Desktop and HTTP app-server turns feed an existing shared pet companion when
  its followed session has no live producer. Automatic selection is checked
  atomically by the owner; a live TUI keeps its source. The Runtime producer
  forwards activity metadata only and releases its connection after a 12-second
  completion tail. Refs #6155, #6139.

- `bash` accepts an optional `description`, shown on its card. It never
  changes execution, approval matching or Auto-Review input.

- A first launch with no key or route now opens the composer. Codewhale asks
  for a provider when you send your first message, keeps the message in the
  box, and offers a local model only when a local Ollama server answers. The provider list
  now shows what you type to filter it during first-run setup.
- Removed the own-sandbox cloud dispatch (`--own-sandbox`, `/dispatch`,
  `agent runtime:"cloud"`, the `cloud-agent` alias, and the `--confirm`,
  `--remote`, `--branch`, `--cwd`, `--status` and `--list` options).
  `codewhale dispatch "<task>"` now runs only through the signed-in account,
  with its quote and EU-compute consent. Cloud computers and accounts are
  paused for new users, so `codewhale dispatch` is not available to them.
- `codewhale dispatch` uses the account's neutral wording for cloud errors,
  runs for the quoted seconds unless `--seconds` is given, and asks for consent
  only when both stdin and stdout are terminals. It refuses a message that was
  not read as new Work, and prints the Message ID.
- MCP servers now connect when a session starts, including servers from
  enabled plugins; set `lazy = true` on a server you configure to connect it
  on demand instead. Finishing an OAuth login reconnects just that server, and
  `/mcp reload` redials every server that is not `lazy` and reports how many
  are not connected out of the configured total.
- The MCP status no longer stays at "connecting" after the servers have
  finished. The final status was lost when it arrived while a reply was
  running, when the interface was busy, or when the last server was removed;
  the newest status is now kept until the interface can show it. Refs #7035.
- `/new` keeps configured stdio MCP servers running: the next conversation
  adopts each live process when its workspace, MCP backend and server config
  are unchanged, and dials only what is missing. HTTP/SSE servers reconnect
  so they pick up current credentials and network policy; servers added at
  runtime and plugin-contributed servers are closed with the conversation.
  A workspace switch still restarts every server, and the MCP panel and
  `/mcp` now show the new engine's status instead of staying stale.
- The mobile page retires an approval card's Allow and Deny buttons when that
  approval is decided from another client, cancelled or timed out, and shows
  which happened.
- Settings name compaction "Compact at" with a plain-language hint, explain
  Input history as the number of sent messages kept for recall, and label the
  Anthropic-compatible DeepSeek endpoint "DeepSeek (Anthropic API)".
- Running shell commands open an inspector with their output and status. It
  follows new output until you scroll back, and drops Stop once the command
  ends. `Alt+V` opens the card you clicked, even after newer cards arrive or a
  tool run collapses around it. `/jobs wait` and `/jobs poll` open the same
  inspector instead of freezing the screen for up to 5 s, a shell row in the
  work dock opens it too, and a task row opens `/task show` instead of failing
  with "Job … not found" (#6931).
- In `/setup`, `u` and `d` act only on the working-agreement step. Pressed on
  another step they no longer close the wizard and switch your agreement off.
- `/setup` no longer says "You're ready." when no model is connected; its last
  step asks for a provider and lists the P and C keys it accepts. A rejected
  API key's reason stays visible at narrow widths.
- First run with a local Ollama model continues to the trust question and the
  ready screen instead of ending onboarding, and the next launch no longer
  starts onboarding again.
- A turn that ended in failure reads as an error, live and after resume. Retry
  notices stay in the transcript instead of flashing a red error that hid the
  recovery.
- File edits show as patches with their change summary once. Tool status
  words read `done`, `failed` and `loaded, not run` everywhere, and a tool
  that only loaded no longer gets a done check.
- Delegated agents' cards name the tool they are running instead of
  "Core tool <call id>".
- Markdown: a line like `#6235 fixed it` is no longer a heading, headings
  render inline code and emphasis, and tables draw one rule under the header
  instead of one between every row.
- The approval card shows its field labels and description in your language in
  every locale, not only Simplified Chinese.
- `/mode work` says "Switched to Work mode.", and `/zidong` says it switched to
  Full Access. `/overview` and `/setup` call the mode Work.
- The language screen points to `/config locale <tag> --save`, the Korean
  Never chip says it allows nothing, and the elevation dialog's Abort says it
  skips only that tool call.
- Less work on the UI thread and less memory per turn:
  - the context meter keeps its estimates for unchanged history;
  - tool calls share the turn's history instead of copying it;
  - compaction checks pressure with one pass;
  - write_atomic sweeps a directory for stale temps at most hourly;
  - the pet costs nothing while hidden;
  - `GET /v1/usage` reads the turn store once, not once per thread;
  - each save's session cleanup no longer reads every session's cost ledger;
  - sub-agent checkpoints copy only the tail they keep;
  - the underwater theme paints its background once per frame.
- `/resume`, `/load` and other session changes that cannot run yet now list
  what is still busy and how to stop it. Thanks @SparkofSpike (#6948).
- Sessions that ended without a clean exit show `interrupted` with their age
  in the session list.
- `codewhale logout` also deletes a stored legacy sandbox token.
- `codewhale lane list` prints an empty list instead of failing when no lane
  has been started, `codewhale completion` no longer errors when its output
  pipe closes early, and `session-diagnostics`, `receipts` and `features` help
  and errors say what to pass and where to find saved sessions.
- A missing `--config` file, a missing `-C` workspace directory and an unknown
  `--profile` now fail at startup with a clear message.
- `codewhale web` works when `$HOME` sits under a root-owned symlink.
- Typing `/exi` and pressing Enter now completes the command instead of running
  a different destructive command (`/exit`, `/clear`, `/new`, `/logout`,
  `/restore`).
- The Shift+Tab permission hint stays in the footer while in Auto-Review and
  Full Access.
- An idle pet view no longer builds animation frames while nobody is
  looking at it (#6728).
- On the pet main view, `Ctrl+]` cycles the pet's views: the
  companion `whale`, a crisp local `cove` whale without the fog, `braille`,
  `pixels` (kitty graphics), `particles`, `whale-girl` and installed avatar
  packs. Terminals with the kitty keyboard protocol can also send `Ctrl+Tab`
  and `Ctrl+Shift+Tab` to move forward or back; Terminal.app uses those to
  switch terminal tabs, so use `Ctrl+]` there. The footer names the view, and clicking
  it cycles. A view that cannot draw shows its fallback and says what it
  needs, for example `pixels needs kitty graphics · showing particles`.
  `/pet avatar <key>` jumps to a view, and the `/pet` usage lists every view,
  `style` and `language`.
- `F6`, `F8` and `F9` work on the pet main view, not only in the `F5`
  inspector. Every F-key hint on macOS, including the help overlay, reads
  `fn+F5`, `fn+F8` and so on, including in ASCII-safe mode. `/pet on`, `F8`
  and `F9` restart a companion view that stopped, so "/pet on retries" is
  true. A failed launch shows its own reason, a successful one says
  `habitat open`, and pressing `F8` several times while the companion starts
  opens one tab. `F8` and `F9` also restart a stopped companion from the inspector.
- The live dot whale stays dense in large panes, the whale-girl sprite scales
  by whole pixels, and avatars, the particle grid and the pixel view no longer
  show the whale through them. Avatars follow your theme and
  `CODEWHALE_ASCII_SAFE`, the pixel view's image is a whole fraction of the
  terminal's pixel size, and the pet's view worker sleeps until its next
  deadline instead of waking every 2 ms.
- The terminal dot whale respects studio brightness and sleep dimming. Normal
  companion startup no longer shows a missing-service notice, avatar atlas
  notices match the current frame, and pet fallback descriptions are translated.
- README: `codewhale exec --auto` is shown for headless runs that edit files
  or run commands.
- `bash` now moves a foreground command to the background after 30 s and
  reports the elapsed time and how long ago it last printed output.
  `task_shell_start` is removed.
- Every change of effective permission now writes a `Permissions changed to …`
  line in the transcript, and runtime threads record it as a `status` item and
  a `thread.permissions_changed` event.

- The bundled first-party catalog pins marketplace revision
  `2e3a6fba453c9b34d5b1444ab5bd6edc7b119189` with 19 catalog entries,
  including the bundled Skills pack. Installed bundles carry the reviewed
  Computer Use and bridge fixes and the MIT license.
- `codewhale app-server` no longer serves `GET /jobs`, which was always empty;
  background jobs are `GET /v1/jobs` on the Runtime API (#6513).
- `codewhale app-server` started inside a project no longer writes
  `.deepseek/events.jsonl` there; with no configured path the hook log sits
  beside the state database in the Codewhale home. Thanks @Lstarsky0 (#6921,
  Refs #6513).

- Streaming requests honor the configured retry count even when provider
  backoff or `Retry-After` exceeds the response-header timeout. Chat,
  Responses and Anthropic apply that timeout to each physical request;
  waiting between attempts does not consume it. HTTP/1.1 responses receive
  the same error classification and retry handling. Explicit weekly,
  five-hour and plan-reset usage exhaustion stops as a quota error, while
  temporary request-rate limits remain retryable. Closing retry receipts
  label each per-kind counter so it does not imply which failure ended the
  turn. Failed Chat opens retain the provider's `reasoning_content`
  diagnostic. Ambiguous transport failures can still replay a request the
  provider accepted and incur a second completion charge; protocol fallback
  remains limited to one HTTP/1.1 attempt per transport open. Refs #7045.
  Thanks @7jrxt42BxFZo4iAnN4CX.

- Body text on a screen that paints its own background now uses the theme's
  primary text colour, and on a terminal that does not report a background
  colour it uses the terminal's own foreground instead of an assumed ivory
  tone. The System and Blue Stage themes showed ivory text on a light
  terminal because Codewhale could not measure the ground it was drawing on;
  modals, the nested underwater surfaces, fleet setup, the automations view,
  the live transcript and the worktree manager now carry body ink explicitly.
  A theme that never sets an ink colour for one of its own surfaces can still
  be hard to read. Refs #7043. Thanks @Statter.

- A saved session that cannot be resumed because the provider it recorded is
  gone now says how to keep its history: the refusal names
  `codewhale sessions export <session id>` with that session's real id. The
  hint now appears at three resume sites - the TUI resume, the executor
  resume and the Runtime API - instead of only one. A Runtime resume refused
  because the persisted provider route needs an exact custom provider is now
  reported as a bad request rather than an internal error. Other persist
  refusals keep their current status; `codewhale resume` and `fork` on the
  mount path still refuse without the hint. Refs #6996. Thanks @luestr.

- A read-only role whose shell command is refused now reads that the run has
  no network, that a read-only command reaches the network only in Full
  Access with an allowing session sandbox, a `[network]` policy that does not
  deny by default and a network grant, and that it should use a web tool or
  report the source unreachable. That replaces a child reporting "bash denied
  for non-admitted programs". The same hint covers a command that starts with
  `cd ... &&` and the contract bash path as well as the legacy one. A network
  attempt made from Python or `gh` still gets no hint. A read-only child's own
  briefing states up front that its shell has no network and that `web` is its
  only route to a page, so it reports a source it could not fetch with the
  error it saw instead of blaming a blocked program. Refs #7040.

- A `codewhale resume` or `codewhale fork` that is refused no longer leaves a
  half-bound session behind that blocks the next attempt. A saved session
  whose beginning changed after an earlier resume, through a compaction
  checkpoint, contract pruning or a restored sub-agent handoff, now resumes
  instead of failing with "saved holder checkpoint differs from the saved
  history" or "saved holder has a successor outside the selected document".
  Refs #6954. Thanks @7jrxt42BxFZo4iAnN4CX.

- On Windows, Python run through the shell tool no longer garbles Chinese and
  other non-ASCII output. Windows shells were meant to start with
  `PYTHONIOENCODING=utf-8`, but adding hook variables replaced the whole
  environment and dropped that default on every run, so piped Python output
  used the system code page while Codewhale read UTF-8. The default now
  survives. A `shell_env` hook can still set its own value (on Windows,
  variable names match regardless of case), and the default replaces a
  different `PYTHONIOENCODING` inherited from your environment. Refs #4202.
  Thanks @zhuowp (#7041).

### Added

- **Herdr pane integration** reports Codewhale's idle, working and blocked
  state, releases the pane on quit, and supplies the current session's resume
  command to Herdr 0.9.2 or newer. Thanks
  [Dhruv Singal (@dsingal0)](https://github.com/dsingal0) (#7064).
- The desktop app can connect an OrcaRouter account without a terminal. The
  Runtime API's `POST /v1/providers/orcarouter/connect` returns OrcaRouter's
  consent link, waits for the browser to come back and saves the key the same
  way `codewhale auth orcarouter` does; `GET` and `DELETE
  /v1/providers/orcarouter/connect/{attemptId}` report and cancel the attempt,
  and `GET /v1/providers` marks the route with `browserConnect`.
- `codewhale review`, the `review` agent tool and `/review` now share one
  reviewer. It can read the repository with read-only tools (read, list,
  grep, file search and `git status/diff/log/show/blame`) when the checkout is
  clean and, for a pull request, `HEAD` is the PR head; otherwise it reviews
  the diff alone and says so. Findings use one severity scale, `blocker`,
  `concern` and `nit`; the old `error`/`warning`/`info` words map onto it.
  Review effort follows the change's risk: sensitive paths (auth, billing,
  migrations, secrets, sandbox, schema and similar) or large changes get more
  steps and tool calls, small low-risk changes get one pass over the diff.
- Every review body now ends with a severity tally as one HTML comment,
  `<!-- codewhale-review: {"blocker":0,"concern":0,"nit":0,"introduced":0,"preexisting":0} -->`,
  so a repository can gate a merge on "no blockers" without scraping prose or
  granting Codewhale `checks: write`. Findings also say whether the line they
  point at was introduced by this change or was already there, decided from
  the diff rather than from the model's own label. `codewhale review`,
  `codewhale review --pr --post` and the extension-host path write the same
  body.
  `--money-cap-usd`, with `--input-usd-per-mtok` and `--output-usd-per-mtok`,
  stops a review at a spending cap; without both prices the receipt says the
  cap could not be applied. A review that stops early names the limit and says
  that files it did not reach were not reviewed. Each finding says whether it
  was reproduced, traced or only suspected; suspected ones move to a short
  "worth a human look" list, and on high-risk changes a second pass tries to
  refute each finding and can only remove findings, never add them. A defect in
  code the change did not add is labelled pre-existing, kept out of inline
  comments and listed separately, and on its own does not raise the merge
  risk. The summary lists what the reviewer checked and found sound under
  "Checked, nothing wrong".
- Runtime API: `GET /v1/workspace/status` reports `workspace_trusted`, and
  `POST /v1/workspace/trust` with `{"trusted": true|false}` records the
  server workspace's project trust in the same record `/trust on|off --save`
  writes. It takes no path and needs write authority; watch tokens can read
  but not write. The answer is the decision read back, with the `config_path`
  that holds it. Revoking trust does not remove `AGENTS.md` or project rules,
  project hooks still need their own approval, and project MCP servers change
  only after the engine restarts. Thanks @gaord (#6959).
- Runtime API: when `capabilities.account_model_owner` is true, a turn can
  name the signed-in account in `account_model_owner`, with provider
  `codewhale` and an exact `provider/model` route. That turn runs only on that
  account's credential, never a local key, an environment variable or another
  provider, and is refused if the account's model access is missing, expired
  or revoked. The Chat relay takes the same field as `accountModelOwner`.
- The model can hand a goal back at a milestone: `update_goal` with
  `status: "yield"` ends the turn with the goal still unfinished, and your
  next message resumes it. A goal the model reported blocked, or one you
  paused, still needs `/goal resume`. Thanks @SparkofSpike (#6930).
- Bundled Computer Use can re-read only what changed in an app
  (`get_app_state` with `since`) and run up to eight steps in one
  `run_actions` batch. Each step can target a unique element found just
  before it runs, and the batch receipt says which steps completed and
  whether input was sent, so a failed batch is not replayed. On Linux, keys
  and pointer events go only to the window that will receive them, and are
  refused when that window cannot be determined (including under Wayland) or
  while a menu is open. Linux also gains AT-SPI focus, value reading and text
  selection, app-window capture, ffmpeg screen recording, `kill_app` and
  `list_sessions`. Computer Use remains a preview.
- Standard Compute is available as a bundled compatible provider, with the
  `standardcompute` router default and authenticated model discovery. Its
  billing basis remains Unknown (#6985).
- Sub-agents retain permitted code-mode tool composition. Nested calls keep
  the child's approvals, write scope, tool timeout and work deadline; approval
  waits do not consume that tool timeout. Refused nested calls still do not
  count toward Fleet's denial-loop guard. Refs #6841.
- `/connect [<provider>]` signs in to a model provider that a plugin adds,
  inside the TUI, and `/disconnect <provider>` removes that sign-in.
  `/login` and `/logout` are for your Codewhale account only.
- `codewhale install <source>` installs a plugin from the terminal through the
  same installer as `/plugin install`, without starting a model or needing
  model credentials. Both accept `git:github.com/<owner>/<repo>[@<ref>]` and
  `npm:<package>@<exact version>` as well as a local folder, GitHub archive or
  tarball URL. The plugin stays disabled and untrusted until you review and
  enable it; no Git hooks, npm lifecycle scripts or dependency installs run.
  Experimental. Thanks @LIghtJUNction (#6950).
- `codewhale auth claude` and `/auth claude` add Claude subscription sign-in
  to the existing Anthropic route, with protected owned credentials, refresh
  before requests, a provider-picker choice and local sign-out. API-key billing
  remains an explicit separate choice. This is experimental: live subscription
  grant and inference acceptance have not been qualified (#6932).

- `/pet on` makes the animated GPUI whale the main terminal view, with the
  existing message box, queued messages and permission controls always available.
  F5 or `/pet inspect` opens streamed replies, errors and the current session's
  agents; Escape returns to the same pet view and draft. `/pet off` restores the
  ordinary shell. Whether `/pet on` is active is remembered across launches,
  and the
  inspector can copy the last finished reply with c or its footer action.
  Motion preferences and unknown usage stay truthful (#6920, #6907),
  using the shared `codewhale-ratatui` components
  ([#23](https://github.com/codewhale-hq/codewhale-ratatui/pull/23), [#25](https://github.com/codewhale-hq/codewhale-ratatui/pull/25)).
- The pet view uses the latest Ratatui particle-whale renderer. Choose a
  smooth aurora or pixel whale with `/pet style smooth|pixel`, and choose
  English, Simplified Chinese, Traditional Chinese or Japanese activity words
  with `/pet language auto|en|zh-Hans|zh-Hant|ja`. Language follows the interface
  by default; a fixed choice lets you learn another language beside the normal
  status text. The 980 drones form the same full phrases as the interface,
  including `读取`, `搜索` and `调用工具`, then return to the whale. The appearance
  studio saves the same choices. Reduced motion uses a stable pose, and the
  reply inspector keeps priority on small terminals.

- A Terminal work dock (`/workbar terminal`) shows the model’s live PTY sessions
  in the current workspace, with ANSI-styled output, session selection, resize
  and direct keyboard input. `Ctrl+N` starts a separate shell; `Alt+Down` and
  `Alt+Up` switch sessions, and Esc returns input to the composer. Input remains
  bound to the selected shell’s identity across resets. Available on macOS and
  Linux; Windows names its current PTY limitation. The view shows a bounded
  output tail rather than emulating full-screen terminal programs (Refs #6912).

- When a Plan turn finishes with an answer, the TUI asks "How do you want to
  continue?". **Work (Ask)** and **Work (Auto-Review)** switch to Work with
  that permission and send the plan as the next message (a plan written only
  as prose adds one pending To-do); **Keep planning**,
  or Esc, stays in Plan and sends nothing; typed feedback is sent as the next
  message in Plan. The question appears only when the composer is empty, no
  message is queued, no other view is open and no agent is focused. If the
  chosen permission cannot be applied (for example it is locked by config),
  the session stays in Plan and the reason is shown
  ([#6902](https://github.com/codewhale-hq/Codewhale/issues/6902)).
- `/undo force` undoes a request whose end was never recorded, including
  edits made to its files since it started. Any other `/undo` option is
  reported as unknown and nothing runs.
- `/diff` opens the changed lines in a pager titled "Changes since session
  start", rendered like an edit in the transcript, under the file list and
  stat it already printed. Patch text is cut at 256 KiB and left out past
  200,000 changed lines; the message then says "The diff is too long to show
  in full. The file list above is complete."
- The model can stop a background shell command it owns: `task_shell_wait`
  takes `cancel=true`. That call needs approval, its card reads "Stop a
  running shell command", and it is refused when sent together with `gate`.
  The notices for a command moved to the background now name `cancel=true`.
- Each turn tells the model which configured MCP servers it may load, so a
  question can reach a server you never named. The note lists enabled, allowed
  server names only (at most 24), asks the model to `tool_search` a name
  before searching files or saying no tool exists, and is recorded again only
  when the list changes or a compaction dropped it. No server is started and
  no schema is loaded until the model searches.
- `codewhale mcp --help` lists the `mcp` subcommands with their descriptions
  and shows `Usage: codewhale mcp [OPTIONS] <COMMAND>`.
- A file-edit approval card in a folder with no Git repository says "No Git
  repository here, so edits ask first." on its question row. It appears only
  for `write_file`, `edit_file` and `apply_patch` in the posture where the
  same edit runs without a card inside a repository.

- Clients that start `codewhale serve` can declare a telemetry surface with
  `CODEWHALE_TELEMETRY_SURFACE`, including `vscode-extension`. This changes the
  session label while preserving collection settings and opt-outs
  ([#6916](https://github.com/codewhale-hq/Codewhale/pull/6916)).
- `codewhale account computers files` saves a Computer's workspace file (up to
  16 MiB) to account storage, lists its versions, and restores (up to 4 MiB)
  or downloads one. Restoring over an existing file needs its current
  `--expected-revision`; a download verifies size and SHA-256 and never
  overwrites a local file. `codewhale account computers delete` now asks for
  `--discard-files`. This is experimental.
- Runtime API: `GET /v1/goals` lists goal records across threads, newest
  update first, with `status` and cursor paging. Goals whose thread no longer
  exists are left out, and a cursor issued under a different `status` is `400`.
- Runtime API: `GET /v1/approvals` rows add a redacted `target` (a URL's host
  or a workspace path) and a one-line `summary`, plus `standing_rule` (`allow`
  or `deny`) when a remembered rule or grant answered instead of a person. A
  shell command keeps only its program name and plain arguments. Rows recorded
  earlier omit these fields.
- Runtime API: automations carry a `revision`. `PATCH`, `run`, `pause` and
  `resume` accept `expected_revision` and answer `409` `revision_conflict` on a
  mismatch. Create and those four accept an `Idempotency-Key`; a retry replays
  the first success instead of creating or running twice. `DELETE` honors
  neither. Clients check `capabilities.automation_mutation_preconditions`.

- The first start after an update says what changed, once: the version you
  moved to, how many features, fixes and other changes its notes list, and a
  link to that version's release notes on codewhale.net, in the UI language.
  The TUI shows it on the startup card, so the recent-sessions card no longer
  disappears on the first launch after an update; it moves into the
  transcript when the card leaves. Report and setup commands such as
  `codewhale doctor` print it to stderr in a terminal. `exec`, JSON output
  and non-terminal runs never show it, a fresh install or a downgrade shows
  nothing, and `[update] show_whats_new = false` turns it off.
  codewhale.net has a page for every version at `/en/changelog/<version>`
  and `/zh/changelog/<version>` with all of its entries.

### Changed

- `transcript_spacing` has two settings, `compact` and `spacious`, and
  `spacious` is the new default. `comfortable` drew the same conversation as
  `compact` everywhere except between activity rows, so the settings list
  offered a choice that did nothing. `spacious` leaves two blank rows between
  activity rows and three between turns. A saved `comfortable` (or `default`,
  `normal`) now reads as `spacious`; set `compact` to keep the denser layout.
- The OrcaRouter consent link that `codewhale auth orcarouter` and
  `/auth orcarouter` open now carries Codewhale's public `ref` identifier.
- The OrcaRouter sign-in origin can no longer be moved to another HTTPS
  address. `ORCA_AUTH_BASE_URL` and `ORCAROUTER_AUTH_BASE_URL` accept only a
  plain-HTTP loopback address, for local test fixtures, and `ORCA_BASE_URL`
  now moves only the inference origin.
- `codewhale account agents work-quote` and `work-launch` quote and start Work
  on your computer balance as well as the trial. `--seconds` picks whole
  seconds within the served offer; without it `work-launch` uses the default
  its quote showed instead of a fixed 300 seconds, so a balance-funded launch
  can no longer disagree with its quote. `--recovery-from <workspace id>`
  recovers a failed workspace on a fresh computer under the same Work ID.
  `codewhale account agents work` accepts `--context-file` (UTF-8, at most
  64,000 UTF-16 units) and up to ten `--file FILE_ID@VERSION` saved files, so
  Work needs no repository. `account computers create --computer-trial`
  replaces `--boat-trial`, which still works. Experimental, and available only
  to existing accounts while new accounts are paused.
- Goal mode has no step limit when `[goal].max_steps` is omitted or `0`.
  Positive values still limit the goal, with values above `100000` clamped to
  `100000` ([#6512](https://github.com/codewhale-hq/Codewhale/issues/6512)).
- MCP and provider browser sign-in allow 15 minutes for the callback instead
  of five ([#6865](https://github.com/codewhale-hq/Codewhale/issues/6865)).

- `/undo` takes back your last request in one step, in every access mode:
  every file the request changed, plus the request and its reply. It no
  longer asks for `/trust on`. Its report names the request and lists each
  file as `restored`, `brought back` or `removed`, then says whether the
  request and its reply were removed from the conversation. It changes
  nothing if one of those files was edited after the request, and names the
  file. For a request whose end was never recorded it changes nothing and
  points to `/undo force`. `/undo` no longer steps back one tool call at a
  time; `/restore` still reaches those points. When only the conversation is
  rolled back, the message adds "No file changes to undo." or says that
  workspace files were not reverted.
- `/restore` without trust mode or Full Access now says that it rolls every
  file in the folder back to the chosen point, including later edits, that
  nothing was changed, and that `/undo` takes back only your last request.
- `/diff` compares the workspace with this session's first restore point, so
  it works in a folder that is not a git repository and includes files
  created since. Before the session has a restore point it shows the
  workspace repository's uncommitted changes. With neither, it says "Nothing
  to compare yet: this session has not saved a restore point here, and this
  folder is not a git repository." instead of "No changes since session
  start".
- `/trust on` and `/trust off` leave a line in the transcript saying what
  changed; with `--save` the line also says what was saved for the folder.
- After Esc Esc the footer reads "Files not changed. Use /restore.
  Conversation rewound.", replacing "Rewound to previous user message — edit
  and Enter to resend" and the "`/undo` puts them back" promise it used to
  carry. `/restore` rolls the whole folder back to a point you pick and needs
  `/trust on` or Full Access; `/undo` still takes back the last request's
  files where it can prove which snapshots belong to this conversation.
  Refs #6970. Thanks @luestr.
- `/relay` writes the session relay to `.codewhale/handoff.md`, the path a
  new session reads first, instead of `.deepseek/handoff.md`. A relay left at
  the legacy path is still read when the primary file is absent, and the
  prompt block names the file that was actually read.
- The highlighted option on an approval card is tagged `(Enter)`, so the
  default `[3 / d / n] Don't allow (Enter)` row shows what Enter does.
- The edit approval card shows only the lines an edit changes, as `- ` and
  `+ ` rows. Lines shared at the start and end of the old and new text are
  left out, the "replace this" and "with this" sub-labels are gone, and a
  single edit is no longer headed "edit 1". Lines left out on a side are
  counted (`... (+2 more lines)`); several edits are numbered and each shows
  its first changed line.
- The three-row preview of a successful command shows one opening row and two
  closing rows, preferring closing rows that say something passed or failed.
  Blank rows no longer take a slot, and lines hidden after the last row shown
  are counted in the `… (+N lines)` marker too.
- An MCP server the session has not needed yet shows as `not connected` in
  `/mcp` instead of "not started", with its transport and "Not connected in
  this session yet. Connects when a tool is needed, or connect it now." in
  place of zero tool counts.
- `codewhale doctor` adds "works without an API key (limited quota; set
  FIRECRAWL_API_KEY or [search] api_key to raise it)" to the
  `search_provider` line when Firecrawl is the provider and no key is set.
- The stalled-tool notice now ends "Run /jobs to see it and /jobs cancel <id>
  to stop it." instead of naming `exec_shell_cancel`.
- A DeepSeek Harness (`dsh`) package refused with "No portable components in
  this package" now lists the skipped rows, each with its package and reason,
  and the Native composition refusal names the unresolved rows. Both name at
  most five rows and count the rest.
- Full Access runs the calls it grants. The built-in Auto-Review floor
  (publish-like commands, destructive background or headless work, and the
  Windows npm-launcher hold) applies only in Ask, Auto-Review and Never, and no
  longer blocks a call under Full Access, including a detached agent's.
- `file_search` is one of the model's default tools from the first turn
  instead of being loaded through `tool_search`
  ([#6934](https://github.com/codewhale-hq/Codewhale/issues/6934)). It also
  accepts globs (`src/**/*.rs`) and several space-separated terms that must
  all match. Literal matches rank above fuzzy ones, build and dependency
  folders are skipped by default, and `limit` goes up to 200. An `exclude`
  list replaces the default exclusions, and `[]` turns them off.
- `fim_edit` is offered only on routes that can run it: Chat Completions
  routes other than OpenCode Zen. The default DeepSeek Flash route speaks
  Responses and no longer lists it
  ([#6933](https://github.com/codewhale-hq/Codewhale/issues/6933)).
- Runtime session grants (`"remember": true`) are stored in the Runtime data
  directory instead of memory. After a restart a thread's grants stay
  suspended, and matching calls ask again, until an authenticated client
  resumes the thread. A grant store that cannot be read holds no grants, so
  every call asks.

### Fixed

- Delegated work uses the full explicit turn and time limit. Default child nesting and Workflow plan depth caps are removed; explicit owner restrictions remain. The host no longer reserves the last turn or part of the deadline for a forced report, or makes an extra reporting request after the limit; recorded work and files remain available in the partial-result receipt.

- Runtime child completions retain their original parent turn after the parent
  becomes idle or starts a newer turn; terminal receipts remain durable and
  deduplicated.
- Windows Native extension hosts resolve `DeriveCapabilitySidsFromName`
  at runtime and return an unsupported error during LPAC setup when the
  export is unavailable (#6953). Windows 10 1607 startup and Native-host
  acceptance remain unverified.
- `codewhale resume` no longer refuses a session whose saved history still
  holds a raw sub-agent handoff (#6954).
- Snapshot restore and `/undo` work with older git, such as the 2.43 that
  Ubuntu 24.04 ships. They failed there with "only one reference expected".
- When the model cancels its own background job, it no longer gets a
  "finished" event or starts a new turn from it; when you stop a job the
  model started, the model is told it was killed. Cancelling a job that had
  already exited keeps its real exit status instead of reading "killed, exit
  0", and `/jobs cancel all` no longer reports exited jobs as cancelled.
- A shell command that moves to the background after 30 seconds shows the
  same "keep running in /jobs" notice as `Ctrl+B`, and its card no longer
  shows the instructions written for the model.
- `todo_write` returns a short receipt (counts and the item in progress) to
  the model instead of repeating the whole list on every call.
- Goal usage counts the turn that completes, yields or blocks the goal, so
  `tokens_used` and `time_used_seconds` no longer stay at 0. `update_goal`
  accepts a progress estimate with `yield`.
- Waiting on a running shell job reports how long ago it last printed and
  whether it is still compiling or already running tests or the program.
  Stopping a job from `/jobs`, and the completion the model then receives,
  say whether build artifacts already on disk survive the kill (#6944).
- A shell command that has printed nothing for a minute still reads as
  running ("no output for 1m 05s"), keeps its Stop action and is reachable
  through the Runtime API; only a job whose process is gone after a restart
  is stale. `/jobs poll` and `/jobs stdin` no longer consume the output the
  model has not read yet (#6931).
- `/constitution`: `ratify` checks the digest you reviewed in `suggestions`,
  so a file that changed after your review is refused as documented; `preview`
  shows the block exactly as the model receives it; messages no longer claim a
  ratify or migrate moves the prompt-cache prefix; the setup wizard no longer
  lists an authority order that contradicts the base prompt, and it caps your
  own note so saving never cuts its end. The Simplified Chinese manager uses
  the glossary term 宪章 throughout. The manager checks whether your signed-in
  account profile takes precedence and labels an active local file as standby;
  the Runtime API returns the same guidance body from GET and preview. `doctor`
  reports autonomy only from an active local constitution and leaves account
  checks to sign-in. The context inspector shows the personal constitution as
  a recorded session message outside the system-prompt layer.
- The pet shows sub-agents while they run, and a finished one no longer stays
  "working". Half-block pet sprites are drawn crisply instead of blurred.
- A rate-limit 429 with a body retry delay of up to 120 s retries the interrupted
  model request. This includes ambiguous current-quota wording with billing
  guidance when the body supplies a bounded delay; explicit exhausted account,
  billing and subscription limits remain terminal (Refs #6923, thanks @Statter).
- Claude sign-in keeps its PKCE verifier out of the sign-in URL and gives its
  pasted authorization code one terminal input reader. Failed input restores
  the TUI; live subscription sign-in remains experimental (Refs #6932).
- Oversized-session refusals preserve compaction errors and no longer assume
  that the active conversation alone caused the remaining size limit
  (Refs #6955, thanks @7jrxt42BxFZo4iAnN4CX).
- A Codewhale home reached through a symlink or Windows junction keeps saving
  artifacts and compacting (#6947).
- Sub-agent state and transcripts also save through a relocated Codewhale
  home when its junction target already exists. Containment checks use the
  same resolved state root throughout the operation (Refs #6949, thanks
  @SparkofSpike).
- Saving a DeepSeek key in the TUI writes the provider's durable-key marker,
  so its saved credential remains visible after restart while another
  provider is active. Older saves missing that marker need one re-save
  (Refs #6972, thanks @gaord).
- A workflow gate that blocks its own role is rejected when the plan is
  admitted instead of deadlocking the run (#6945).
- Auto-Review gives its guardian an output budget a reasoning model can think
  inside.
- Skills are deduplicated across roots by file, ignored-key warnings are
  grouped, and in-workspace skill links are followed. A session warns once
  when a `[tools] always_load` entry matches no tool, and `codewhale doctor`
  names a bundled skill whose installed text matches no shipped version.
  Bundled skills you have not edited now update to the current version even
  when your Codewhale home is several releases behind; edited ones are still
  left alone. Bundled skills are installed only into Codewhale's own skills
  folder, so a custom `skills_dir` and a legacy `~/.deepseek/skills` folder
  are no longer written to, and `codewhale serve`, the desktop app and other
  Runtime hosts now install them there too, as the TUI does.
- Opening a conversation in a Git repository no longer adds an untracked
  `.codewhale/` to `git status` or the desktop app's Review panel. The
  sub-agent coordination folder `.codewhale/state` now holds a `.gitignore`
  that ignores only that folder. Files you keep elsewhere under
  `.codewhale/` are unaffected, an existing `.gitignore` there is left alone,
  and the file is never written through a link, so a cloned repository cannot
  redirect it to your global git ignore file.
- TUI messages and cards say what happened, once. Provider readiness reads
  "endpoint reachable · model not checked" instead of "models endpoint 2xx",
  since some providers list models for a bad key. A failed `/rc` reads as
  failed, not "not available", including in `/setup`. Copy and cut say
  "Copied to the clipboard", and a failed copy says "Clipboard copy failed:
  …", in every language. The provider-fallback notice leads with the switch
  so it fits at 80–100 columns, and the effort field reads `auto→high`.
  `Alt+V:full error` appears only when an error is cut short. The elevation
  card no longer repeats its heading or a `Tool: exec_shell` row, and the
  checklist card drops its `checklist:` row and shows translated statuses
  (`in progress`, not `in_progress`). A failed read or search names its
  reason, card bodies no longer repeat their header, run summaries say `read
  file`, and MCP calls are no longer folded into a run summary, because their
  side effects are unknown.
- DeepSeek V4/V4.1, `deepseek-chat`, `deepseek-reasoner` and R-series models
  on a custom Chat Completions provider now receive your reasoning effort
  (`low`, `high`, `max`) and thinking setting, as on the direct DeepSeek
  route. Before, both were dropped from every request.
- `grep_files` reports matches relative to the workspace when the workspace
  path goes through a symlink, such as macOS `/tmp` or `/var` or a symlinked
  project folder. Before, every match came back as a full `/private/...`
  path.
- On Android (Termux), workflow journals, compaction checkpoints, saved tool
  output and skills work again. Confined file access opened `/` for reading,
  which Android's SELinux policy refuses; on Linux it now uses path-only
  handles that need no read access, with the symlink and escape checks
  unchanged. Compiled Android builds and on-device use are not yet
  qualified. Thanks @printf033 (#6980, #6983).
- Runtime API servers that use different stores on one machine no longer
  refuse each other with "authenticated Runtime owner belongs to another
  selected store". On macOS and Linux a store with its own runtime directory
  gets its own control socket; the default store keeps its usual path. A
  second Runtime API server that tries to drive a workspace another one
  already drives is refused and told which process holds it. Watch-only
  clients are not limited. Windows keeps its current transport, and the
  standalone TUI and CLI do not take this workspace lease yet. Thanks @gaord
  (#6924).
- In Spanish (Latin America), Japanese, Brazilian Portuguese, Vietnamese and
  Simplified Chinese, `/provider`'s description now matches the English
  "Switch provider or model" instead of listing only deepseek, nvidia-nim and
  ollama. Thanks @Lstarsky0 (#6922).
- MCP servers reconnect when a session sync drops the eager connection pool.
- The shared pet owner exits after ten minutes with no client.
- Tool headers keep their clipped part visible at narrow widths, `/jobs`
  prints plain exit codes and a three-line tail, and a collapsed tool output
  shows a single hidden row instead of a "1 line omitted" marker.
- The shared pet pauses its world and periodic checkpoint writes after its
  viewers, producer and audio leases expire. Authenticated clients wake it
  through the existing request queue; its active clock waits for the next
  frame instead of polling every two milliseconds (Refs #6728, #6155).

- `Ctrl+B` and steering input release foreground and background shell waits
  without stopping their commands. Requests apply only to active waits in the
  current session, including waits on several tasks; a later wait starts fresh
  ([#6909](https://github.com/codewhale-hq/Codewhale/issues/6909)).
- Concurrent starts and reconnects for one workspace converge on one Engine
  owner. A receipt retired during a reconnect is observed again before
  attaching; files with unsafe ownership, permissions or multiple links are
  still refused.
- Local plugin updates retain a private recovery backup while replacing the
  installed version. Plugin actions marked as requiring approval keep that
  requirement in Full Access, and internal installation paths are excluded
  from discovery.
- Concurrent Files API creates publish a workspace file exclusively. A
  competing creator gets HTTP 409 and cannot overwrite the winning file.
  Within one Runtime, writes to an existing file are applied one at a time,
  so two clients sending the same `expected_revision` can no longer both
  succeed. Writes from another process or a shell are not covered.
- Cache usage accounts for cache-write tokens as well as cache hits when
  deriving cache misses. Missing cache details remain unknown, and explicit
  zero counts stay zero
  ([#6913](https://github.com/codewhale-hq/Codewhale/pull/6913)).
- `/profile` usage, switching, success and failure replies follow the UI
  language across all 15 complete locale packs
  ([#6919](https://github.com/codewhale-hq/Codewhale/pull/6919)).
- On Windows npm installs, the model's environment identifies `node.exe` as
  the launcher and warns that killing it by name also stops npm-launched
  Codewhale sessions. It points to stopping servers by PID or port instead
  ([#6906](https://github.com/codewhale-hq/Codewhale/pull/6906)).

- Persistent terminal commands keep interactive stdin available while preserving
  current-shell variables, directories and aliases. Human answers no longer
  consume the command's completion marker. Oversized raw input batches are
  refused before writing in canonical line-input mode, across the Terminal
  dock, `terminal/send` and the Runtime API.

- A stream request that receives no response headers in time now adds that
  the provider may still be loading the model, and names `codewhale config
  set stream.open_timeout_secs 180` (up to 300). `codewhale doctor
  --probe-api` reports a live check that ran out of time as "Not confirmed"
  with "API check got no answer in time", not as a failed credential, and no
  longer points at replacing the key
  ([#6889](https://github.com/codewhale-hq/Codewhale/issues/6889), thanks
  @BX166).
- When the same model fails with the same upstream HTTP status on two or more
  requests in a row, the error adds how many times it has failed, that a
  provider can list a model that is not serving requests, and to choose
  another model with `/model`. The count starts over when a request opens its
  stream, fails another way, or uses a different model or status
  ([#6889](https://github.com/codewhale-hq/Codewhale/issues/6889), thanks
  @BX166).
- A Chat Completions response that reports `stop` while its reported
  completion tokens equal the output ceiling the request asked for is treated
  as cut at the output limit. A partial answer is kept and continued; an
  empty one fails after that single request instead of being asked for again.
  The "Model reached the response output limit with no answer or tool call"
  error no longer depends on the provider having streamed reasoning
  ([#6889](https://github.com/codewhale-hq/Codewhale/issues/6889), thanks
  @BX166).
- A shell command with a descriptor redirect such as `npm test 2>&1` or
  `cargo build > /dev/null 2>&1` is no longer classified as a destructive
  action: its approval card reads "Runs a command" without "Can't be
  undone". `rm -rf /etc 2>&1` is still held as destructive, and a command
  after a bare `&` (`echo x&rm -rf /etc`) is still checked.
- A plugin command or skill typed on the startup screen resolves for the
  session that submit begins, instead of reading as an unknown command. The
  submit waits up to 2 seconds for the engine to install the new session.
- A session saved in Operate by v0.10.0 and resumed on this build has that
  release's internal Operate instructions recognized as a runtime message
  rather than as something you typed. Compaction keeps only the current
  instructions, so the two no longer disagree about who creates the goal.
- The `agent` tool's rejection of an invalid `thinking` value lists `xhigh`
  and `ultra`, the same values its schema offers. Its description now says
  that in Operate a child the main session starts edits files and runs the
  Run tool's built-in checks without a second approval, and that any other
  shell command follows the session's approval settings.
- A long line of tool output wraps between words instead of mid-word, and the
  first row, which sits after the `result:` or `output:` label, is no longer
  wrapped a second time when drawn.
- A running task's unsaved progress is written to its task record on the
  deadline set by its first unsaved event. Tool heartbeats and store polls no
  longer restart that wait, and heartbeats are kept in memory only.

- On Windows, when the app-server that owned a Runtime has died, the next
  start now replaces it automatically, as on macOS and Linux. A live owner,
  a reused process ID, or a publication from another user, store or config
  is still refused
  ([#6915](https://github.com/codewhale-hq/Codewhale/issues/6915)).
- `/network allow <host>` and `/network deny <host>` take effect on the next
  tool call without a restart
  ([#6928](https://github.com/codewhale-hq/Codewhale/pull/6928)). Under a
  managed `[network]` overlay or a Fleet `network_access = false`, editing your
  own `[network]` table cannot widen the policy: a denial from either layer
  stands, a deny-by-default authority keeps its own allow list, proxy and
  fake-IP exceptions, a prompt-by-default authority adds your hosts to its own,
  and audit logging stays on if either layer asks for it.
- A workflow's final receipt is no longer dropped when its completion inbox is
  full, and each agent's queue of messages to its parent holds at most 64; a
  send past that returns an error instead of growing without bound
  ([#6560](https://github.com/codewhale-hq/Codewhale/issues/6560)).
- `cargo +<toolchain> build`, `scripts/dev-test.sh` and `scripts/dev-cargo.sh`,
  also through a wrapper, count as heavy commands for resource admission
  ([#6560](https://github.com/codewhale-hq/Codewhale/issues/6560)).
- Sub-agent state is written atomically, with the file and its directory
  flushed before the rename
  ([#6555](https://github.com/codewhale-hq/Codewhale/issues/6555)), and image
  search reads its seed and result bodies through the size cap
  ([#6554](https://github.com/codewhale-hq/Codewhale/issues/6554)).

### Known limits

- From a pre-release security review of the local network surfaces, not yet
  fixed: `codewhale serve --insecure` and `codewhale app-server
  --insecure-no-auth` do not check the `Origin` header, so with either flag
  a page on an allowed development origin (`localhost:3000`, `:1420`) can
  drive the Runtime API; use them only on a machine you trust. The one-time
  `codewhale web` and `--mobile` bootstrap capability passes through the
  browser launcher's arguments, where another local OS user can read it. The
  opt-in control socket (`[control_socket] enabled = true`) does not check
  the connecting user and can be reached from sandboxed commands.
- The sandbox deny-list for the Runtime daemon socket covers its default
  locations as this process sees them. It does not cover a daemon started
  with `--socket-path` or with a different `XDG_RUNTIME_DIR` or
  `CODEWHALE_HOME`; a run directory created after a long-running sandboxed
  command started is not hidden from that command; and on macOS, when the
  workspace is your home directory or another folder above the daemon's
  directory, a sandboxed command can still move that directory out of the
  denied path.
- Ambiguous current-quota 429s without a body retry delay of at most 120 s
  still end the turn; a header alone does not change that classification.
  Explicit exhausted account, billing and subscription limits remain terminal
  regardless of a retry hint. Gemini limits without an accepted body delay are
  not automatically retried
  ([#6923](https://github.com/codewhale-hq/Codewhale/issues/6923)).
- Idle CPU has not been measured on FreeBSD or Linux for this release
  ([#6728](https://github.com/codewhale-hq/Codewhale/issues/6728)).
- Shell commands run by delegated agents appear in `/jobs` and the work dock
  with their owner, not in the agent's own detail view; named terminal
  sessions live in the terminal dock
  ([#6931](https://github.com/codewhale-hq/Codewhale/issues/6931)).
- The pet view chosen with `Ctrl+]` is not saved across restarts and cannot
  be changed inside the `F5` inspector. The browser studio and the macOS
  window are not views in the cycle, and `Ctrl+]` cannot open the work bar
  while the pet main view is showing.
- Computer Use is a preview. Its observation diffs, fresh element targets and
  named action batches were checked against a macOS fixture app only;
  packaged installs, installed-model use and sharing the desktop with a person
  working at the same time have not been qualified.
- The `GET /v1/approvals` `target` and `summary` redaction is heuristic: a
  secret passed as a plain word argument is not recognized, so treat approval
  history as sensitive.

### Maintenance

- 0.10.1 never reached crates.io because the `codewhale-tui` package was over
  the 10 MiB upload limit, so `cargo install codewhale-cli` kept installing
  0.10.0. The package now leaves out test sources, and the release checks
  every package's size before the first upload.
- The README and its 18 translations were rewritten in plain language and
  corrected where they described behaviour wrongly: in Ask, file edits at a
  Git repository's root apply without a prompt, and Ask asks before shell
  commands that are not read-only; `a` at an approval allows similar commands
  for the rest of the conversation; Full Access turns Codewhale's sandbox off;
  and only `AGENTS.md` loads by default, while `CLAUDE.md` and similar files
  need `project_instruction_imports`. The README no longer suggests keeping
  provider keys in a Codewhale account. CONFIGURATION.md and PROVIDERS.md say
  a `[providers.<name>]` table can set `wire = "responses"` or `"anthropic"`
  (Refs #6997). Documentation only.
- The documentation no longer steers new users to Codewhale accounts, account
  key storage or cloud dispatch, which are paused for new users, and the user
  guide says `codewhale dispatch` is not available. The GitHub Action guide
  opens with a bring-your-own-key DeepSeek example. `docs/PROVIDERS.md`
  explains DeepSeek V4.1 Flash (`deepseek-flash`) and no longer says the
  Ollama default is `deepseek-v4-flash`, and the remaining docs say Work
  instead of Act. Documentation only. Refs #6486, #6466.
- Main is mirrored to CNB on every push again, and to GitCode once its token
  is configured.
- Contributor pull requests get fast checks again: formatting, lint, the
  Linux test suite, a Windows build, the npm wrapper smoke and the existing
  issue-link and credit checks. They can merge as themselves without an
  administrator override. Pushes to `main` start no GitHub build or test
  workflow. The manual CI dispatch on a release commit now also runs
  `cargo-deny`, the cargo and npm audits and the OpenHarmony check.
  CONTRIBUTING.md explains each check and what maintainers do with a pull
  request.
- SANDBOX.md and CONFIGURATION.md, in English and Chinese, now state that
  `sandbox_mode` can only narrow the sandbox the permission mode already
  allows, so `danger-full-access` and `external-sandbox` take effect only in
  Full Access (`yolo`). Documentation only; no behavior change. Refs #6992.
- CI and test harness only, no change to the shipped binaries. The
  persistence backlog budget takes five samples and judges enqueue time on the
  fastest one. Linux CI jobs install apt packages through
  `scripts/ci-apt-install.sh`, which bounds every wait and drops an
  unreachable Azure mirror.

### Contributors

- **[Dhruv Singal / @dsingal0](https://github.com/dsingal0)** — added Herdr agent state, session resume commands and pane release ([#7064](https://github.com/codewhale-hq/Codewhale/pull/7064)).
- **[@shenjackyuanjie](https://github.com/shenjackyuanjie)** — diagnosed and fixed the Windows OpenHarmony CMake generator and CLI hashing-module build failures ([#6988](https://github.com/codewhale-hq/Codewhale/issues/6988), [#6989](https://github.com/codewhale-hq/Codewhale/pull/6989)).
- **[@printf033](https://github.com/printf033)** — replaced the SELinux-denied root-directory anchor of confined-file I/O with an `O_PATH` walk (Linux-cfg-gated) and a re-materialized final parent, restoring workflow journal, compaction checkpoint and artifact writes on Android/Termux ([#6983](https://github.com/codewhale-hq/Codewhale/pull/6983)); reported the Android confined-I/O failure with strace evidence and an on-device reproducer ([#6980](https://github.com/codewhale-hq/Codewhale/issues/6980)). Source change; compiled Android and device acceptance remain unqualified.
- **[@aboimpinto](https://github.com/aboimpinto)** — moved config-command parsing and replies onto portable contracts while the host retains effects ([#6973](https://github.com/codewhale-hq/Codewhale/pull/6973)).
- **[@henrikdukefoss](https://github.com/henrikdukefoss)** — added the Standard Compute compatible-host descriptor, with live model discovery and its billing basis kept Unknown ([#6985](https://github.com/codewhale-hq/Codewhale/pull/6985)).
- **[@LIghtJUNction](https://github.com/LIghtJUNction)** — plugin shell install and in-TUI plugin OAuth sign-in ([#6950](https://github.com/codewhale-hq/Codewhale/pull/6950)).
- **[@SparkofSpike](https://github.com/SparkofSpike)** — live network-policy updates and goal milestone hand-back ([#6928](https://github.com/codewhale-hq/Codewhale/pull/6928), [#6930](https://github.com/codewhale-hq/Codewhale/pull/6930)), Windows ampersand classification ([#6929](https://github.com/codewhale-hq/Codewhale/pull/6929)), and a relocated Codewhale home including sub-agent state ([#6947](https://github.com/codewhale-hq/Codewhale/issues/6947), [#6949](https://github.com/codewhale-hq/Codewhale/pull/6949)). Also removed repeated catalog construction and credential-readiness work from the model picker ([#7026](https://github.com/codewhale-hq/Codewhale/pull/7026)), listed what still blocks a session switch ([#6948](https://github.com/codewhale-hq/Codewhale/pull/6948)), and fixed custom relay model discovery ([#7013](https://github.com/codewhale-hq/Codewhale/pull/7013)).
- **[@dajiaohuang](https://github.com/dajiaohuang)** — cache-write token accounting ([#6913](https://github.com/codewhale-hq/Codewhale/pull/6913)).
- **[@gaord](https://github.com/gaord)** — embedder telemetry surface support ([#6916](https://github.com/codewhale-hq/Codewhale/pull/6916)) and the DeepSeek saved-key marker diagnosis and fix ([#6972](https://github.com/codewhale-hq/Codewhale/issues/6972), [#6974](https://github.com/codewhale-hq/Codewhale/pull/6974)). Also gave each Runtime store its own control socket and one driver per workspace ([#6924](https://github.com/codewhale-hq/Codewhale/pull/6924)), and added reading and writing a workspace's project trust through the Runtime API ([#6959](https://github.com/codewhale-hq/Codewhale/pull/6959)).
- **[@Lstarsky0](https://github.com/Lstarsky0)** — localized profile replies ([#6919](https://github.com/codewhale-hq/Codewhale/pull/6919)), workflow gates that block their own role ([#6952](https://github.com/codewhale-hq/Codewhale/pull/6952)), proxy-independent MCP loopback checks ([#6971](https://github.com/codewhale-hq/Codewhale/pull/6971), [#6975](https://github.com/codewhale-hq/Codewhale/pull/6975)), durable steering/child-approval acceptance checks ([#6967](https://github.com/codewhale-hq/Codewhale/pull/6967), [#6966](https://github.com/codewhale-hq/Codewhale/pull/6966)), and an app-server hook log that no longer lands in the project directory ([#6921](https://github.com/codewhale-hq/Codewhale/pull/6921)). Also made feedback truncation readable in ASCII-safe mode, added a statusline scroll rail, and removed duplicate clipped descriptions from split-view hotbar setup ([#7027](https://github.com/codewhale-hq/Codewhale/pull/7027), [#7028](https://github.com/codewhale-hq/Codewhale/pull/7028), [#7029](https://github.com/codewhale-hq/Codewhale/pull/7029)). Kept nested list indentation in rendered markdown, kept a to-do row's status word when its title is long, and put the approval footer's separators between its hints ([#7030](https://github.com/codewhale-hq/Codewhale/pull/7030), [#7033](https://github.com/codewhale-hq/Codewhale/pull/7033), [#7034](https://github.com/codewhale-hq/Codewhale/pull/7034)). Also lined up the provider lists' status column ([#6984](https://github.com/codewhale-hq/Codewhale/pull/6984)), brought the `/provider` description in five language packs up to date ([#6922](https://github.com/codewhale-hq/Codewhale/pull/6922)), said Work in the `/statusline` Mode hint ([#7012](https://github.com/codewhale-hq/Codewhale/pull/7012)) and fitted more `/statusline` items on short terminals ([#7025](https://github.com/codewhale-hq/Codewhale/pull/7025)).
- **[@jayanthvee](https://github.com/jayanthvee)** — Windows npm launcher guidance ([#6906](https://github.com/codewhale-hq/Codewhale/pull/6906)).
- **[@asto18089](https://github.com/asto18089)** — callback-window reference fix for longer browser sign-in waits ([#6865](https://github.com/codewhale-hq/Codewhale/issues/6865)). Also restricted the CNB mirror to the project owner so forks skip it while missing upstream credentials still fail explicitly ([#7016](https://github.com/codewhale-hq/Codewhale/pull/7016), with initial absent-secret handling co-authored by [@zhuowp](https://github.com/zhuowp)).
- **[@BX166](https://github.com/BX166)** — reported three provider failure modes measured on AICraft's own traffic: a cold model that reads as a dead connection, a listed model that fails on every call, and a response cut at the output ceiling that reports `stop` ([#6889](https://github.com/codewhale-hq/Codewhale/issues/6889)).
- **[@harryvizcaino-oss](https://github.com/harryvizcaino-oss)** — reported the invalid Full Access configuration guidance shown to the Paseo ACP client ([#6992](https://github.com/codewhale-hq/Codewhale/issues/6992)).
- **[@rafacouto](https://github.com/rafacouto)** — reported Linux sandbox mount failures for symlinked credential directories and supplied a standalone bubblewrap reproduction ([#7007](https://github.com/codewhale-hq/Codewhale/issues/7007)).
- **[@leoca](https://github.com/leoca)** — fixed the command splitter behind the destructive-command checks so a backslash-escaped quote outside quotes is an ordinary character, as the shell treats it (commit 82a14ee2d1).
- **[@luestr](https://github.com/luestr)** — reported refusals when starting a new session ([#6981](https://github.com/codewhale-hq/Codewhale/issues/6981)) and the `/undo` notice that reported files reverted when only the conversation had been rewound ([#6970](https://github.com/codewhale-hq/Codewhale/issues/6970)), and a session that could not be restored after its provider was deleted ([#6996](https://github.com/codewhale-hq/Codewhale/issues/6996)).
- **[@zhuowp](https://github.com/zhuowp)** — found that the Windows shell's Python UTF-8 default was discarded whenever hook variables were applied, and fixed the merge so it survives while explicit overrides still win ([#7041](https://github.com/codewhale-hq/Codewhale/pull/7041)), and documented the JSONPath subset the `Web` tool's `fields` option accepts ([#7046](https://github.com/codewhale-hq/Codewhale/pull/7046)).
- **[@Asteria1028](https://github.com/Asteria1028)** — asked for `/init` to write `AGENTS.md` in the interface language ([#6964](https://github.com/codewhale-hq/Codewhale/issues/6964)).
- **[@SakuraNeko](https://github.com/SakuraNeko)** — asked for HarmonyOS PC support, which shaped the HarmonyOS install guidance ([#1072](https://github.com/codewhale-hq/Codewhale/issues/1072)).
- **[@8sero](https://github.com/8sero)** — reported the TUI crash in non-Latin-script sessions, with the panic signature that located the byte-index slice ([#7048](https://github.com/codewhale-hq/Codewhale/issues/7048)).
- **[@SergiuSSoluS](https://github.com/SergiuSSoluS)** — reported queued turns being delivered again after a session was restored ([#6978](https://github.com/codewhale-hq/Codewhale/issues/6978)).
- **[@Statter](https://github.com/Statter)** — reported rate-limit retry behavior that informed the bounded current-quota 429 retry fix ([#6923](https://github.com/codewhale-hq/Codewhale/issues/6923)), and black-on-black body text on terminals that do not report their background ([#7043](https://github.com/codewhale-hq/Codewhale/issues/7043)).
- **[@7jrxt42BxFZo4iAnN4CX](https://github.com/7jrxt42BxFZo4iAnN4CX)** — reported oversized-session refusals that informed compaction error preservation and size-limit attribution ([#6955](https://github.com/codewhale-hq/Codewhale/issues/6955)), SSE retries capped by the stream-open window ([#7045](https://github.com/codewhale-hq/Codewhale/issues/7045)), and a second session that could not be resumed in a busy workspace ([#7042](https://github.com/codewhale-hq/Codewhale/issues/7042)).

## [0.10.1] - 2026-10-07

### Contributor integration and reliability

- OAuth retry diagnostics omit provider-controlled error fields; account replacement stops before retired Weixin receipts exceed their storage limit, preserving uncertain work for local review.
- Resuming a session through a symlink to the same workspace no longer shows a false workspace-change warning.
- Runtime clients can read one tool call's actual workspace changes and reviewed skill details (thanks @gaord, #6817 and #6869). In-flight snapshot pairs remain pending; missing objects and corrupt repository metadata are distinguished.
- Search accepts valid preferred locales, and image dimensions describe the same bytes sent to the model (thanks @asto18089, #6860 and #6858). Automation deletion keeps its definition until cleanup succeeds, and compaction preserves its original summary anchor (#6864 and #6857).
- Config/status/permission commands share portable contracts while the host retains mutation authority; queue workers acknowledge a scheduled retry for temporary first-claim contention and fail honestly on corruption (thanks @aboimpinto, #6832).
- Indefinite questions, approvals and elevation waits survive the TUI watchdog. Answers get time to resume the current turn; settled requests disappear by identity. Thanks @7jrxt42BxFZo4iAnN4CX for #6872.
- Configured approval expiry belongs to the held Engine request; hiding or covering its card cannot restart the deadline, and a late queued answer cannot approve an expired call.
- Tool discovery keeps the highest-ranked matches when a result batch exceeds the existing cache bounds, preserving search order and the 16 KiB limit (adapted from @AdityaVG13's #6393).
- Model-switch receipts now translate their session-only saving note in every complete locale pack; the three save commands remain directly usable (thanks @Lstarsky0, #6875).
- Route-save receipts and `/workspace` replies are translated in every complete locale pack, the Operate descriptions in fourteen packs match the current English, pt-BR, es-419 and ca regain accents six strings had lost, and long CJK text can be shortened between Han and kana characters (thanks @Lstarsky0, #6884, #6885, #6886, #6888, #6887 and #6882).
- Long sessions keep less in memory: superseded journal entries beyond a retained window move out of the live session into a per-session archive under `sessions/.journal-archive/` (kept until the session is deleted; `/branch` can still restore an archived entry), ended shell operations in the work graph are capped, and session metadata larger than the first read no longer forces a full-file read. Not yet addressed: usage fingerprints in session metadata still grow without a bound, snapshots are still cloned on the UI task, and resume still reads the session file twice (thanks @7jrxt42BxFZo4iAnN4CX, refs #6842).
- Weixin bridge threads belong to the account and chat that created them:
  resuming or listing another chat's thread is refused. An explicit `/new` after
  an account replacement keeps the old account's private receipt and never
  replays its prompts automatically. Replies never reuse another bot account's
  Weixin context token, and `/threads` lists this chat's own older threads even
  when other chats have newer ones.
- Windows messaging bridges retry a briefly locked record replacement
  (EPERM/EACCES/EBUSY) with bounded delays and never delete the previous record
  first.
- The website and README installation guides are generated from one shared
  source, and the critical Simplified Chinese guides were re-reviewed against
  current English.
- A Codewhale sign-in that expired, or an account service that is unreachable,
  no longer blocks turns on your own provider key. The turn uses your local
  settings and Codewhale says once how to restore account preferences.
- `codewhale login` keeps a DeepSeek route you chose, or one with a local key,
  instead of switching it to the managed Codewhale provider.
- The bundled `computer-use` plugin is 0.12.1, reconciled with canonical source
  `724f9c422db1a51880dc81154dae70816c475ed8` while retaining Core's embedding
  manifest and version contract. Its image renderer lock now carries Sharp 0.35.5.
- The bundled first-party catalog pins marketplace revision
  `6512f1dfaa91ee287e9f81ebabaf4909e8a371a3` and lists all 19 reviewed plugins,
  up from 6. Every catalog plugin still installs disabled and untrusted until
  you review it.
- Every committed npm lockfile is audited in CI, including build tooling. The
  VS Code extension packages with `@vscode/vsce` 4 on Node 22 while it still
  compiles and tests on its Node 20 runtime. The website keeps one reviewed,
  hash-verified depth guard for an unpatched `braces` advisory
  (GHSA-vfj7-8cjw-p6xm); its raw audit findings are retained, not hidden.
- Pasting through Windows Terminal (Ctrl+Shift+V) no longer drops emoji and
  other characters outside the Basic Multilingual Plane. Release binaries and source builds carry
  a small patch to crossterm 0.29.0 in `patches/` (the change proposed upstream
  as crossterm-rs/crossterm#1073); a real Windows Terminal paste of 24 Unicode
  lines is now a release gate.
- Experimental extension host: reviewed Native plugins can register avatar
  packs with `ctx.avatars.registerPack` (`/pet avatar [key]`, `/pet action|view
  <name>`). `/plugin import dsh <package-dir>` reviews a DeepSeek Harness
  bundle and now lists the Native host code it contains; `approve <dir>
  <content-hash>` installs it disabled and untrusted. Trusted host plugins stay
  trusted across a restart, enabling a second host plugin activates it without
  `/plugin reload`, and the review no longer crashes the TUI.

Codewhale v0.10.1 focuses on reliability and first-run behavior. Turns that
stall now say so, approvals keep what you approved, plugin suggestions are
quieter, and Fleet runs can be checked before they spend anything.
Scripts that parse `--json` output should read the **Breaking for scripts**
note below before upgrading.

### Added

- `/plugin doctor` reports stale built-in records and snapshots, and
  `/plugin doctor --fix` retires them. A user plugin, a snapshot a running
  process names, and a snapshot inside the grace window are kept. The
  previous `state.json` is kept as `state.json.pre-gc`.
- Reviewed plugins can declare named OpenAI-compatible OAuth routes. The host
  owns PKCE, refresh and credential storage, and checks the review at each
  request ([docs/PLUGIN_PROVIDERS.md](docs/PLUGIN_PROVIDERS.md), #6805).
  The provider capability advances plugin review policy to v5 (v6 with the
  extension host): older receipts require explicit review again.
- OrcaRouter account sign-in uses PKCE and saves the same durable API key as
  manual setup; its live catalog keeps chat-capable rows and stated pricing
  and modality facts (#6867).

- Experimental TypeScript extension host. New in this release and off by
  default: turn it on with `[features] extension_host = true`. A plugin that
  declares a `native` TypeScript or JavaScript entry (the Cordis / DeepSeek
  Harness plugin model) can contribute tools, slash commands, pre-execution
  policy hooks, additive prompt sections, reviewed skill roots and plugin-local
  JSON state. Its code runs in a separate
  Node process, never inside Codewhale (Bun is an opt-in through
  `[extension_host] runtime`), and every call goes through Codewhale's own gate:
  an extension tool is never treated as read-only, so it always meets the
  approval gate (Full Access, Bypass or a session grant for that reviewed plugin
  can satisfy it), a command runs only when you type it, and a plugin must be
  reviewed and enabled first. The host is sandboxed on macOS (Seatbelt) and on
  Linux with bubblewrap, with no direct network and no reads of the protected
  credential locations (Codewhale's secret stores and the Codewhale, Codex and
  DSH homes). A Native extension starts only after that sandbox is verified at
  launch: if bubblewrap is missing or its namespace probe fails, activation is
  refused with the concrete error. Codewhale's own pinned built-in host modules
  keep a diagnosed exception, and every effect still needs a Rust operation
  ticket. On Windows a Native extension starts only in a freshly created Less
  Privileged AppContainer whose only capability is `registryRead` (Winsock
  cannot start without it), after Codewhale checks its token and a real file
  and network probe; otherwise activation is refused.
  Other files you
  can read, such as project `.env` files, stay readable, and plugins sharing the
  host can interfere with each other, so enable only plugins you have reviewed
  ([docs/EXTENSIONS.md](docs/EXTENSIONS.md)).
- Extension slash commands: a plugin registers one with `ctx.commands.register`.
  It runs only when you type it, and it can show text, or submit a prompt as
  your next message that then goes through the ordinary turn and its approvals;
  it cannot call the model or a tool itself. A command cannot take a built-in
  command's name or another plugin's, and a markdown command with the same name
  wins. A command has 30 seconds, and commands are TUI only: the Runtime API
  does not list or run them.
- Native extension authoring now includes policy listeners through
  `ctx.on('tools/pre-execute', ...)`, prompt sections through
  `ctx.prompt.registerSection`, and persistent plugin-local state through
  `ctx.storage`. Listeners may deny, ask, revise input or annotate; Rust
  rechecks revised calls and owns every approval. Prompt sections cannot
  replace the system prompt, and state exposes no session history or secret
  API. These services share the experimental, off-by-default host's reviewed
  owner lifecycle and are withdrawn when their owner unloads or is revoked;
  saved state remains available when the plugin reloads.
- Extension tool input is checked against the JSON Schema the tool registered,
  before an approval card appears and again before the call reaches the host. An
  invalid call comes back to the model as an error naming what to correct, and
  the host never sees it. A schema that cannot be compiled, or that refers
  outside itself with `$ref`, is refused when the tool registers.
- An extension tool can call eligible core tools with `core/call` while the
  ordinary turn is running that extension tool. Rust supplies a temporary
  invocation ticket and still owns planning, hooks, permissions, approval and
  execution. The ticket is bound to the plugin owner, host process and live
  invocation, and expires when that invocation ends or is revoked. Commands,
  timers and plugin startup cannot use this path. Approval grants are scoped
  to the reviewed extension build; a grant to the model does not cover it.
  Shell and network calls require a person's prompt even with a remembered
  grant; a permission mode that cannot present that prompt refuses the call.
  MCP tools, Computer Use, other extensions, agent/workflow launches and
  permission-changing tools remain unavailable through this path. A cancelled
  invocation withdraws its pending approvals instead of deciding them for you.
- Extension host identity is checked at startup: its reported trust tier and
  built-in module digests must match the process Rust launched. Built-in host
  code and third-party plugins have separate process and owner namespaces;
  the table pins the MCP protocol module and the execution harness. The
  optional Host MCP backend uses the official TypeScript SDK for framing,
  with Rust retaining credentials, network/process access, approval and exact
  operation tickets. All 30 existing recorded comparisons pass locally.
  The Rust backend stays the default until platform and rollout gates pass;
  its protocol adapters remain through the compatibility window.
- A plugin can declare several `native` entries (`native.paths`, up to 64). They
  activate in order under one owner, so one disable, review change or crash
  takes all of them down. If one entry fails to activate, only that entry is
  withdrawn: entries that already activated stay registered, and the plugin
  fails only when no entry activates.
- Plugin settings and context: `[plugins."<name>".config]` in your own
  `config.toml` is passed to the plugin's `apply(ctx, config)` and checked
  against its exported `Config` schema; a project's `.codewhale/config.toml`
  cannot set it, a change applies at `/plugin reload`, and `/plugin show` lists
  the keys, never the values. Tools and commands also receive the workspace of
  the session that called them and a private data directory for the plugin under
  `~/.codewhale/extension-host/data/plugins/`; no home, credential or other
  workspace path is passed.
- `codewhale --enable <feature>` and `--disable <feature>` work from the
  `codewhale` command; before, they were rejected. Put the flag before any
  subcommand.
- The extension host bundle carries MIT-licensed code (Cordis, Schemastery and
  others). Its third-party licence notices are generated from what the bundle
  actually contains and written beside the bundle when it is unpacked, and
  `THIRD_PARTY_NOTICES.md` lists the packages.
- Extension host runtime: Bun 1.4.0 or newer is an opt-in; Node stays the default. `[extension_host] runtime = "node" | "bun" | "auto"` chooses: unset means `node` (or `bun` when the table sets only a `bun` path), `bun` runs only Bun, and `auto` prefers a supported Bun and uses Node when none is found or the Bun host fails to start. An explicit choice never falls back, and restarts keep the runtime the session first started on; a runtime binary replaced mid-session is refused. A configured `node` or `bun` path is used as given and fails loudly rather than falling through to `PATH`, and a runtime found on `PATH` inside a `node_modules` directory or the working directory is skipped, not run. `codewhale doctor` and `/plugin` show which runtime runs the host, why, and how its memory is capped. Under Bun the host never auto-installs packages and ignores `.env` and `bunfig.toml` in its data directory. Extensions cannot run native code inside the host on either runtime: `bun:ffi`, `Bun.FFI`, `node:ffi`, the SQLite builtins, `process.dlopen`, `process.execve`, Worker threads and `ShadowRealm` are refused, and the host will not start if one of these restrictions does not hold. Known limit: that lockdown covers the native-code entry points found so far (Bun 1.4, Node 22 and 26); one a newer runtime adds is not covered until it is added. The 1 GiB memory cap uses `RLIMIT_DATA` on Linux, the Job Object on Windows and, for a Bun host, a kernel jetsam limit on macOS; a Node host on macOS is checked at each heartbeat. Bun is qualified on macOS only: the Rust host tests, the memory cap included, have not run a Bun host on Linux or Windows (CI runs them with Node, and the JS host suites under Bun 1.4.0 on Linux). `host/hello` reports the runtime and version that actually run.
- `docs/features.toml` lists every user feature with its status, first
  release, docs page and owning code. A test fails when a `[features]` flag
  and its row disagree, or when a listed docs page or code path is missing.
  The configuration reference now lists the `verify_tool`, `vision_model` and
  `extension_host` flags, and `config.example.toml` lists `code_mode`.
- A reusable GitHub Action runs PR reviews with a configurable model endpoint
  and a checksum-pinned Codewhale binary. It checks the PR's eligibility and
  exact revisions before inference; fork events receive no model key
  ([#6780](https://github.com/codewhale-hq/Codewhale/pull/6780)).
- An optional, off-by-default shadow Decision Gate classifies the first request
  of a turn — tool need, answerability from context, intent — through the
  existing System One client and only logs a typed recommendation; it never
  changes routing or delays the model call. Configure it with
  `SUPERFAST_ENABLED` and `SUPERFAST_PROVIDER` (docs/CONFIGURATION.md)
  ([#6603](https://github.com/codewhale-hq/Codewhale/issues/6603),
  [#6604](https://github.com/codewhale-hq/Codewhale/pull/6604), thanks @Andrea-Bruno).
- `tool_call_after` hooks for shell tools receive `DEEPSEEK_TOOL_EXECUTION_RECEIPT`: the command that actually ran after admission, its working directory, how it ended, and bounded stdout/stderr previews, so a hook can record exactly what executed ([#6689](https://github.com/codewhale-hq/Codewhale/issues/6689), requested by [@wuisabel-gif](https://github.com/wuisabel-gif)).
  Supported settled local shell calls also send a versioned JSON document on
  stdin to foreground or background observers, with session/tool-call IDs,
  nullable exit status and output truncation flags. Existing environment fields
  stay available; unsupported execution paths send no document, and observer
  output cannot change the completed call
  ([#6582](https://github.com/codewhale-hq/Codewhale/issues/6582)).
- Runtime API: turns now record what they produced. Each item and turn
  carries typed artifact references (path, kind, size, revision, and a
  restore point when file-revert would accept one) for files a tool wrote,
  spilled tool output, and media.
  - Once the post-turn snapshot settles, the turn also lists what changed in
    the workspace while it ran, including shell and sub-agent writes. It then
    publishes `turn.artifacts`.
  - `GET /v1/threads/{id}/turns/{turn_id}/artifacts` lists a turn's
    references, and `.../artifacts/{artifact_id}` reads one from the
    workspace, the post-turn snapshot or the session artifact directory.
  - Spills from unbound runtime threads were unreadable before; they are now
    readable.
  - The legacy `artifact_refs` field is now filled with the workspace files a
    tool call wrote, so Preview in current desktop builds shows them
  ([#6653](https://github.com/codewhale-hq/Codewhale/issues/6653)).
- Runtime API: git stage, unstage, discard and commit accept optional
  `expect` preconditions (full HEAD id, an index token, per-file `rev`, or a
  whole-tree `revision`, all read from `GET /v1/git`). When the repository
  changed since the client read it, the write does nothing and answers 409
  `git_state_changed` with the current state, so a Review sheet can no
  longer stage bytes, discard edits or commit an index the user never saw.
  Requests without `expect` behave as before. Path writes now use literal
  pathspecs, as the docs already said, and `files[].path` is
  workspace-relative in a subdirectory workspace. Guards cover executable
  mode, submodule HEAD and changes outside that workspace; oversized or
  budget-limited stat-only revisions cannot authorize a guarded write.
  Status stays best-effort: unreadable paths, symlinked ancestors, special
  files and broken nested repositories withhold only affected row tokens
  and the whole-tree revision. Normal Git filters and untracked settings
  apply, with directory inventories scoped to the rows or paths that need
  them. Untracked workspace files and renames leaving the workspace remain
  visible, and broken HEADs cannot satisfy an unborn-HEAD guard
  ([#6647](https://github.com/codewhale-hq/Codewhale/issues/6647)).
- Runtime API: `POST /v1/threads/{id}/fork-at-turn` forks a thread at a named
  user turn, keeping that turn and every turn before it. The receipt matches
  `/undo` and returns the first dropped prompt so a client can put it back in
  the composer. Naming the turn replaces a client-computed depth, which could
  fork the wrong prefix. The fork leaves the workspace and any running turn
  untouched
  ([#6580](https://github.com/codewhale-hq/Codewhale/pull/6580), thanks @gaord).
- Official model routing: `/router` (also `/model router`) sets up the Auto
  router with presets: Jev (TypeSafe's decision model, via OpenRouter or a
  TypeSafe key), your provider's fast tier, Off, or Custom. Jev and Fast each
  make one test call before you save, the Turn Inspector (Ctrl+Alt+O or
  `/turn inspect`) shows the router's choice, cost and latency, and a failing
  router is shown as failing
  ([#6525](https://github.com/codewhale-hq/Codewhale/issues/6525)).
- ChatGPT sign-in uses Codewhale-owned protected credentials and a model roster
  fetched for the signed-in account. Account, workspace and issuer changes
  cannot reuse another roster; a missing or stale roster offers no models.
  Authentication failures and subscription limits do not silently fall back
  to a paid API route.
- Code mode composes MCP and plugin tools and is on by default:
  `execute_tools` programs can call MCP tools, and each nested call passes the
  same approval gate as a direct call, pausing the program for approval when
  needed. Every nested call keeps its receipt, including calls that finish
  before a deadline, and `[features] code_mode = false` stops offering
  `execute_tools` up front (it stays reachable through tool search).
  `codewhale mcp list`
  and `codewhale doctor` warn when a user MCP server duplicates the built-in
  Computer Use bundle
  ([#6562](https://github.com/codewhale-hq/Codewhale/issues/6562),
  [#6509](https://github.com/codewhale-hq/Codewhale/issues/6509)).
- Tsubasa is a bundled OpenAI-compatible provider row: base
  `https://api.tsubasa.sh/v1`, key `TSUBASA_API_KEY`, model `tsubasa-pro`
  (also `tsubasa-fast`). Its window is 32,768 tokens; set
  `providers.tsubasa.context_window` to match
  ([#6695](https://github.com/codewhale-hq/Codewhale/issues/6695), thanks @cenab).

### Changed

- The pinned prompt header follows the turn at the top of the viewport,
  handing over to the previous prompt as you scroll, and clicking it jumps back
  to the message it names
  ([#6830](https://github.com/codewhale-hq/Codewhale/pull/6830), thanks @SparkofSpike).
- On Windows, every PowerShell command the shell tool starts now passes
  `-ExecutionPolicy Bypass` for that process only, so a local `Restricted` or
  `AllSigned` policy no longer blocks multi-line commands. (`.ps1` script tools
  still start as `powershell -File` without the flag and remain subject to the
  local policy.) A policy set by
  Group Policy still wins and the command is refused with PowerShell's own
  message. Scripts that a command invokes run under the same process-scoped
  setting. To let the machine or user policy apply instead, start Codewhale
  with `CODEWHALE_POWERSHELL_EXECUTION_POLICY=inherit`: it then omits
  `-ExecutionPolicy`, so a `Restricted` or `AllSigned` policy refuses the
  multi-line commands that need a temporary script; any other value keeps
  the default. Not verified on a native Windows machine
  ([#6745](https://github.com/codewhale-hq/Codewhale/issues/6745)).
- The website's not-found page now uses the Codwhale poster and typo joke,
  with English/Chinese recovery links to home and docs
  ([#6419](https://github.com/codewhale-hq/Codewhale/issues/6419),
  [#6420](https://github.com/codewhale-hq/Codewhale/pull/6420)).
- Script tools can no longer approve themselves or replace built-in tools
  (founder decision D4). A `# approval: auto` line in a script under
  `~/.codewhale/tools` (or `[tools].plugin_dir`) is ignored: the tool follows
  the session's approval setting like a script with no `approval:` line, and
  the runtime log and `/plugin tools` name each script that still declares it.
  A `[tools.overrides]` entry of `type = "script"` or `type = "command"` keyed
  by a built-in tool name is refused, and the built-in stays active; a status
  line names the key once per session, and the runtime log records it. `type = "disabled"` still turns a
  built-in off, and script or command overrides under a new name still work.
  To keep a wrapper such as an audited shell, disable the built-in and give
  the wrapper its own name
  ([configuration](docs/CONFIGURATION.md#script-tools-and-overrides)).
- Complete the session slash-command group’s shared command boundary, including
  `/structcopy`, so all seventeen commands can compile independently of the TUI.
  Host operations remain behind capability interfaces; command behavior and
  upstream tool-execution identity safeguards are preserved
  ([#6792](https://github.com/codewhale-hq/Codewhale/issues/6792),
  [#6145](https://github.com/codewhale-hq/Codewhale/issues/6145)).
- The declared minimum Rust version is now 1.89. It said 1.88, which could not
  build Codewhale: a locked dependency (`serde-saphyr`) needs 1.89, the version
  CI's minimum-version job builds. The install guides and the npm wrapper's
  build-from-source hints now say 1.89 too.
- A deferred tool's first call now runs when its arguments already carry every
  required field and no field the schema does not declare, instead of costing a
  retry turn. Malformed calls still get the schema, and approvals, deny lists
  and Plan mode apply first. Argument types are still checked by the tool
  itself. Sub-agents preload tools named in `allowed_tools`
  ([#6494](https://github.com/codewhale-hq/Codewhale/issues/6494),
  [#6437](https://github.com/codewhale-hq/Codewhale/pull/6437)).
- Automatic Git status and review reads need Git 2.31 or newer, because they pin
  Git's runtime configuration overrides. With an older Git they refuse with
  "upgrade Git (2.31 or newer)". This covers the read-only Git tools
  (`git_status`, `git_diff`, `git_log`, `git_show`, `git_blame`, `verify`); Git
  writes you ask for keep their existing behavior
  ([docs/dependency-maintenance.md](docs/dependency-maintenance.md)).

### Fixed

- A failure Codewhale can name is no longer labelled an internal fault. An HTTP
  400/405/409/413/422 rejection, an out-of-credits 402, the context-budget stop
  and a turn's own step or wall-clock ceiling now carry an input or budget
  label, and a bare `ERROR` from a provider is reported as an unreadable error
  instead of a warning. Refs #6843.
- A transient upstream failure reported as an error frame inside a successful
  response is retried within the stream retry budget. When the budget is spent
  the turn fails once with an error card instead of an amber warning that
  promised a retry. Refs #6795.
- Diff lines and tool output wrap at grapheme boundaries, so emoji families,
  skin tones and variation selectors no longer split across lines
  ([#6829](https://github.com/codewhale-hq/Codewhale/pull/6829), thanks @Lstarsky0).
- Twelve translated packs now translate the context inspector's making-room
  and anchors rows, the Ctrl+O hint and the `/turn inspect` and `/advisor`
  descriptions instead of showing English
  ([#6831](https://github.com/codewhale-hq/Codewhale/pull/6831), thanks @Lstarsky0).
- Optional MCP servers can be found before they connect. `tool_search` matches
  the query against configured, enabled server names (or an `mcp_<server>_`
  prefix), connects up to eight matches within the existing boot wait, and
  returns their real tool schemas. Before, a lazily started server exposed no
  tools, so the model could never trigger its connection
  ([#6828](https://github.com/codewhale-hq/Codewhale/issues/6828)). Known limit: a
  query that names neither the server nor `mcp` does not wake it.
  `codewhale mcp connect`, `validate` and `tools` run their own connection and
  do not attach to a running session.
- `/undo` and `/restore <N>` refuse while a turn is running in the workspace,
  instead of rewriting files under it.
- `--resume <id>` after a crash recovers that session's interrupted turn from
  its crash checkpoint, as `--continue` does.
- `/resume <file>` keeps an imported session on the current provider route
  instead of a default one.
- When the stall watchdog recovers a turn, the Engine's turn is ended too, so
  the next message is accepted
  ([#6800](https://github.com/codewhale-hq/Codewhale/issues/6800)).
- A turn you cancel, or one the stall watchdog recovers, releases input at once.
  A message still waiting to reach the engine is abandoned and its unsent text
  returns to the composer, instead of holding input for up to 60 seconds
  ([#6800](https://github.com/codewhale-hq/Codewhale/issues/6800)).
- A transient upstream failure that a gateway reports as an error frame inside
  a successful response ("Provider returned an empty response") is retried
  under the stream retry budget when nothing had streamed; authentication and
  invalid-model frames still fail at once
  ([#6795](https://github.com/codewhale-hq/Codewhale/issues/6795)).
- Long sessions no longer start every turn late. Once a workspace held more
  than 50 undo snapshots (around the sixteenth turn), each new snapshot rebuilt
  the whole snapshot history before the provider request, about 2.8 s per turn
  in a small workspace. Old snapshots are now dropped half a window at a time.
- Resuming a crashed session from inside the TUI recovers its interrupted
  turn too.
- Release builds compile on Rust 1.99.
- TUI undo and retry rewind the Engine conversation and saved session before
  replacement inference. If the conversation or its settings change while
  undo is being prepared, it refuses without overwriting that newer state or
  changing the draft. Retained compaction summaries survive the rewind and
  cannot be selected as editable user prompts
  ([#6788](https://github.com/codewhale-hq/Codewhale/issues/6788)).
- `/edit` replaces the exchange it revises through the same rollback: the old
  prompt and its answer leave the transcript, the model's context and the
  saved session before the edited prompt is sent.
- When the rollback that `/edit` needs is refused, your revised text goes back
  to the composer with edit mode still on, instead of being dropped. Opening
  `/edit` while you are editing a queued follow-up returns that follow-up to the
  queue first.
- A session can still be resumed by its id when a stray copy of its file sits
  in the sessions directory; before, the id was reported as ambiguous.
- Agents follow the Permissions you choose while they run: switching to
  Full Access reaches an agent that is already working, instead of leaving
  it with the Auto-Review guardian that denied it. Tightening reaches it too.
  An agent's answer to an approval prompt is delivered while the main
  session is busy, and a forced delete inside the workspace is no longer
  treated as a catastrophic system delete. On Linux, Full Access switched on
  at runtime cannot lift the kernel no-new-privileges flag set at startup,
  so `sudo` and setuid helpers still fail; start with
  `sandbox_mode = "danger-full-access"` or `CODEWHALE_NO_NEW_PRIVS=0` if
  agents need them ([#6787](https://github.com/codewhale-hq/Codewhale/issues/6787)).
- Stream limits and transport settings share a typed `[stream]` configuration
  table, including retry budgets, TCP keepalive and HTTP/2 keepalive. Explicit
  values take precedence over legacy `[tui]` aliases; omitted values preserve
  existing defaults and environment behavior. A configured HTTP/1 pin stays
  pinned during recovery. `/config stream_chunk_timeout_secs ... --save`
  updates the canonical setting so it survives reopening a configuration
  that already had a timeout. Transport changes apply when a client is built
  ([#6700](https://github.com/codewhale-hq/Codewhale/issues/6700)).
- Switching providers keeps a model set only in the root `default_text_model`
  with the provider it belongs to. Switching away and back (`/provider` in the
  TUI, or the desktop app's model chip) used to land on the provider's catalog
  default such as `gpt-5.6`, and a pass-through provider switched to in between
  inherited the other provider's model, or a stale legacy root `model` that the
  root default had been shadowing
  ([#6693](https://github.com/codewhale-hq/Codewhale/pull/6693)).
- A turn no longer stops after an hour of work. The cumulative per-turn wall
  clock is unlimited by default, like model steps; set
  `[tui].turn_wall_clock_secs` to cap it.
- OpenRouter turns are priced again instead of always showing "rate
  unavailable". The model-list refresh no longer fails as a whole on one bad
  row: `~` "latest" aliases are dropped, routers that publish a `-1`
  variable price are listed without a rate, and other malformed rows are
  skipped and counted. Main interactive turns now honor a `[[custom_models]]`
  rate the same way background work does, a declaration without rates no
  longer hides the catalog price, and an explicit declared rate prices a
  vendor-pinned OpenRouter route
  ([#6690](https://github.com/codewhale-hq/Codewhale/issues/6690)).
- `codewhale exec` can take prompts past the OS argument limit (~128 KiB on
  Linux, where larger prompts failed with `Argument list too long` before
  Codewhale started): `--prompt-file <PATH>` reads the prompt from a file, and
  `--prompt-file -` reads it from stdin; a positional `-` stays literal text
  ([#6688](https://github.com/codewhale-hq/Codewhale/issues/6688)).
- Keep reasoning effort labels, including `xhigh` and `ultra`, visible beside
  short model names in the 80-column terminal footer.
- First-run TUI startup keeps explicitly configured providers, models and
  endpoints (including environment overrides and a legacy top-level
  `base_url`), even with a missing key, instead of adopting a detected local
  Ollama model. The `default_text_model` line written into the generated
  first-launch config is not treated as a choice, so starting Ollama after the
  first launch is still detected. Usable configured routes skip the provider
  picker and record the provider step as configured, without claiming a
  successful check; missing-key recovery focuses the configured provider even
  on a fresh home. Switching from that recovery picker (or after Esc) to a
  provider that has its key clears the launch "needs a key" state, so the
  footer names the route instead of "model not connected" and a detected local
  Ollama can no longer take over the provider just chosen. Empty sessions no
  longer show a full context window from the startup prompt; a submitted first
  turn or reported usage still shows real pressure.
- `/trust on|off` changes file-tool trust for this session. Add `--save` to
  persist workspace trust for project skills, commands, hooks, MCP servers,
  and project context. Skipped-skill notices stay out of conversation titles
  and user bubbles, and trust changes append a correction to the model's history.
- Stored-secret cleanup skips busy Runtime stores and reports unreadable paths
  while continuing through other files. Configured storage roots may be symlinks;
  discovered symlinks are not followed. Cleanup also masks current configured
  credentials, and doctor's bounded scan reserves capacity for saved transcripts
  and checkpoints separately from Runtime receipts.
- Hooks: `tool_call_after` and `on_error` now get a shell command's exit code
  in `DEEPSEEK_TOOL_EXIT_CODE` on Runtime API threads as well as in the TUI,
  and for a failing command as well as a passing one, so `exit_code`
  conditions match. The Runtime API path passed no exit code at all, and a
  command that exited nonzero, timed out, or was killed reached hooks with no
  exit code on either surface. The new `DEEPSEEK_TOOL_STATUS` says how the
  command ended (`completed`, `failed`, `timed_out`, `killed`)
  ([#6582](https://github.com/codewhale-hq/Codewhale/issues/6582)).
- Scrolling a long transcript repaints the reasoning reveal hint in place
  instead of rebuilding the transcript tail. Wheel and scrollbar input use
  interactive frame pacing, collapsed tool groups reuse cached summaries, and
  height-only resizes keep wrapped history. Width changes still rewrap content,
  and other per-frame work still grows with session length
  ([#6652](https://github.com/codewhale-hq/Codewhale/issues/6652)).
- A settled transcript frame reuses its existing render plan instead of
  rebuilding or moving unchanged history on every poll. New output, selection,
  resize and other real changes still invalidate the affected work
  ([#6652](https://github.com/codewhale-hq/Codewhale/issues/6652)).
- Workflow progress rows and working, verification and translation indicators
  use the pinned Codewhale terminal component kit. Quiet and reduced-motion
  working/verification indicators hold its semantic current-work mark. Working
  and verification keep their existing delay and cadence, while translation
  uses the same earned marker and five-frame-per-second cadence. The Engine
  still supplies workflow state, clocks, localized text, custom themes and
  terminal adaptation. The composer, transcript viewports, dock tabs,
  posture/metrics rows, pending-input cards, approval band, Ocean background
  and the whale's Braille raster now also render through the pinned kit.
- Calm transcript previews keep the latest three rows of live thought and a
  single duration header when it settles. Successful tool headers are quieter;
  failed generic and MCP calls keep a bounded head-and-tail excerpt, with full
  output available in details. Existing collapsed groups show their hidden
  count and reveal cue, and explicit thought expansion stays available.
- Composer history preserves multiline entries and literal quoted text while
  retaining the old history file during migration. An oversized message stays
  in the composer if its paste file cannot be saved, with a localized notice;
  mouse placement and quoted file mentions handle multibyte text and spaces
  ([#6776](https://github.com/codewhale-hq/Codewhale/pull/6776)).
- The notice for an oversized message that cannot be saved as a paste file is
  now a short line that fits the footer ("Not sent: over N characters; paste
  file not saved."), in every supported language. The full reason, including the
  write error, goes to the transcript once.
- Model-facing guidance names callable tools and explains discovery before a
  deferred read. The stopship workflow's scout activates the deferred
  `grep_files` with one required `tool_search` call, then gathers bounded
  `grep_files` evidence from the current workflow source paths
  ([#6747](https://github.com/codewhale-hq/Codewhale/issues/6747)).
- Simplified and Traditional Chinese permission, provider and session wording
  now follows the current behavior. Chinese guides correct hook receipts,
  trusted skills, MCP startup and Windows limitations, preserving the
  contributor translations and a shared glossary.
- Unknown website locale segments, including dotted missing paths, return a
  real 404 with noindex metadata and no home-page canonical URL. The existing
  layout guards now have regression coverage across all registered locales
  ([#6786](https://github.com/codewhale-hq/Codewhale/pull/6786)).
- The terminal caret no longer blinks at the hidden composer while a picker,
  settings screen or other view covers it; it returns when the view closes
  ([#6545](https://github.com/codewhale-hq/Codewhale/issues/6545)).
- On Linux and macOS, a `background: true` shell's process group is now
  stopped when the TUI dies without cleaning up (SIGKILL, a crash, or the
  signal exit path). The shell and the processes it started used to keep
  running as orphans. Processes that move to their own process group or
  session, staged services you choose to keep, and `tty: true` background
  shells are not covered. Under the bwrap sandbox, the sandboxed command
  now also exits when bwrap does
  ([#6654](https://github.com/codewhale-hq/Codewhale/issues/6654)).
- Idle task workers stop periodically reloading a settled, unchanged empty
  queue, and task listings reuse an unchanged store snapshot. New queue writes
  and local notifications still wake workers; recent or unreadable metadata and
  failed claims keep bounded retries. This reduces repeated disk work when
  several TUIs share a data directory without changing task ownership or
  cancellation ([#6573](https://github.com/codewhale-hq/Codewhale/issues/6573),
  [#6728](https://github.com/codewhale-hq/Codewhale/issues/6728)).
- Idle task workers check the queue file's metadata about once a second instead
  of five times a second, and no longer reloads the store when nothing changed.
  An in-process notification still wakes a worker at once; a
  write by another Codewhale process that shares the data directory is noticed
  within about a second instead of about 200 ms. A worker with pending work, a
  retry deadline or a failed claim keeps the short interval
  ([#6728](https://github.com/codewhale-hq/Codewhale/issues/6728),
  [#6573](https://github.com/codewhale-hq/Codewhale/issues/6573)).
- After five seconds without session activity, the UI loop polls every 250 ms
  instead of 48 ms and the automation-panel scan backs off to 15 seconds. After
  thirty quiet seconds, the Git probe runs one `git status` every 15 seconds
  instead of the full probe every two seconds, unless the Git panel is showing.
  User input and engine events restore the active cadence. This reduces idle
  work; it does not remove every cost that grows with session history
  ([#6728](https://github.com/codewhale-hq/Codewhale/issues/6728)).
- When the task store is busy, the message now names the process that holds its
  lock and how long it has held it ("lock held by pid 1234 for 12s"), when that
  can be read. A repeated "Task claim unavailable" error is logged once per
  episode, then at debug level
  ([#6573](https://github.com/codewhale-hq/Codewhale/issues/6573)).
- On Windows, Fleet can read a running worker's log while its writer holds the
  file open, and task-store contention can still read the lock-holder record.
  These paths previously failed because of whole-file sharing and locking.
- File writes, including legacy `File` and `write_file` calls, stop if the
  original contents cannot be read; legacy writers also reject non-UTF-8
  originals, keeping undo and diffs from recording an empty original file.
- Resuming a session preserves user messages that quote a compaction marker.
- Requirements allow-lists now check unset approval and sandbox defaults,
  accept equivalent approval aliases, and lock permission posture when sandbox
  modes are constrained so saved Full Access and YOLO cannot bypass them.
- Logout completes xAI OAuth revocation and attempts every credential deletion
  before reporting remaining credentials with a non-zero exit status.
- ChatGPT and xAI sign-in now show the account the selected route actually
  uses, and sign-in names the account it replaces. `/auth chatgpt` and
  `/auth xai-device` switch the running session; signing in from a separate
  shell requires restarting an already-open session. Subscription-limit errors
  name the account that made the request and explain how to switch, without
  printing credentials ([#6715](https://github.com/codewhale-hq/Codewhale/pull/6715)).
- `doctor --fix` keeps temporary files modified within the last hour.
- Provider streams stop with a clear error if an SSE line exceeds 8 MiB.
- Recursive `rlm_query` turns inherit the parent's remaining deadline,
  bounded by the existing child budget, including asynchronous lock waits,
  startup, model requests and Python evaluation. Expired work is refused before
  dispatch; a timeout returns the last response with an incomplete-result error.
  Nested code and status receipts accompany the enclosing tool result on success
  or failure and survive its session save/reopen. In-flight receipts still need
  that result to be handed back before they become durable
  ([#6511](https://github.com/codewhale-hq/Codewhale/issues/6511)).
- The TUI keeps redrawing while its terminal is unfocused. v0.10.0 held
  every frame on focus loss, so on Windows Terminal, macOS and other
  terminals a window sitting behind another one looked frozen until you
  clicked back into it. Frames are now held only on GTK/VTE terminals, which
  replay the damage themselves when they return. That hold was added for the
  MATE flicker in [#6311](https://github.com/codewhale-hq/Codewhale/issues/6311)
  ([#6651](https://github.com/codewhale-hq/Codewhale/issues/6651)).
- TUI `/undo` now restores only the files the undone tool call or turn
  changed, and refuses, changing nothing, when one of them changed since.
  Before, it checked out the whole snapshot tree, which also reverted later
  edits to other files. It also finds restore points older than the newest
  100 snapshots, and a forked session can undo the turns it inherited. It
  waits for a post-turn snapshot that is still being written, leaves a
  changed symlink or other non-regular path in place (and says so) instead
  of refusing the whole undo, and writes nothing to the snapshot store when
  it refuses outside trusted mode
  ([#6644](https://github.com/codewhale-hq/Codewhale/issues/6644)).
- On a fixed model, each Ctrl+T press now moves to a different thinking
  tier. Ctrl+T and the `/model` picker could list tiers the route treats as
  the same one — DeepSeek's `medium` is `high`, and Z.ai GLM-5.2's `low` is
  `high` — so some presses changed nothing. They now share one list per
  route with one entry per effective tier. Under Auto model routing, Ctrl+T
  walks the picker's Auto list instead of a longer private one; the tier is
  still settled when the turn is routed, so two neighbouring choices can
  land on the same tier. DeepSeek now reports `minimal`, `xhigh` and `ultra`
  as the `low`, `high` and `max` it sends, in `/status`, receipts and
  `/effort`, and Kimi Code K3 no longer offers `off`, which it always ran as
  `low` ([#6650](https://github.com/codewhale-hq/Codewhale/issues/6650)).
- A top-level `base_url` or `api_key` in `config.toml` now means one thing
  everywhere. Every reader used its own rule for which routes inherited it,
  which is how a DeepSeek endpoint became the Xiaomi MiMo route's and failed
  with DeepSeek's 401. Old files keep working: the keys are read as
  `[providers.deepseek]` (or the vendor whose official host they name), the
  next save moves them there with a one-time backup and a one-line note, and
  a value that disagrees with its table is left for `codewhale config migrate
  --prefer` to settle. `codewhale config doctor` shows what is in use
  ([#6394](https://github.com/codewhale-hq/Codewhale/issues/6394)).
- Setting up a bundled OpenAI-compatible host from `/provider` now shows
  that host's key console, docs link and guidance on the setup form, the same
  way the built-in providers' key entry does; before, the descriptor file
  carried them and nothing read them. AICraft gains all three
  ([#6616](https://github.com/codewhale-hq/Codewhale/issues/6616), thanks @BX166).
- Runtime API undo now restores the files of the turns it undoes, for every
  kind of thread. Before, a new thread's turns were not linked to their
  workspace snapshots, so `patch-undo` returned `201` and rewound the
  conversation but left the files changed, and `file-revert` refused. Each
  turn record now lists its snapshots in `workspace_snapshots`, the engine runs
  under the thread's own id across restarts, and a fork owns the turns it
  inherited. `patch-undo` restores only the files the undone turns changed,
  including every write in a turn, and leaves later edits by the user or
  another thread alone. Every tool call that may write is bounded by its own
  snapshots, so a file another thread or an editor changed while the turn ran
  is never reverted as the turn's, and a turn's restore points survive the
  snapshot count cap. When it cannot restore files (including a write to a
  gitignored path) it now refuses with `409` and an `error.code` instead of
  returning `201`. A nameless `PUT /v1/sessions` updates the document the
  thread is bound to
  ([#6621](https://github.com/codewhale-hq/Codewhale/issues/6621)).
- A Runtime thread that is not bound to a saved session keeps one engine
  session id, its own thread id, across its first turn, eviction and
  restarts. Before, each engine spawn generated a new id, so the thread's
  tool-output spills, snapshot tags and shell jobs were scattered across a
  different `sessions/<id>/` per spawn
  ([#6659](https://github.com/codewhale-hq/Codewhale/issues/6659)).
- Saved sessions no longer go orphaned, and existing orphans are repaired at
  launch. Deleting a session now sets aside the Runtime store it was actually
  bound to, and switching a conversation to another host's store sets aside the
  store it left, unless another session still uses that store or it holds
  work. Exporting a thread (`POST /v1/sessions`) updates the thread's own
  document instead of creating a new one each time. A thread whose document was
  appended to elsewhere keeps loading. A thread whose document was deleted or
  rewritten loads from its own turns instead of failing, and threads write
  their files under the session they belong to. Each launch and `codewhale
  serve` repairs the store in the background: a store holding threads with no
  session gets a "Recovered:" session for each thread, and unreadable
  documents, unused empty stores and old files no session names move to
  `sessions/.set-aside/` with a manifest. Nothing is deleted. `codewhale doctor`
  reports the last repair, and `codewhale doctor --repair-sessions [--dry-run]`
  runs one now. `PUT` and `DELETE /v1/sessions` refuse a session that another
  Codewhale process has open
  ([#6144](https://github.com/codewhale-hq/Codewhale/issues/6144)).
- Runtime API: the thread event stream now ends with a typed `stream.end`
  frame (reason and resume cursor) whenever the server closes it, including
  on shutdown, so clients can tell a Runtime error from a dropped connection
  and resume from the right event. A replay worker crash no longer ends
  history early and skips events without a detectable gap, and the mobile
  page resumes from its last event instead of replaying from zero, with stream
  statuses in the configured UI language. Cross-origin clients can read the
  stream-end and replay-progress capability headers, and the SDK exports
  `isThreadStreamEnd` for TypeScript narrowing.
- `codewhale exec --auto` no longer exits 141 with no output when a child
  it writes to, such as a stdio MCP server, closes its pipe early. Headless
  exec now ignores SIGPIPE while it runs, as the interactive TUI already did,
  and `exec ... | head` still ends quietly. One-shot `codewhale exec` no
  longer prints DeepSeek's raw `<｜｜DSML｜｜ calls>` tool-call markup as its
  answer: the markup is removed, and an answer that was only a tool call
  fails at once with the reason and a pointer to `--auto`, instead of asking
  the model again and blaming an incomplete provider response.
- Resuming a session keeps its "Resumed:" confirmation on screen instead of
  replacing it with "Make room automatically: on" when nothing was switched.
- Tool output is no longer cut off where you can't get it back. Search
  answers, test runs, git, verifier and web results reach the model whole up
  to one budget sized to the model's context window, and anything beyond it
  can be read back with `retrieve_tool_result`, including read-only tools
  executed alone or in parallel. Restored raw results use the
  active route's same inline budget; existing recovery receipts stay intact.
  Tiny budgets use a compact retrieval reference instead of a long artifact
  path and instructions; a complete reference is kept even when it alone
  exceeds the allowance.
  Native search now asks for answers up to 8,192 tokens (was 2,048–4,096)
  and waits long enough for
  them to arrive, and an answer the provider still cuts short is marked as
  cut ([#6508](https://github.com/codewhale-hq/Codewhale/issues/6508)).
- The installation page is generated from `docs/INSTALL.md`, so the website
  and the guide can no longer disagree; broken anchors and unsafe links fail
  the build ([#6450](https://github.com/codewhale-hq/Codewhale/pull/6450)).
- `codewhale config set` refuses a value of the wrong type for a known setting
  (a word for an on/off switch, text for a number, a choice outside the list)
  instead of saving it ([#6568](https://github.com/codewhale-hq/Codewhale/pull/6568),
  thanks @dajiaohuang).
- Receipts: `/receipts`, `codewhale receipts [ID|--last] [--format md|json]`,
  and `GET /v1/threads/{id}/receipt` (plus a per-turn form) list what a session
  did, one line per action: files changed with line counts, commands with exit
  codes, web and MCP calls, agents, approvals and who gave them, and failures.
  They also count what ran without asking and name the posture each turn ran
  under, read from the turn's own record. A call Codewhale blocked before it
  started (Auto-Review or guardian, a tool policy, a refused sandbox
  escalation, invalid input, a missing tool) is listed as blocked, with the
  reason, and is not counted as run or as ran without asking. Only
  Codewhale's own refusal text counts: an MCP server, a fetched page, or a
  program cannot make a call that ran read as blocked. A terminal
  session's receipt also lists the files a command changed in each turn,
  from the workspace snapshots taken before and after it (not ignored files
  or anything outside the workspace), with control characters in paths
  escaped so a file name cannot forge a receipt line. All three read the
  records Codewhale already keeps and say what those records do not hold
  ([docs/RECEIPTS.md](docs/RECEIPTS.md)). `audit.log` is not that record: it
  logs security events, and it logs an approval only when one is requested,
  which under Full Access is almost never.
- Approvals now record who decided: you, a session rule, or the posture. An
  automatic approval used to be saved exactly like one you gave, and an app
  approval that expired was saved as your denial. `GET /v1/approvals` now
  returns `decided_by`.
- Network audit lines now go to the same `audit.log` as every other audit
  event (`$CODEWHALE_HOME` included), and test runs no longer append to your
  real one.
- In the terminal UI, Auto-Review verdicts now reach `audit.log`, as
  `/permissions` said they did; before, they were written only when
  `CODEWHALE_TOOL_AUDIT_LOG` was set. Headless `exec` and Runtime API sessions
  do not write them yet.
- A turn that stops producing output now reports itself: the turn loop records
  its phase and last progress, and an overdue phase surfaces instead of
  hanging silently until the stream idle timeout. A delegated agent's final result is
  never dropped when the host is busy, so a finished child no longer leaves a
  ghost Running row behind ([#6184](https://github.com/codewhale-hq/Codewhale/issues/6184)).
- Git commands run by tools never stop to ask for a password, passphrase or
  host-key confirmation inside the terminal, and `git_fetch` has a timeout
  ([#6184](https://github.com/codewhale-hq/Codewhale/issues/6184)).
- A provider response that ends cleanly with no text and no tool call is
  retried before the turn fails, and the failure names how many retries ran
  ([#6310](https://github.com/codewhale-hq/Codewhale/issues/6310)).
- The context meter, the point where Codewhale makes room, preflight,
  `/context` and turn receipts show one pressure number instead of disagreeing
  ([#6407](https://github.com/codewhale-hq/Codewhale/pull/6407)).
- When Codewhale makes room, the note it leaves for the next turn is written in
  its own words, under fixed headings: the objective, the user's corrections,
  permissions and limits, changed files, what is still running and the checks
  left. A message that quotes the note's opening words now stays in the
  conversation instead of being dropped. If the summary request itself is too
  large, older history is dropped but the previous note is kept, and making
  room stops rather than write a new note without it.
- Continuing a conversation that is already open no longer adds a second
  thread, and a fork keeps its own session file, so autosave on one side no
  longer leaves the other unloadable
  ([#6406](https://github.com/codewhale-hq/Codewhale/pull/6406), thanks @gaord).
- Upgrading Codewhale no longer turns off the built-in Computer Use. Each build
  writes the built-in bundle to its own directory, so an upgrade used to present
  it as never reviewed and disabled. Now the review and enablement carry to the
  new build when its capabilities are unchanged. Changed capabilities show
  `capabilities-changed` and wait for review, and a revoked trust never carries
  ([#6303](https://github.com/codewhale-hq/Codewhale/issues/6303)).
- "Allow for this conversation" records a grant for that tool and argument
  class instead of switching the whole thread to Full Access, so the call you
  just approved is no longer failed by a Permissions change. An approval
  also survives a Permissions change that only widens what is allowed, grants
  end when a thread is archived or deleted, and `web.run` open grants are
  scoped by host. Full Access covers MCP tools that declare themselves destructive in
  every host, including `codewhale exec`
  ([#3866](https://github.com/codewhale-hq/Codewhale/issues/3866)).
- `web.run` retries a refused page once with a browser user agent, and one
  site's failure no longer fails the whole call or drops its search results.
- Web search no longer stops when DuckDuckGo cannot be reached. After a
  connection error, timeout or non-2xx answer, the search tries Bing before
  giving up, and the result says it used Bing. A DuckDuckGo that hangs no longer
  uses up the whole time budget: when Bing may follow, DuckDuckGo gets 60% of it
  and Bing the rest. This also covers the DuckDuckGo step after a configured API
  provider. A custom `search_base_url` still has no public fallback and keeps
  the full budget. Bing sees the query only after DuckDuckGo has failed or
  returned nothing it could read
  ([#6746](https://github.com/codewhale-hq/Codewhale/issues/6746)).
- Hooks treat `bash`, `Bash` and `exec_shell` as one tool in `tool_name`
  conditions, so the documented example fires.
- macOS no longer reports Codewhale's ordinary heap as GPU (IOAccelerator)
  memory.
- Code highlighting uses less memory, and long transcripts, the pager and the
  session picker do less work on the event loop; session previews load in the
  background ([#6014](https://github.com/codewhale-hq/Codewhale/issues/6014)).
- The composer's send cue follows the draft, not a paste in progress
  ([#6397](https://github.com/codewhale-hq/Codewhale/issues/6397)).
- Voice status is localized, ASCII-mode markers are distinct, and the cursor
  honours `NO_COLOR` ([#5846](https://github.com/codewhale-hq/Codewhale/issues/5846)).
- `/cache`, `/stash`, `/config`, session prune, `metrics --since` and the
  `lane start`/`lane stop --json` flags handle their edge cases.
- Plain `codewhale exec` (no `--auto`) runs one Engine turn with the same
  system prompt as every other run, so `--json` no longer changes the model's
  instructions. It offers no tools unless a flag grants them (`--auto`,
  `--yolo`, `--allowed-tools`, or resuming a session): `--max-turns`,
  `--disallowed-tools`, `--append-system-prompt`, `--sandbox` and
  `--output-format stream-json` no longer turn a chat call into a tool-using
  agent, and tool-only flags passed without a grant print a warning. A reply
  cut off at the provider's output limit is continued instead of failing the
  run, for at most 8 model steps unless `--max-turns` sets another limit.
  Breaking for scripts: the `--json` one-shot receipt no longer has
  `stop_reason`, adds the agent receipt's fields (`prompt`, `tools`,
  `outcomes`, `status`, `termination_reason`, `error_category`), and leaves
  out `usage` when the turn never settled
  ([#6510](https://github.com/codewhale-hq/Codewhale/issues/6510)).
- `codewhale review` of a plain diff uses the same review prompt as
  `review --pr` and prints the structured review as Markdown, or the model's
  prose when it ignores the JSON format
  ([#6510](https://github.com/codewhale-hq/Codewhale/issues/6510)).
- A recursive `rlm_query` that runs out of rounds returns its last answer
  marked `[rlm_query incomplete: …]` instead of an empty string, its model
  calls appear in the parent turn's record, and its history is no longer
  trimmed ([#6511](https://github.com/codewhale-hq/Codewhale/issues/6511)).
- Starting without a network connection no longer drops images you attach to a
  model that accepts them. The offline model list lagged behind providers and
  listed Claude and others as text-only; it can now only say a model takes
  images, never that it refuses them, and a provider that does refuse gets one
  resend without the image and a message saying so
  ([#6396](https://github.com/codewhale-hq/Codewhale/issues/6396)).
- Model prices that a flat rate would get wrong now show as unknown after the
  model list refreshes online, as they already did offline: DeepSeek (priced by
  time of day, or from Codewhale's reviewed table on hosted providers), Grok and MiniMax M3 (rates rise on long prompts), Xiaomi MiMo
  (pay-as-you-go and Token Plan keys look the same), GLM-5.3 and the Alibaba
  Model Studio plans (quota or credits, not per-token) and StepFun. DeepSeek's output limit stays at its
  published 384K instead of the refreshed 393,216, and the reasoning controls
  Codewhale records for Grok, MiniMax, Qwen 3.8 Max, Muse Spark and Step 3.5
  (defaults, always-on thinking, extra effort tiers) survive the refresh.
  Committed corrections now fail at load if their provider or model is missing
  from the bundled seed; partial live refreshes may still omit those rows
  ([#6396](https://github.com/codewhale-hq/Codewhale/issues/6396)).
- The offline model list is now generated from Models.dev instead of edited by
  hand, so a fresh install without network sees the same limits, image
  support and reasoning controls as an online one. Claude, GPT-5.5, Kimi and
  several OpenRouter models now show image input offline, and a model whose
  catalog row offers an on/off switch next to its effort tiers keeps Off in
  `/effort` and Ctrl+T instead of rounding it up
  ([#6396](https://github.com/codewhale-hq/Codewhale/issues/6396),
  [#6612](https://github.com/codewhale-hq/Codewhale/pull/6612)).
- Cost estimates for GPT-5.6 and GPT-5.6 Sol use OpenAI's current rates of
  $4 input, $0.40 cached input and $20 output per million tokens. They were
  still at the older $5, $0.50 and $30
  ([#6396](https://github.com/codewhale-hq/Codewhale/issues/6396)).
- `codewhale integrations dsh` now reads current DeepSeek Harness prerelease
  versions such as `0.1.7-alpha.2` as semver. They were reported as `offline`
  and refused; they now show as `stale-version` (launchable once connected,
  clearly unverified). Only `0.1.0-rc.6` is still the verified version, and text
  that is not a version is still `offline`.
- Rule and chain-segment source labels in the project instructions the model
  receives (`<project_rule source=...>` and the scoped-instructions comments)
  are repo-relative instead of absolute. Moving or recasing a checkout no longer
  changes the pinned system prompt prefix or adds a spurious context update
  ([#6739](https://github.com/codewhale-hq/Codewhale/pull/6739), thanks @asto18089).

### Security

- On Windows, a broad Node kill (`Stop-Process -Name node`,
  `Get-Process node | Stop-Process`, `taskkill /IM node.exe`, and wrapped,
  aliased or nested-shell forms) is held by a built-in safety floor: it is
  refused in Full Access, Auto-Review and Never, and asks in other modes. With
  the npm launcher such a command ended this and every other npm-launched
  Codewhale session without cleanup. Stopping a server by PID or port still
  works, and the Windows installer and archives have no Node launcher
  ([#6827](https://github.com/codewhale-hq/Codewhale/issues/6827), thanks @jayanthvee).
- `code_execution` and `js_execution` now start their Python and Node
  interpreters through the same permission-aware launcher as other commands, so
  the session's execution policy applies to them. Before, an approved call ran
  the interpreter directly, and its working directory was the only boundary
  ([#6820](https://github.com/codewhale-hq/Codewhale/pull/6820), thanks @Guan0923).
- A saved task without an `auto_approve` field is no longer treated as
  auto-approved. Updates accept HTTPS URLs only from the release-host
  allow-list, and the npm release-asset check bounds every request.
- Workspace profile and skill discovery, pasted-image and screenshot writes,
  configuration redaction receipts and legacy configuration migration reject
  linked paths where they could redirect access outside their intended roots.
  Additional `.codewhale` writers use the existing confined filesystem helpers.
  The VS Code file opener checks the file's real path, and PowerShell temporary
  scripts receive unpredictable names and owner-only permissions.
- Update development tooling to Undici 7.29.1 or newer for
  [GHSA-w293-vg96-wgc3](https://github.com/advisories/GHSA-w293-vg96-wgc3),
  brace-expansion 1.1.21/5.0.12 for
  [GHSA-q2hr-2g5m-vwhr](https://github.com/advisories/GHSA-q2hr-2g5m-vwhr),
  and the VS Code extension's markdown-it to 14.3.2 for
  [GHSA-253c-mchw-3w2r](https://github.com/advisories/GHSA-253c-mchw-3w2r).
- Harden local runtime browser sessions, fleet SSH trust, agent continuation
  ownership, task gate approval, plugin tool registration, bridge action tokens,
  and release metadata credential forwarding. Browser sessions recover across
  reloads and new tabs, and stream tickets retry after transient failures.
  Fleet SSH known-host checks support OpenSSH 7.x and later. SSH host configs
  using `host_key_fingerprint` must migrate to `known_hosts` with verified host
  keys; the unsupported fingerprint field is now refused when an SSH worker
  starts, before any connection is made, instead of being silently ignored.
- Harden workspace instruction, note, and anchor file access with shared
  no-follow reads and writes. Compaction loads pinned anchors only from
  trusted workspaces. Validate registry skill names before selecting cache paths.
- Tighten approval, execution and endpoint boundaries. Computer control
  consent and script calls need an exact decision a person gave on that call's
  own approval card. Automatic Git status and review reads go through the
  sanitized review command and stay pinned to the checked Git executable.
  Config backups, config dumps, MCP listings and notification payloads share
  one sensitive-key vocabulary; structured exports now also redact keys ending
  in `key`. Python a child model writes during a recursive `rlm` call is
  admitted round by round like an inline `repl` block, and Python kernels are
  discarded when permissions narrow. Gate commands and the cargo test runner
  start inside the session's shell sandbox. Mutable session artifacts are
  written and read through pinned session-relative paths. The stdio bridge's
  Runtime child chooses and reports its own loopback port.
- Chat bridges (Feishu, Telegram, WeCom, Weixin) accept an approval decision
  only from the person who started that turn, for the Runtime's current
  pending approval. After upgrading, approvals for turns that were already
  running are decided from the TUI, and WeCom `/allow` no longer takes
  `remember`.
- More file and network paths are confined. A task or automation created by a
  session without full shell access no longer inherits the host's shell default.
  The commit planner, oversized-paste backup and project harness notes do not
  read or write through links that leave the workspace. On Unix, audit and
  approval logs are created owner-only and are not opened through a link;
  Windows is unchanged. Lane ids must be plain names. A session file is refused
  when it records a different session id than its name. The updater does not
  follow a redirect from HTTPS to plain HTTP and stops reading a response larger
  than 512 MB.
- Nothing is written through a `.codewhale` that is a link. The workflow run
  journal no longer creates `.codewhale/` or its ledger when it is opened, only
  when the first record is written, and reads and appends through the same
  confined helpers as other state. The sub-agent coordination lock refuses a
  linked `.codewhale` or `state` directory before it creates anything.
- On Unix, the runtime log is created owner-only and is not opened through a
  link; an existing log from an earlier version is tightened. The
  `.reconcile.lock` and `current.json.lock` lock files are created owner-only
  too.
- Inline ```` ```repl ```` blocks in a reply now follow the same rules as the
  `code_execution` tool. They never run in Plan mode or when
  `code_execution` is not on the turn's tool surface, and they are admitted
  like a `code_execution` call with the same code: before-tool hooks,
  disallowed-tool rules, Auto-Review (including its reviewer) and repository
  law all apply, then the same approval under the session's permission
  posture (a blocked, denied or unanswerable call skips them with a visible
  note). Only a fence that opens its own line counts, here and in the `rlm`
  tool's child rounds, so a reply that mentions a fence in prose runs nothing.
  An `rlm(...)` call from a fence gets a one-shot child answer rather than a
  child that runs its own code. The docs no longer describe the REPL kernel
  as sandboxed.
- `codewhale serve --mcp` now exposes only read-only tools by default
  (`file_read`, `search`). Tools that write files or run commands are
  neither listed nor run unless the operator sets `require_approval = false`
  in the MCP server config; a client's own `approved` flag no longer counts
  as approval. At startup the server names on stderr each configured tool it
  withholds and the setting that allows it.
- A `Bash` entry in the disallowed tools now also blocks `start_mcp_server`
  and `start_registry_mcp_server`, which launch local processes.
- An MCP tool call whose connection closes before it answers is no longer
  sent again on a new connection, since the server may already have run it.
  The same holds for a server error reply that mentions an expired or invalid
  session. The call reports an unknown outcome and the next call reconnects.
  Only a call the HTTP transport turned away for a stale session, before the
  server handled it, is still retried once.
- Durable tasks and scheduled automations created by the agent no longer
  carry more authority than the session that created them: requested
  `auto_approve`, `trust_mode` and `allow_shell` are capped at what that
  session holds, a task's workspace and an automation's directories must be
  reachable from the session, and a task's turn runs under the posture pinned
  when it was created instead of re-deriving one from the legacy bit or a
  legacy mode alias.
- The approval card for creating a task or creating/updating an automation
  now lists the requested trust mode, shell, auto-approve, mode and
  workspace as labeled lines.
- "Approve for session" on a shell command now covers only a simple, known
  command family (`git status` still covers `git status -s`). Compound,
  wrapper, interpreter, unrecognised and option-configured commands are
  granted as the exact command, and interact/wait calls on a running shell as
  the exact call.
- Workflow plan approval flags shell, network and file-write capability for
  the `Bash`, `Web` and `File` tool families regardless of letter case.
- An agent session can no longer update, resume or run an automation whose
  stored `auto_approve`, `trust_mode` or `allow_shell` is more than the
  session holds, unless the same update lowers those fields.
- The approval summary every Runtime client receives for task and
  automation create/update now names the requested trust mode, shell,
  auto-approve, mode and workspace, from the same fields the TUI card shows.
- A shell session grant keeps its command family only when every option is a
  known value-free option such as `--release` or `--porcelain`; other options,
  in any spelling, grant the exact command. Families whose arguments are what
  runs or is installed (`go run`, `deno run`, `cargo run`, `make`,
  `git bisect`, `git submodule`, package installs) grant the exact command.
- Workflow plan approval recognises tool names in any quote style and with a
  pattern suffix (`'Web'`, `` `File` ``, `Web(*)`), and flags `rlm`, Git and
  GitHub, computer and browser control, and task/automation tools with the
  capabilities they carry.
- Model-run code no longer inherits provider credentials or other secrets
  from Codewhale's environment. Python `code_execution` (and every other
  Python runner), `gate_run` task gates, custom and completion verifier gates,
  persistent `terminal` sessions, and `run_tests` now start from the same
  scrubbed child environment as `exec_shell`; variables a gate declares are
  still passed through. `gate_run` also runs `/bin/sh -c` instead of a login
  shell, so profile files cannot re-export what was removed.
- A `workflow` start with `verify: true` now needs approval even for a
  read-only plan, because the completion gates run workspace build and test
  scripts.
- Git commands no longer hand provider credentials to programs that a
  workspace's git config can start (`core.fsmonitor`, hooks, filters) when
  they run through Codewhale's shared git helper: those start from the scrubbed
  environment plus the ssh agent, global config location and author identity.
  Sub-agent worktree provisioning and delivery do not use it yet. The read-only `git_status`,
  `git_diff`, history and `verify` tools also disable the workspace's
  fsmonitor, hooks and clean/process filters.
- Language servers started for post-edit diagnostics, and every Python and
  Node command constructor, start from the scrubbed child environment.
- Proxy URLs passed to model-run children (`HTTP_PROXY`, `HTTPS_PROXY`,
  `ALL_PROXY`, `FTP_PROXY`, `CARGO_HTTP_PROXY`) keep their host and port but
  lose any `user:password@` part.
- Behaviour change: the scrubbed child environment now keeps non-secret build
  configuration — `CARGO_*` (except registry and token-like keys),
  `RUSTFLAGS`, `RUST_LOG`, `RUST_BACKTRACE`, `VIRTUAL_ENV`, `JAVA_HOME`, Go
  paths, `NVM_*`/`NODE_OPTIONS` and CA-bundle variables — so `run_tests` and
  gates keep the user's target dir, job limit and toolchain. Connection
  strings such as `DATABASE_URL` are still dropped; set them in the project's
  own config, or wrap the command in a script that sets them, when a build
  needs them.
- Deny rules in the permission settings now hold for these ways of hiding a
  command word until the shell runs it: a variable (`$v`), a substitution, a
  glob or brace list, escaped ANSI-C quoting, or a shell reading its script
  from a pipe, here-string or process
  substitution. Rules in a separate `execpolicy.toml` file are applied when
  the command runs, so there such a command is refused at run time rather than
  before an approval prompt. While any deny rule is configured in the permission
  settings, such a command is refused instead of being checked against text
  the shell will rewrite (an
  approval-always policy asks instead). This also applies to common forms such
  as `source "$HOME/.cargo/env"`, `eval "$(pyenv init -)"` and `$PYTHON -m
  pytest`; name the command directly to run it. The same holds when the text
  does not parse cleanly (an unterminated quote, substitution or heredoc, a
  `case` inside `$( … )`) or when a parse budget runs out. Commands after
  `if`, `then`, `while`, `do`, `!` and similar words, `function f { … }`
  bodies, and `find -exec` payloads are now checked like any other command.
- Wrapper commands are unwrapped by their real option grammar, so
  `chroot DIR cmd`, `sudo --user NAME cmd` and `timeout -s SIG N cmd` expose
  `cmd` (and any `-c` payload) to deny rules. Options missing from a wrapper's
  table are read both with and without a value, BSD and macOS options are
  covered (`env -P`, `xargs -J`, `chroot -u`), and more wrappers are recognized
  (`caffeinate`, `arch`, `sandbox-exec`, `nsenter`, `unshare`, `runuser`,
  `flock`, `watch`, `wsl`, `noglob`, `nocorrect`), including `.exe` spellings.
  Code passed as a string to `trap`, `su -c`, `flock -c`, `script -c`,
  `watch`, `cmd /c` and PowerShell `-Command` is checked as a command line.
- `(( … ))` arithmetic no longer reads `<<` as a heredoc that hides the lines
  after it, and an `xargs` or `find -exec` replacement string used as the
  command or as shell code (`xargs -I{} sh -c {}`) counts as known only at run
  time.
- `task_shell_start` and task gate commands are now checked against shell deny
  and ask rules, like any other shell command.
- An "approve for session" grant for a shell command covers only flag variants
  of that command as written; a command with options before the subcommand,
  a chain, or nested code matches only an identical repeat.
- The read-only shell surface for sub-agents rejects a word that starts with
  an unquoted `*`, whose matches could be read as options; quote the pattern
  or give it a path prefix (`src/*.rs`).
- A trusted or allow prefix such as `git status` no longer covers options
  placed before the subcommand (`git -c key=value status`,
  `git --exec-path=… status`), nor a command that runs nested code or whose
  command word is resolved at run time. Such commands ask instead. Typed deny
  rules also match a path-qualified command word (`/bin/rm`).
- Commands containing parentheses are no longer auto-approved as parallel
  read-only commands, since some shells treat them as glob qualifiers or
  command substitution.
- Sub-agent worktrees stay under the per-repo `.codewhale-worktrees/<repo>/`
  root: an absolute `worktree_path` is now held to the same containment as a
  relative one, with symlinks resolved before the check. Any start that asks
  for a worktree keeps the approval card, even for a read-only role. A
  `worktree_base` starting with `-` is refused, and `git worktree add` now
  receives its path and base after `--`.
- Fleet and reasoning-router names must be plain file names (optionally
  `origin/name`); a name with path separators or `..` is refused before any
  file is looked up, through one shared check in the workflow crate.
- `pandoc_convert` and `image_ocr` apply the same read deny-list and
  credential-store checks as `read`, through one shared helper, and pandoc
  always runs with `--sandbox`. This needs pandoc 2.15 or newer; an older
  pandoc gets an upgrade message instead of a conversion.
- Computer Use: screenshot and zoom output paths must be `.png`/`.jpg`/`.jpeg`
  files inside the recordings directory, and zoom always crops the last
  captured raster instead of a caller-named source file.
- Skill registry sync refuses an index key that is not a single path-safe
  name before it is used as a cache directory, the same check an installed
  skill name already gets.
- A project's `.codewhale/config.toml` can no longer set `notes_path`; it is
  ignored with the other user-only keys. The `note` tool and `/note` refuse a
  notes file that is a symlink or whose directory resolves outside the
  workspace, and a note is flushed to disk before the tool reports success.
- Diffs and shows that read repository content (`codewhale review`, the
  `git_diff`/`git_show` tools, verification, delivery, task attempt records,
  `@diff` and the runtime API diff routes) share one flag set: no external
  diff, no textconv, and submodules compared by commit only, so a submodule's
  configured filters and diff drivers never run. Reads of the working tree
  (`git_diff`, `git_show`, `git_blame`, commit planning, verification, task
  attempt records, `@diff`/`@git`) also run through the hardened review
  command, so the repository's clean filters do not run either. `git_blame`
  and read-only shell `git blame` skip textconv, and read-only shell `git`
  reads neutralize the repository's clean filters.
- `git_fetch` refuses refspecs that would write a local branch or tag: a
  `src:dst` destination must be under `refs/remotes/`, and the two-word
  `tag <name>` form is rejected.
- Opening a URL on Windows goes through the URL protocol handler instead of
  `cmd /C start`, so characters in the URL are not interpreted by the shell.
- Skill downloads and the skills registry index are read through a streaming
  size cap shared with the MCP HTTP transport, instead of buffering the whole
  body before checking its size.
- Durable runtime thread, turn and item ids carry a full UUID instead of 32
  random bits, so two records can no longer collide and overwrite each other.

### Removed

- Flags, settings and tool parameters that did nothing are gone
  ([#6516](https://github.com/codewhale-hq/Codewhale/issues/6516)). `--output-mode`
  is hidden. It is still accepted, prints a warning, and is ignored.
- The dispatcher no longer exports `DEEPSEEK_*` copies of its `CODEWHALE_*`
  variables. A `DEEPSEEK_*` variable you set yourself is still read.
- `lane start` and `workflow run --runtime vm|ci` are rejected before a lane
  is created. Older lane records for those runtimes still load.
- The control socket's `relaunch` verb is removed; it always returned an
  error.
- The `speech` tool drops `stream`. `stream=true` used to fail; it is now
  ignored, a complete audio file is written, and the result no longer carries
  `"stream": false`. The `finance` tool drops `market`, and a call that still
  passes it has it ignored.
- `[context].enabled`, the seam-manager keys and
  `tui.terminal_probe_timeout_ms` no longer load; old configs that carry
  them still start. The `[workshop]` docs now describe bounded spillover
  instead of a synthesis sub-agent.
- About 2,650 lines of workflow code that nothing ran are deleted: the replay
  executor, the review-repair loop and experimental search. The
  `replay_diverged` status they produced goes with them. The isolated
  Runtime Chat prompt and the legacy YOLO alias list each have one owner now
  ([#6517](https://github.com/codewhale-hq/Codewhale/issues/6517)).

### Experience

- Typing a first message with no model connected leaves a line in the
  transcript that says the message was not sent and opens the provider picker.
- First run picks a chat-capable Ollama model instead of the alphabetically
  first tag, and says plainly when no model is available yet.
- `codewhale doctor` leads and ends with one verdict and the next step, and
  gives the update command for how you actually installed Codewhale.
  Command-line usage and errors say `codewhale`.
- The approval card leads with a plain summary of the action, such as
  "Run `cargo test`", and shows workspace-relative paths. The footer labels
  its values.
- `/status` warns when the session's pinned model is no longer in its
  provider's live model list
  ([#6035](https://github.com/codewhale-hq/Codewhale/issues/6035)).
- Error messages give one true sentence and one next step. The TUI's English
  copy says agent, Fleet, Permissions and Work consistently, help lists one
  summary per row, provider rows without a key say "needs key", `/setup` says
  what it sets up, and the pet tank rests when it is offline.
- ACP clients can see the Permissions setting the server started with,
  including Full Access and how to turn it on, but cannot select it
  ([#6310](https://github.com/codewhale-hq/Codewhale/issues/6310)).
- `GET /v1/commands` tells clients each command's argument shape, so they do
  not re-derive composer behaviour from the usage string
  ([#6230](https://github.com/codewhale-hq/Codewhale/issues/6230)).

### Fleet and agents

- `codewhale fleet run <spec> --check` runs every validation a real run would
  and stops there: nothing is created, launched or spent.
- A queued agent says why it is waiting, for example when launches are
  throttled after provider rate limits, and when it stops waiting
  ([#6277](https://github.com/codewhale-hq/Codewhale/issues/6277)).
- Read-only agents can run chained inspection commands (a leading `cd`,
  `&&`, `;`, `echo` separators, `2>/dev/null`), and a refused command now
  names the rule it broke and what to do instead. Durable Fleet workers
  accept the same read-only commands as in-session agents. An agent's time
  budget starts when it launches, and a queued agent that never gets a slot
  says it never started
  ([#6015](https://github.com/codewhale-hq/Codewhale/issues/6015)).
- Stopping an agent that writes files keeps and names the work it had
  changed, as a budget stop already did
  ([#5529](https://github.com/codewhale-hq/Codewhale/issues/5529)).
- `workflow(fleet:)` runs Fleets saved from the Fleet UI, and finds
  workspace Fleets under `.codewhale/fleets`.
- The runtime API can stop a delegated agent run from the desktop.
- A finished agent's answer is no longer cut off. Its row and its completion
  notification show the first sentence of its result instead of a
  `## Summary` heading or its last tool, and opening the agent shows the whole
  result, or the full reason it stopped, even when no transcript was captured.
  Each agent also has one name: a workflow task's label or its dispatch name
  appears on the rows, the notification and the runtime API alike, never its
  internal id ([#6565](https://github.com/codewhale-hq/Codewhale/issues/6565)).
- The mobile page shows the thread's agents: a strip naming each one, its
  state, and what it is doing or what it found, rebuilt when the page
  reconnects. Sub-agent prompt caching now counts toward the session, including
  cache-write-only reports. PRICE and `/cache` show the parent, agents and
  combined hit rates, each labelled and weighted by all input tokens,
  including cache writes,
  and the footer `cache N%` still means this conversation's own requests
  ([#6565](https://github.com/codewhale-hq/Codewhale/issues/6565)).
- The dock's GIT, FILES and NOTES views are real. GIT shows the branch and
  where it stands against its upstream, the changes (with their paths one
  Enter away), linked worktrees and the last five commits, and it keeps
  updating during a turn while it is open. It says "not a git repository"
  only when that is true, reports failed status probes in the view and composer
  instead of claiming a clean tree, counts every unmerged path as a conflict, and only
  says there are more changed paths when the list is capped. FILES lists the
  files this session edited, including writes without diff receipts, and the
  files it read, with every unique path retained beyond the activity summary's
  twelve-path preview. FILES and its badge refresh from the current session
  even before TASKS opens. Edits with receipts show their size and open their diff.
  NOTES lists your `/note` notes. The git
  badge, the Git view and the model's git line now share one
  `git status --porcelain=v2` call, so there are fewer git processes than
  before ([#6565](https://github.com/codewhale-hq/Codewhale/issues/6565)).
- Background work tells you when it ends, even between refreshes. Batched
  shell notices cover explicitly backgrounded or detached commands; foreground
  results and old completions from before this TUI session stay quiet.
  Switching sessions does not repeat a completion notice. Batched
  notices count completed, failed and stopped work separately; shell commands,
  task prompts and errors stay in the app, away from lock-screen notifications.
  A running dev server no longer holds the notice back. Failed, killed and
  timed-out shells retain their outcomes, and the last eight finished shells
  stay listed per session. Finished tasks no longer look live or reopen the
  dock. Background labels follow the UI language, long agent previews keep
  their full-result pointer, and the quiet indicator uses the engine's clock
  and limit without marking an active tool as quiet
  ([#6565](https://github.com/codewhale-hq/Codewhale/issues/6565)).

### Plugins

- Codewhale no longer appends plugin recommendations to your messages to the
  model. Suggestions appear in one place, follow one switch and one budget,
  and never advertise built-in plugins, generic words or plugins for another
  operating system.
- `/plugin dismissals` lists the plugins suggestions skip, and
  `/plugin dismissals reset [<name>]` brings them back.
- Tools from reviewed plugins that declare themselves read-only no longer ask
  for approval on every call.
- The bundled Computer Use plugin is 0.12.0, the published upstream release
  `8435692` ([#6303](https://github.com/codewhale-hq/Codewhale/issues/6303),
  [#5856](https://github.com/codewhale-hq/Codewhale/issues/5856)). On macOS the agent
  uses its own pointer and never drives your cursor. `app_script` refuses shell
  escapes. Clicks on irreversible actions such as
  pay, send or delete need confirmation. Consent decisions cannot ride inside
  `run_actions` or trajectory replay, and trajectories redact secure fields.
  Also new: a shared-computer control lease that pauses agent input while a
  person drives, and a browser attach mode for a shared Chromium. The vendored
  README no longer claims delegated agents share the Computer Use session; they
  never receive its tools.
- The bundled first-party catalog pins marketplace revision
  `ae3dd2255a9a266365c6125a084f511eb26bc04d`. It lists Computer Use 0.12.0 and
  adds Codewhale for Chrome (Chromewhale) 0.3.0 as a developer preview: you
  load its Chrome extension unpacked, and like every catalog plugin it installs
  disabled and untrusted until you review it.

### Release reliability

- Upload the complete release into a private draft, verify every asset's size
  and SHA-256 digest, then publish. An interrupted retry cannot reuse stale
  same-size bytes. CNB and GHCR version tags follow canonical publication.
- Ubuntu Lighthouse bootstrap requires trusted SSH source CIDRs or an explicit
  public-SSH opt-in before changing the host; malformed IPv6 and broad default
  networks are refused.

### CI

- State-touching command and session tests seal `HOME`, `USERPROFILE` and
  `CODEWHALE_HOME` onto temporary directories. Unsealed session, snapshot,
  artifact, composer-history, audit and built-in-plugin storage resolves into
  an isolated test root; unrelated worker threads cannot borrow another test's
  home seal. A child-process sentinel checks that known command-dispatch and
  session cases leave their ambient home unchanged.
- Fork pull requests stay under the macOS runner limit and the Actions cache
  stays under its cap.
- Release candidates and releases share one parity gate, and a release tag
  without a release-candidate receipt is refused.
- Budget ratchets block same-repository pull requests unless the pull request
  updates the budget with a receipt.
- A CodeQL advanced-setup workflow is ready for when the repository switches
  from default setup.

### Contributors

- **[@AdityaVG13](https://github.com/AdityaVG13)** — supplied the discovery-cache priority correction adapted from [#6393](https://github.com/codewhale-hq/Codewhale/pull/6393), keeping highest-ranked tools through cache overflow. Its broader echo and fork-inheritance draft remains open.
- **[@7jrxt42BxFZo4iAnN4CX](https://github.com/7jrxt42BxFZo4iAnN4CX)** — reported indefinite questions cancelled by the TUI watchdog and supplied the timer evidence ([#6872](https://github.com/codewhale-hq/Codewhale/issues/6872)).

- **[@hodeswildsmith455-boop](https://github.com/hodeswildsmith455-boop)** — added OrcaRouter account sign-in with PKCE and its live chat catalog ([#6867](https://github.com/codewhale-hq/Codewhale/pull/6867)).
- **[@LIghtJUNction](https://github.com/LIghtJUNction)** — added reviewed plugin-provided AI routes with host-owned OAuth PKCE credentials and request-time authority checks ([#6805](https://github.com/codewhale-hq/Codewhale/pull/6805)).

Contributors and issue reporters are credited below, including
@cenab's provider report.

- **[@Guan0923](https://github.com/Guan0923)** — accepted case-insensitive HTTP(S) schemes in `config doctor` without rewriting the configured URL ([#6819](https://github.com/codewhale-hq/Codewhale/pull/6819)), and routed the Python and JavaScript execution tools through the session's execution policy ([#6820](https://github.com/codewhale-hq/Codewhale/pull/6820)).
- **[@harryvgiunta](https://github.com/harryvgiunta)** — added Yolo-Auto as a bundled OpenAI-compatible host, starting on the vendor's recommended `qwen3.8-flash` model ([#6408](https://github.com/codewhale-hq/Codewhale/pull/6408)).
- **[@asto18089](https://github.com/asto18089)** — contributed the integrated runtime liveness, context, search, JavaScript execution, stopship scout and pet repairs, preserving their original contributor commits ([#6799](https://github.com/codewhale-hq/Codewhale/pull/6799)); made context rule and chain-segment source labels repository-relative ([#6739](https://github.com/codewhale-hq/Codewhale/pull/6739)).
- **[@qiuYliangM](https://github.com/qiuYliangM)** — made provider-bound project instruction and constitution labels stable across directory moves and kept their absolute paths in operator reports ([#6799](https://github.com/codewhale-hq/Codewhale/pull/6799)).
- **[@zhuowp](https://github.com/zhuowp)** — supplied the process-scoped PowerShell execution-policy repair adapted for Codewhale, preserving machine and user Group Policy precedence ([#6745](https://github.com/codewhale-hq/Codewhale/issues/6745)).
- **[@Andrea-Bruno](https://github.com/Andrea-Bruno)** — designed the Superfast Decision Gate and contributed its off-by-default shadow classifier ([#6604](https://github.com/codewhale-hq/Codewhale/pull/6604), [#6603](https://github.com/codewhale-hq/Codewhale/issues/6603)).
- **[@aiapienthusiast](https://github.com/aiapienthusiast)** — added Cheaper Inference to the bundled provider catalog ([#6761](https://github.com/codewhale-hq/Codewhale/pull/6761)).
- **[@gaord](https://github.com/gaord)** — let a client fork a thread at a named turn ([#6580](https://github.com/codewhale-hq/Codewhale/pull/6580)), let undo roll back files for the turn it is undoing ([#6483](https://github.com/codewhale-hq/Codewhale/pull/6483)), stopped resume and fork from duplicating threads and sessions ([#6406](https://github.com/codewhale-hq/Codewhale/pull/6406)), exposed user-defined provider routes to native clients ([#6404](https://github.com/codewhale-hq/Codewhale/pull/6404)), and kept a fork going when a turn lost its tool call ([#6664](https://github.com/codewhale-hq/Codewhale/pull/6664)).
- **[@Lstarsky0](https://github.com/Lstarsky0)** — moved the docs/work, legal, digest and FAQ pages onto the dictionary spine ([#6405](https://github.com/codewhale-hq/Codewhale/pull/6405), [#6417](https://github.com/codewhale-hq/Codewhale/pull/6417), [#6499](https://github.com/codewhale-hq/Codewhale/pull/6499), [#6574](https://github.com/codewhale-hq/Codewhale/pull/6574)), tightened the Chinese-branching ceiling to 18 ([#6403](https://github.com/codewhale-hq/Codewhale/pull/6403)), and made Fleet publish without a two-link window ([#6431](https://github.com/codewhale-hq/Codewhale/pull/6431)). Also moved the constitution page onto the dictionary spine and kept its install link in the selected locale ([#6733](https://github.com/codewhale-hq/Codewhale/pull/6733)), wrapped diff and tool output at grapheme boundaries ([#6829](https://github.com/codewhale-hq/Codewhale/pull/6829)), and translated the context inspector rows twelve packs still shipped in English ([#6831](https://github.com/codewhale-hq/Codewhale/pull/6831)). Translated the session-only note after model switches across the complete TUI locale packs ([#6875](https://github.com/codewhale-hq/Codewhale/pull/6875)). Translated the route-save receipts ([#6884](https://github.com/codewhale-hq/Codewhale/pull/6884)) and `/workspace` replies ([#6885](https://github.com/codewhale-hq/Codewhale/pull/6885)), brought the Operate descriptions in fourteen packs up to date ([#6886](https://github.com/codewhale-hq/Codewhale/pull/6886)), restored lost accents in pt-BR, es-419 and ca ([#6888](https://github.com/codewhale-hq/Codewhale/pull/6888)), let `semantic_truncate` cut between Han and kana ([#6887](https://github.com/codewhale-hq/Codewhale/pull/6887)), and removed the unused session-only route-save choice ([#6882](https://github.com/codewhale-hq/Codewhale/pull/6882)).
- **[@aboimpinto](https://github.com/aboimpinto)** — restored a green Linux full-workspace test gate without loosening any test, twice ([#6581](https://github.com/codewhale-hq/Codewhale/pull/6581), [#6666](https://github.com/codewhale-hq/Codewhale/pull/6666)). Completed the seventeen-command portable session group, including `/structcopy` ([#6793](https://github.com/codewhale-hq/Codewhale/pull/6793)).
- **[@dajiaohuang](https://github.com/dajiaohuang)** — `codewhale config set` checks a known setting's value against its schema type before saving it ([#6568](https://github.com/codewhale-hq/Codewhale/pull/6568)).
- **[@jayanthvee](https://github.com/jayanthvee)** — reported and diagnosed that killing the npm launcher's `node.exe` ends Windows sessions without cleanup, with reproductions and fix directions ([#6827](https://github.com/codewhale-hq/Codewhale/issues/6827)).
- **[@cenab](https://github.com/cenab)** — requested the Tsubasa provider row and supplied its endpoint, key and model values ([#6695](https://github.com/codewhale-hq/Codewhale/issues/6695)).
- **[@BX166](https://github.com/BX166)** — reported the AICraft provider row missing its key console, docs link and guidance, and supplied the values ([#6616](https://github.com/codewhale-hq/Codewhale/issues/6616)).
- **[@Water-Run](https://github.com/Water-Run)** — ingested namespaced model-only catalog entries so models present only in the canonical `models` map reach the offering list ([#6400](https://github.com/codewhale-hq/Codewhale/pull/6400)), and retired the blanket dead-code allowance with its unused feature stages, tightening the budget to match ([#6402](https://github.com/codewhale-hq/Codewhale/pull/6402)).
- **[@wuisabel-gif](https://github.com/wuisabel-gif)** — designed the `tool_call_after` execution-receipt contract and its tests on a reference branch, which landed re-implemented on the current hook seam ([#6689](https://github.com/codewhale-hq/Codewhale/issues/6689), [#6713](https://github.com/codewhale-hq/Codewhale/pull/6713)).
- **[@SparkofSpike](https://github.com/SparkofSpike)** — let making room survive a provider request-body limit (HTTP 413) by shrinking, then replacing, inline images for that one summary pass ([#6642](https://github.com/codewhale-hq/Codewhale/pull/6642)). Translated seventeen Tier-2 guides and thirteen developer and internal docs into Simplified Chinese, and connected the localized documentation ([#6662](https://github.com/codewhale-hq/Codewhale/pull/6662), [#6663](https://github.com/codewhale-hq/Codewhale/pull/6663)); added regression coverage for rejecting unknown website locales before dictionary lookup ([#6786](https://github.com/codewhale-hq/Codewhale/pull/6786)); made the pinned prompt header follow the viewport's turn and jump on click ([#6830](https://github.com/codewhale-hq/Codewhale/pull/6830)).

## [0.10.0] - 2026-09-22

Codewhale v0.10.0 brings a redesigned terminal workbench, clearer settings, and
more reliable session and runtime behavior. It supersedes the unpublished
v0.9.14 candidate.

### Contributors

- **[@AdityaVG13](https://github.com/AdityaVG13)** — fixed composer wrapping, tab/caret placement, pasted and editor-returned draft history, painted-column transcript copying, explicit terminal foregrounds, headless user-input tool availability, and engine synchronization after importing foreign sessions ([#6369](https://github.com/codewhale-hq/Codewhale/pull/6369), [#6363](https://github.com/codewhale-hq/Codewhale/pull/6363), [#6365](https://github.com/codewhale-hq/Codewhale/pull/6365)).
- **[@aboimpinto](https://github.com/aboimpinto)** — moved the TUI session-export slice onto shared command contracts (FEAT-025): a session-export contract facet with one shared sanitizer, `/export` routed through the facet, pinned with baseline-captured goldens and gates ([#6096](https://github.com/codewhale-hq/Codewhale/pull/6096)).
- **[@BX166](https://github.com/BX166)** — contributed the AICraft provider template and its documentation ([#6171](https://github.com/codewhale-hq/Codewhale/pull/6171)). It was closed unmerged, but it is what surfaced the decision to stop special-casing named OpenAI-compatible hosts ([#6289](https://github.com/codewhale-hq/Codewhale/issues/6289)).
- **[@7jrxt42BxFZo4iAnN4CX](https://github.com/7jrxt42BxFZo4iAnN4CX)** — reported the session-retention defects behind archive-past-the-cap and empty-session cap occupancy ([#6136](https://github.com/codewhale-hq/Codewhale/issues/6136), [#6137](https://github.com/codewhale-hq/Codewhale/issues/6137)), the resume-failure design behind durable transcript errors ([#6138](https://github.com/codewhale-hq/Codewhale/issues/6138)), and the gaps behind the opt-in approval timeout ([#6101](https://github.com/codewhale-hq/Codewhale/issues/6101)), `codewhale exec --hooks` ([#6099](https://github.com/codewhale-hq/Codewhale/issues/6099)), Markdown drag-copy ([#6156](https://github.com/codewhale-hq/Codewhale/issues/6156)), and the browsable, current-aware session picker ([#6014](https://github.com/codewhale-hq/Codewhale/issues/6014)); the goal token-budget hard stop ([#6013](https://github.com/codewhale-hq/Codewhale/issues/6013)) and the fleet no-progress guard shared with child workers ([#6015](https://github.com/codewhale-hq/Codewhale/issues/6015)) landed as first slices of two larger proposals, and the runtime-store session refusal ([#6207](https://github.com/codewhale-hq/Codewhale/issues/6207)).
- **[@Lstarsky0](https://github.com/Lstarsky0)** — reported TUI tests reading machine state instead of hermetic fixtures; the `lock_test_env` remedy from that report shaped two more hermetic fixes, for the shared UI fixtures and the compaction budget test ([#5359](https://github.com/codewhale-hq/Codewhale/issues/5359)).
- **[@Lujc0523](https://github.com/Lujc0523)** — reported `/hooks edit` splitting keystrokes between the editor and the composer, fixed by pausing the TUI input pump inside the editor handoff ([#6165](https://github.com/codewhale-hq/Codewhale/issues/6165)).
- **[@Statter](https://github.com/Statter)** — reported the Gemini `/models` failure that now surfaces the provider's reason instead of an empty error ([#6173](https://github.com/codewhale-hq/Codewhale/issues/6173)).
- **[@sequico](https://github.com/sequico)** — reported the ACP `session/new` ids that `session/load` could not resolve, fixed by minting resolvable session ids ([#6174](https://github.com/codewhale-hq/Codewhale/issues/6174)).
- **[@bevis-wong](https://github.com/bevis-wong)** — reported the mid-run engine freeze behind the bounded turn-end foreground-child join, and the resume path that re-ran identical tool-call repair on every load instead of persisting it ([#6184](https://github.com/codewhale-hq/Codewhale/issues/6184), [#6185](https://github.com/codewhale-hq/Codewhale/issues/6185)).
- **[@gaord](https://github.com/gaord)** — recorded the mode each turn ran in ([#6321](https://github.com/codewhale-hq/Codewhale/pull/6321), harvested), stated the approval posture a task thread starts on ([#6386](https://github.com/codewhale-hq/Codewhale/pull/6386)), and rebuilt the runtime-API thread summary in one store pass ([#6376](https://github.com/codewhale-hq/Codewhale/pull/6376)).
- **[@zhuowp](https://github.com/zhuowp)** — preserved chat roles across compaction, protected user turns on recompaction, and kept the operate contract intact ([#6286](https://github.com/codewhale-hq/Codewhale/pull/6286)).
- **[@h3c-hexin](https://github.com/h3c-hexin)** — rate-limit-adaptive subagent launch scheduling: the `DynamicGate` that replaces fixed spawn pacing under provider throttling ([#6055](https://github.com/codewhale-hq/Codewhale/pull/6055), harvested).
- **[@asto18089](https://github.com/asto18089)** — co-authored the rate-limit-adaptive launch scheduling work ([#6055](https://github.com/codewhale-hq/Codewhale/pull/6055)).
- **[@yrk111222](https://github.com/yrk111222)** — ModelScope provider support, landed through the shared `ChildGrant` fleet authority ([#6299](https://github.com/codewhale-hq/Codewhale/pull/6299), harvested).
- **[@xiechimon](https://github.com/xiechimon)** — capped child tool results at capture time rather than at render ([#6294](https://github.com/codewhale-hq/Codewhale/pull/6294), harvested).
- **[@VincentCorleone](https://github.com/VincentCorleone)** — defined and implemented the weixin-bridge Quick Start so a first run works from the README alone ([#6170](https://github.com/codewhale-hq/Codewhale/pull/6170), harvested).
- **[@Serendo](https://github.com/Serendo)** — fixed the `/config` theme editor exiting the panel on an arrow key ([#6336](https://github.com/codewhale-hq/Codewhale/pull/6336)).
- **[@yetuge](https://github.com/yetuge)** — dropped the retired `token_budget` field from the subagent documentation ([#6335](https://github.com/codewhale-hq/Codewhale/pull/6335)).
- **[@Water-Run](https://github.com/Water-Run)** — translated the local browser client guide into Simplified Chinese ([#6401](https://github.com/codewhale-hq/Codewhale/pull/6401)). Water-Run's catalog and dead-code work (#6400, #6402) landed after the v0.10.0 tag and ships in v0.10.1.
### Security
- Children never inherit desktop or computer-control tools. Desktop control is
  the most machine-wide capability in the catalog, and a verifier child
  inherited it by default: on 2026-09-17 one opened the host Terminal and typed
  a blocked shell command into the user's live session. A family classifier now
  removes those tools when a child's registry is built, so they are neither
  eager nor searchable, and `execute_full` refuses the family at dispatch for
  every child role — visibility is a grant, and hiding is not the only defense
  (#6296).
- Runtimes can be held to an organization's plugin allowlist. A managed policy
  document (`managed-policy.json` beside `state.json`, or
  `CODEWHALE_MANAGED_POLICY_PATH`) lists the plugin ids a Runtime may run,
  with an `allow_unlisted` flag and a schema version checked exactly like
  `PluginStateFile`. Enforcement sits inside `apply_state`, so a plugin
  enabled before the policy arrived — or hand-edited to `enabled: true` —
  never comes back enabled, and `enable()` re-reads the document so a policy
  landing after discovery refuses with a reason that names it. A malformed
  document fails closed rather than degrading to unenforced. With no policy
  present, behaviour is bit-for-bit what it was. This is the enforcement
  primitive only; the authority that decides the allowlist is still local,
  so it binds a cooperating Runtime, not a hostile one.

- Approving an `apply_patch` "for the session" is now scoped to the file you
  approved. The grouping key that scopes a session grant was built by a second,
  weaker patch parser that read only `+++ b/` headers and the `replace` array:
  it saw no target at all for the documented `apply_patch{path, patch}`
  override, for `--no-prefix` diffs, or for delete-only diffs, and collapsed
  every one of them to a single shared key. One approval therefore pre-approved
  every later patch of that shape, to any file, with no card and no notice. The
  key now comes from the same resolver the executor and the permission path
  already use, and an input that cannot be resolved gets its own key rather
  than a shared one (#6247).

### Added
- The statusline's performance readings survive compact mode and are separately
  configurable. Measured TTFT and average output rate are kept when space
  allows, shedding help text and secondary counts first; the existing metrics
  and statusline settings gain individual toggles with a migration that
  preserves a legacy single setting, and the picker takes mouse selection and
  scrolling.
- Tasks can be given their own run name. `NewTaskRequest`, `TaskRecord` and
  `TaskSummary` carry an optional `name`, stored as given and omitted when
  absent, so queues still fall back to prompt-derived titles; a run started by
  an automation inherits the automation's name. The `tasks` tool's schema
  extends backward-compatibly and legacy records decode through serde defaults
  (APPS-153).
- The offline catalog seeds Xiaomi's MiMo 2.6 family — `xiaomi/mimo-v2.6-pro`
  and `xiaomi/mimo-v2.6-flash` — so a first boot without network no longer shows
  the 2.5 generation. `DEFAULT_XIAOMI_MIMO_MODEL` stays on `mimo-v2.5-pro`, and
  the rows carry no price because the published rates cover only `sk-` keys.
- Native memory is a reviewed store, not a model-writable file. `codewhale-memory`
  backs the TUI with SQLite as the authority instead of Markdown, and the
  `remember` tool now only *proposes* candidates — a model can no longer write
  an active memory. `/memory` and the Runtime API commit through
  `remember_reviewed`, and a Context Lens surface (`/v1/memory/lens`,
  `/lens/actions`, `/events`) shows what was kept and why.
- Code mode (Experimental, default off): `execute_tools` runs a JavaScript
  program against a QuickJS host surface so a model can express several tool
  calls as one program. Nested calls must be read-only and auto-approved; the
  tool is hidden in Plan and refused under worker authority. Enable with
  `[features] code_mode`.
- Two new built-in providers: **ZenMux** (`ZENMUX_API_KEY`) and **CSDN 星图**
  (`CSDN_API_KEY`, Coding Plan quota billing), each with its own key slot,
  bootstrap model and catalog rows.
- The Runtime API gained the surface a native client actually needs: jobs with
  stdin, kill and cursor reads; context, secrets, git, diagnostics, targets,
  LSP and voice routes; `GET /v1/commands` for the slash-command catalog;
  `GET /v1/workspace/instructions`; account-wide `GET /v1/approvals`; plan and
  to-do inventory; `/v1/settings/schema`, with `POST /v1/config` now persisting
  every declared `settings.toml` key rather than a curated allowlist; PTY byte
  replay, resize and exit; and tool images as session artifacts.
- Codewhale holds the host's idle-sleep assertion while a turn is in flight
  (`caffeinate -i` on macOS, `systemd-inhibit` on Linux), so an unattended
  machine no longer sleeps mid-run. You will see one child process per turn.
- Skills are reachable in one call. The pinned `## Skills` index told the model
  to call `load_skill`, but the tool was deferred behind `tool_search`, so it
  was never in the tool array that instruction was printed beside: using a
  skill cost a discovery hop, a `name="list"` round trip, and a
  `change:tool_surface` re-pin of the whole cached prefix. `load_skill` is now
  eager for the parent and for children — +244B of pinned catalog, measured,
  against a re-prefill avoided every time a skill is used — and it takes a
  `query` that searches names and descriptions, so a truncated index no longer
  forces pulling the whole catalogue to find one skill. The usage line now says
  to load the matching skill by name before starting the work.
- A fast lane the router cannot serve now says so. `provider_router_candidates`
  answers `cheap: None` for any pair its tables do not know, and a
  `Faster`/`Auto` child on such a pair used to run at the parent's model and
  price with no receipt and no way to tell "single tier by design" from
  misconfiguration. The fallback stays — the router must not invent a model —
  but the spawn receipt now carries a `fallback_note` naming the unserved lane,
  the same field the pinned-provider fallback already uses. `Inherit` and
  served lanes stay quiet: this reports a fallback, it does not nag.

- The runtime API tells a replayed submission apart from a new admission:
  `POST /v1/threads/{id}/turns` answers `200` with `idempotent_replay: true`
  when the operation key was already used, and `201` for a fresh admission, so a
  client that retries after a dropped response can no longer create a second
  turn. The flag is absent on a fresh admission, so responses existing clients
  already parse are unchanged (#76).
- Event streams resume where they left off: journal frames carry their durable
  `seq` as the SSE event `id:`, `Last-Event-ID` is honoured as the cursor when
  no explicit `since_seq` is asked for, and `RuntimeCapabilities` advertises
  `event_stream_resume` so a client can gate its reconnect controls on the
  capability instead of discovering it from a missing id (#76).

- TTFT and average output rate stay visible in the compact working footer when
  space allows. `/statusline` now controls them separately; existing
  `session_metrics` settings still work.
- StepFun’s current coding catalog includes Step 5 Preview, Step 3.7 Flash,
  Step 3.5 Flash and its 2603 version, with published reasoning controls and
  PAYG rates. Existing Step Plan routing remains subscription-based.

- `read` responses now always report the file's byte size, line count, and
  whether output was truncated, and truncation footers name the total size
  alongside the continuation offset — so paging through a large file is
  deliberate instead of a surprise (#6283).
- File edits are parse-gated before the write lands: Rust goes through
  `syn::parse_file` for a grammar-exact `line:column`, and `.toml` / `.json`
  through the parsers already vendored. An edit is refused only when the file
  parsed *before* and would not parse *after* — repairing an already-broken
  file is the commonest reason to edit source at all, so pre-existing breakage
  and new files fail open. The check precedes the write, so a rejection leaves
  the file untouched and `apply_patch` cannot half-apply (#6204, #6206, #6151).
- Rust files that were already `rustfmt`-clean are re-normalized after an edit,
  so the next patch's anchors still match. Hand-formatted files are never
  rewritten, and every failure path skips and lets the edit land (#6205, #6151).
- Native clients can finish provider setup without dropping to the CLI:
  `DELETE /v1/providers/{id}/key` clears a Codewhale-owned credential through
  the same shared owner as `codewhale auth clear`, and `GET /v1/providers`
  now carries `credentialSource` / `credentialWritable` (plus a reason) so a
  client disables its control with a truthful explanation instead of letting
  a write fail late. A credential Codewhale does not own — a literal key in a
  config file, or an active external consent — refuses both verbs with `409`
  rather than appearing to succeed against a source that still wins at
  request time (#6179).
- The interactive approval card can be bounded: `[approval] timeout_seconds`
  resolves an unanswered card to **deny** when the window elapses — the same
  fail-closed decision the external approval path takes — and the transcript
  says the bound denied the call, not the operator. Omitted or `0` keeps
  today's unbounded wait, so nothing changes unless you opt in (#6101).
- Transcript drag selection copies Markdown source by default: every cell the
  selection touches serializes through the same canonical path `Ctrl-Y` and
  `/copy` use, joined with blank lines, and the toast names the copied cell
  count. A selection that does not cover every touched cell end to end copies
  the exact rendered fragment instead: rounding a pinpoint selection out to
  whole cells meant copying an entire model message (#6228).
  `tui.selection_copy_markdown = false` keeps the rendered-text payload
  (#6156).
- The Runtime API serves the workspace files a native client browses and edits:
  `GET /v1/workspace/files` lists one directory, `GET /v1/workspace/files/read`
  returns a bounded byte window with a whole-file SHA-256 revision, and
  `PUT /v1/workspace/files` writes atomically through the confined opener with
  revision-checked overwrites (409 on drift). `.git` is never served and
  symlinks are never followed. A saved session's oversized tool outputs are
  served as artifacts at `GET /v1/sessions/{id}/artifacts` and
  `GET /v1/sessions/{id}/artifacts/{artifact_id}`. (#6163)
- A session that ended mid-turn is no longer invisible to the model. The newest
  workspace-scoped session still holding a crash-recovery checkpoint is
  surfaced as a one-line `## Prior Session` notice in the session-pinned prompt
  prefix — metadata reads only, excluding the live session and any session this
  process instance created. Clean sessions get no block, so their prefix bytes
  are unchanged. Two bounded read-only tools, `session_search` and
  `session_get`, give the model workspace-scoped recall over the same store
  (one-line summaries and an 8-message tail, labeled untrusted user data, read
  on the blocking pool). Resuming stays the user's decision: the hint tells the
  model to offer a continuation, never to silently take one (#5715).
- `codewhale exec --hooks` opts a headless run into the same `HookExecutor` the
  TUI builds — global config, reviewed plugin snapshots, and trusted project
  `hooks.toml`. Headless runs previously fired no hooks at all. `tool_call_before`
  can still deny and `shell_env` still applies; a hook `ask` resolves
  fail-closed without a terminal. Fleet worker subprocesses never opt in, and
  `permissions.toml` typed rules are unchanged (#6099).
- `codewhale doctor` flags fleet and profile model pins that the provider's own
  roster no longer offers. A pin is reported only when a *fresh* cached live
  roster for that exact route exists and omits it — stale, failed, or absent
  rosters prove nothing and are counted as `unverifiable` rather than raising a
  false warning. Each row names the route and every owner of the pin; the pin
  is surfaced, never rewritten (#6035).
- Background-capable clients can enumerate owned work and watch TUI-visible
  conditions: `GET /v1/threads/running` lists threads with queued or
  in-progress turns in one call (one turns scan grouped by thread), and
  `GET /v1/threads/{id}/notices` serves active `subagent-terminal`,
  `elevation-needed`, and `model-notify` notices with thread/turn identity,
  cleared by ack or — for elevation — when the tool call completes (#6180,
  #3757).
- Sub-agent launches adapt to provider throttling: a `DynamicGate` replaces
  the fixed semaphore so launch capacity adjusts at runtime, and a
  `RateLimitGovernor` halves capacity on 429 pressure, pauses admissions
  past the threshold, and recovers additively; 429 retries honor
  `Retry-After` with jittered backoff, and quota exhaustion keeps the
  failure path (#6055).
- Turns record the mode they ran in, so mixed-mode sessions stay legible
  after the fact (#6321).
- Shell spawning refuses NUL bytes in command and cwd before spawn, and
  sub-agent runs fall back loudly past credentialless profile provider
  pins instead of misrouting silently (#5529, #6318, #6320).
- Children land past a per-step context bound instead of burning
  quadratically, and status rows surface live declared-vs-observed writes
  (#6189, #6194).
- Queued Agent Mail can be cancelled before delivery, and the TUI
  suspend/resume handshake restores on stop and rebuilds on continue
  (#6176, #6169).
- MCP connections are supervised: dead servers are probed and reconnected
  with transitions reported, and a failed reconnect keeps the last-good
  catalog instead of dropping tools (#6187, #6142).
- Web search autodetects Tavily from `TAVILY_API_KEY` (Firecrawl stays the
  default), fleet refusals name the alternative, and verification runs on
  a bounded Git fetch plus a `merge_tree` verify surface (#6298, #6296).
- Fleet authority projects through one `ChildGrant`, and ModelScope joins
  the built-in providers (#5633, #6299).
- Child tool results are capped at capture time, and run
  tests/verifiers accept a bounded cwd (#6282, #6294, #6296).

### Changed
- SearXNG results rank by the score the instance returns rather than by arrival
  order. Integers, floats and numeric strings are accepted; anything else,
  including NaN and infinity, becomes 0.0, and rows are stable-sorted descending
  before the result cap, so equal scores keep instance order. The docs now spell
  out the self-hosting requirement: the separate process must expose
  `search.formats: [json]` — an HTML-only instance answers 403 — and bind to
  loopback or a policy-allowed host, with no default instance discovered.
- The bundled skills move to a new generation. social-media and health leave the
  shipped pack (phone-export workflows rather than everyday skills), feedback
  moves to `docs/skills/` beside contributor onboarding, and the exact earlier
  body is retained so only unmodified shipped skills upgrade — a skill you have
  edited is left in place. Google OAuth scopes and client setup, Photos exports,
  forgetting limits, Spotify playback and plugin reload guidance were corrected
  in the same pass.
- `auto` is a declared default, not a guess about your wording. Reasoning
  effort no longer maps request vocabulary to tiers (debug/error to Max, search
  to Low) and Auto routing no longer infers cheap-versus-big from phrasing:
  both resolve the configured default, with `[auto] cost_saving` as the
  explicit opt-in. The same prompt now costs the same thing twice.
- A workflow's shared token budget is opt-in. `[workflow] default_token_budget`
  applied a silent 120,000-token cap across a run and all of its children, and
  a fan-out died at the limit with no hand-back; the default is now 0, meaning
  no shared cap.
- The shipped `deepseek-flash` route speaks the Responses endpoint, and `xhigh`
  effort maps to `high` per the vendor's own table.
- Streamed text is paced at a steady rate rather than inheriting the provider's
  SSE chunking, so output reveals at a readable beat instead of in bursts.
- Broadening shell access for a conversation now requires an idle conversation
  and rejects a stale-workspace check before it commits. Engines advertise a
  `thread_shell_consent` capability, so an older Engine reads as unsupported to
  a native consent client instead of silently accepting.
- One base prompt now serves every host. `HEADLESS_BASE_PROMPT` was a second
  hand-maintained rendering of the same constitution — the drift pattern this
  repo forbids by convention — so headless runs compose the same `BASE_PROMPT`
  plus language and output layers that interactive runs do, skipping only host
  chrome (execution profile, authority recap). Headless and interactive can no
  longer disagree about what the agent is.
- `BASE_PROMPT` gains a **Bearing** article, which changes how the agent talks
  to you: the user is a peer who gets honesty rather than deference, a blocking
  gate is named plainly instead of dressed up as refusal, bad code is called
  bad, a crude request is carried out without a lecture, and an apology appears
  only when there is something to apologize for — not as punctuation. It also
  states that the request is the whole mandate, so the scope law opens with
  what is yours to do before it reaches the edge. Every pinned kernel invariant
  survives verbatim.
- The ocean reads as animals rather than a mechanism. The school used to
  translate as one rigid body — bob phase and tail pose were staggered per
  fish, but horizontal position was locked to an exact wedge offset — so each
  fish now eases a dot fore and aft of its slot on its own slow period, and the
  formation breathes while it travels. Bubble emission and the per-animal
  periods are hash-jittered instead of sharing one clock. Amplitude stays an
  order of magnitude under the crossing speed, so no fish travels against the
  school and `facing == velocity` still holds by construction. The species list
  is unchanged.

- Extensions keeps the exact-content plugin review on the panel: confirming
  a bundle's digest re-reads the inventory, so the row you just reviewed
  reports its new trust state and offers Enable instead of leaving you in
  the transcript with a stale "not reviewed" row.
- Underwater motion ticks at the cadence the frame limiter actually draws
  (the atmosphere interval while only the water moves, the authored 80 ms
  ocean cadence inside the interactive cap while a turn streams), and the
  event loop wakes exactly for the next tick instead of on the next idle
  poll. Idle water no longer requests frames it cannot draw or quantizes its
  cadence to the poll interval; reduced motion, Ghostty, tmux and the
  six-second idle settle are unchanged.
- The launcher keeps the Codewhale mark while balancing its layout above the
  composer. A single cursor identifies the selected action; MCP faults retain
  their warning color even in compact terminals. Recent-session counts now
  read correctly for a single message.
- Model and provider settings use quieter selection surfaces, one focused
  cursor, clearer missing-key warnings, compact output limits, and less
  repetitive credential detail. Plugin actions use plain labels.

- The terminal opens on **Shoreline**, the same palette the GPUI client already
  uses: warm charcoal field `#211F23`, a raised plate for panels and the
  composer, one blue for action and selection `#90B9FF`, and the whale's ivory
  `#F2ECE5` for body text, with 4.5:1 floors on every muted step. The old
  saturated navy gradient is not gone — it is a named `underwater` theme, and
  after review it is once again what a fresh 0.10.0 install opens on
  (`DEFAULT_TUI_THEME`); Shoreline is one `/theme` away. Existing installs keep
  whatever theme they have saved (#6222).

- Menu navigation is starting to mean the same thing everywhere. `menu_style`
  already single-sourced how a selected row *looks*; what a key *does* was still
  reinvented per surface — `h`/`l` in the provider picker against `Left`/`Right`
  in the model picker one screen later, `Home`/`End` in one of seven pickers, and
  no paging at all in Fleet's detail view. `list_nav`, which already owned the
  wrap arithmetic, now owns the vocabulary too: one vertical axis, one
  horizontal axis, and two entry points so a picker with a live filter never has
  a letter stolen out of its query. Fleet's detail view adopts it first and
  gains PageUp/PageDown and Home/End in both its lists (#6290).

- Reviewed plugin bundles are no longer re-hashed four times per MCP dispatch.
  `verify_plugin_authority` walks and hashes both the reviewed source and the
  runtime snapshot, and four separate authority checks ran per `tools/call` —
  eight tree walks. Three of them sat one statement after a
  `validate_before_use` on the same source, so `is_ready` re-verified what had
  just been verified; readiness and authority are now separate, and only the
  callers with no preceding check still pay for both. The tool catalog is built
  once per turn instead of twice, which also removes a case where the two
  assemblies could disagree if authority drifted between them. The digest is
  deliberately **not** cached on `(path, mtime, len)`: the reviewed tree is
  user-writable and `utimensat(2)` lets a same-uid process restore an mtime
  after an equal-length rewrite, so a stat-keyed cache would serve a pre-tamper
  digest (#6209).
- A sub-agent's completion is read from the manager once instead of polled. The
  workflow pump re-read it up to fifty times, sleeping 20ms between attempts,
  waiting for a terminal status that was already committed — every publisher
  commits the status inside the same `&mut self` call that wakes the pump, so
  the write guard spans both and the first read always sees it. A child the
  manager had no record of cost a full second of head-of-line blocking before
  failing; it now fails immediately, and says what actually happened instead of
  claiming the child "did not report a terminal status within 1s" (#6211).

- MCP protocol negotiation: every surface advertised the original 2024-11-05
  revision and the stdio client required an exact match, so newer servers
  could not connect. The server and both clients now advertise 2025-06-18
  and negotiate over the supported set (2025-06-18, 2025-03-26, 2024-11-05)
  — the server echoes the client's revision when it is supported and answers
  with the latest otherwise, the stdio client accepts any supported revision,
  and streamable HTTP sends the required `MCP-Protocol-Version` header on
  every post-initialize request (#6280, first half).
- Configured MCP servers now connect lazily instead of all at session boot. The
  pool owns a `connecting` set marked at spawn and cleared on resolution or
  abort, so "connecting" is no longer inferred as enabled-minus-connected. The
  boot pass scopes to the eager set — `required` servers plus those covered by
  `tools.always_load` / `allowed_tools` — and a turn naming an unstarted server
  spawns its connects alongside, under the existing five-second deadline. A
  configured-but-unstarted server now reads as configured on every surface
  (session-boot rows, Extensions tab, launch card), never as connecting.
  `docs/MCP.md` documents the lifecycle (#6033).
- The launch card's MCP problems row runs its own remedy. It already printed
  `/mcp login <name>` or `/mcp`; it now joins the shared paint/click/keyboard
  ordering, so Up/Down lands on it and Enter or a click types the printed
  command into the composer for you to send. Typing beats copying: no clipboard
  dependency over SSH, and you see the command before a second Enter runs it
  (#6085).

- Computer Use is the only computer-use product in Extensions and
  `/mcp recommendations`. Cua is no longer suggested as a parallel
  desktop-control MCP; enable the first-party `computer-use` plugin
  instead. The bundled plugin is 0.11.2 — macOS beta, with the Windows and
  Linux backends experimental and source-only. It carries the per-app consent
  ledger, background-by-default control, and a focus lease that refuses rather
  than stealing the foreground, alongside `get_app_state`, `focus`/`get_value`
  and `stop_computer_control`. Shared-desktop pointer gestures stay gated, and
  arbitrary background dragging remains unavailable.
- The bundled first-party catalog pins marketplace revision
  `d8640b17f27542e7122c76368724196f92a0af61`, so installing Computer Use from
  the Extensions listing fetches the same 0.11.2 source the binary already
  carries.
- Materializing is not enabling: the built-in bundle is written under
  `$CODEWHALE_HOME/builtin-plugins` on first run but stays `NeverReviewed`,
  so Computer Use is never switched on without an explicit capability review.

### Removed

- The host no longer parses prose into goals. Ten phrasings and a clause
  allow-list ("make it your /goal to …") were turned into durable goals before
  the provider call; prose now reaches the model, which calls `create_goal` when
  a goal is useful. The deterministic path — a leading `/goals <objective>` — is
  unchanged.

### Fixed
- A thinking fold is an absolute choice again. The stored bit was relative to the
  display preference (`folded ^ !(verbose || thinking_default_expanded)`), so
  every recorded choice flipped meaning the moment a preference changed: turning
  `thinking_default_expanded` on closed a block the reader had explicitly
  expanded (#5847). An explicit tri-state intent now records Expanded or
  Collapsed outright, and the absence of an entry means untouched, so the
  preference baseline decides that cell's default. Space still toggles, and an
  untouched cell still follows `verbose || thinking_default_expanded`.
- A terminal byte-stream cursor past the head is clamped instead of echoed back.
  `read_since` returned a future cursor as `next_cursor`, so a client that
  continued from it skipped every byte the stream produced before reaching that
  position — permanently. The start position now clamps to `total`: a future
  cursor reads nothing, is not a gap, and hands back the head.
- Language-server startup is bounded and a failed transport now terminates. The
  client waits for successful initialization before sending notifications,
  drains stderr without buffering it, bounds request queueing and replies, caps
  protocol frames, and fails pending requests when the child transport dies.
- Input no longer freezes for the rest of a turn when the engine's 32-slot op
  mailbox is full. The remaining input-path sends no longer await: droppable ops
  whose rejection is reported and retryable use `try_send` (CancelSubAgent,
  PreviewOutboundRequest, bang shell input, PurgeContext, and the single-op
  settings updates), ops that must land once the UI changed reserve first, and
  `ChangeMode` publishes its live authority even on a full channel. Must-deliver
  ordered transitions still await, and say why at `EngineHandle::send` (#6150).
- The pet's whale is one body again. The same authored point set is checked in
  three copies plus four fixtures, and the Rust and TypeScript cores disagreed
  on particle positions from frame 0 while agreeing on every channel and
  constant — so the v1 fixtures had never matched what Rust produced and the
  conformance job had never passed. The bodies and the fixtures are reconciled
  and that check now runs green.
- The context meter and the auto-compact gate share one honest estimator. The
  status bar inflated `ctx %` by about half and disagreed with the gate, so
  "ctx 82%" could sit beside a `/compact` that refused to run; displayed
  percentages now read materially lower because they are correct (#6297).
- A steer the engine never accepted is queued for the next turn instead of
  being shown as held and then silently dropped with no turn and no answer
  (#6297).
- Every `reqwest` client routes through `codewhale_release::tls`. A bare
  `Client::builder()` panics under rustls with no installed provider; 17 call
  sites were swept.
- The bundled OpenAI-compatible hosts have their `/provider` rows back.
  Retiring the `ProviderSetupTemplate` layer moved SenseNova, Baseten, Groq,
  Cerebras, DashScope and Command Code into `provider_descriptors.json` and
  then wired that file to nothing, so six vendors silently lost their picker
  rows; AICraft never had one. Each descriptor is a row again, built through
  the same named-custom-provider builder a configured host uses, so endpoint,
  bootstrap model and "missing `<ENV>`" reporting all come from one place. A
  descriptor whose id or alias already names a `[providers.*]` entry is
  dropped, so a configured row stays the only one. These rows are an
  invitation, not a route: they are not `is_configured`, and only the form's
  submit writes anything (#6289).
- A StepFun Step Plan subscription reaches its own catalog. A subscriber's
  `base_url` is `https://api.stepfun.ai/step_plan/v1`, but only the
  pay-as-you-go `/v1` host was recognised as official, so the Step Plan host
  read as a custom endpoint, the catalog was withheld, and the picker showed
  `0 bundled` and a guessed context window for a route whose console
  advertises `step-5-preview` at 1M context. The model was in the seed the
  whole time. All four StepFun hosts — global (.ai) and China (.com), each
  with a `/v1` and a `/step_plan/v1` surface — are now recognised; a host
  StepFun does not own stays custom and keyless, since this predicate also
  scopes credentials.
- The send cue stopped strobing while you type. `[↵]` flickered between dim
  `[·]` and bold blue once per character at an ordinary typing pace. The cue
  was not lying — Enter really does insert a newline during the ~120 ms
  paste-safety window, which every keystroke re-armed — but that window only
  exists for terminals that deliver a paste as a burst of ordinary keystrokes.
  Ghostty, iTerm2, WezTerm, Windows Terminal and Terminal.app now skip the
  heuristic from the first keystroke instead of waiting for proof by paste.
- Shift+Tab sets the permission posture in Plan. Tab cycles the mode and
  Shift+Tab cycles Ask/Auto-Review/Full Access, but Plan refused the second
  one outright, so the key silently did nothing there and the two axes read as
  welded together. Plan's read-only guarantee comes from the mode, not the
  posture — `authority` maps `(Plan, _, Bypass)` to `SandboxPolicy::ReadOnly`
  and `tool_catalog` gates every write tool on `mode != Plan` — so the cycle
  now moves the durable Act/Operate baseline while Plan's live policy stays
  `Suggest`, and a toast says the posture lands when the mode leaves Plan.
- A steer delivered mid-turn now clears its pending card. The transcript
  showed the message and the model answered it, while the "sending into this
  turn" card still listed it as pending: `turn_loop` commits a steer trimmed,
  but the UI handed the engine the untrimmed text and kept that same copy for
  matching, so a composer newline made the two differ by whitespace alone and
  the steer was never promoted to a transcript cell. The text is trimmed once
  at the source that feeds both, so they are the same string by construction;
  the match stays an exact `==`.

- A Xiaomi MiMo key no longer fails verification with another vendor's 401.
  The legacy top-level `base_url` is a DeepSeek field, and a route without an
  endpoint of its own inherited it — so a config carrying
  `base_url = "https://api.deepseek.com"` made that host the MiMo route's
  endpoint. The custom-endpoint guard correctly withheld the MiMo credential
  from a host the route does not own, and the setup wizard's probe, which
  bypassed the guard, sent the key as `api-key:` — a header DeepSeek ignores.
  The result was `HTTP 401 Unauthorized: Authentication Fails (governor)`,
  DeepSeek's *unauthenticated* 401, shown to someone who believed they were
  testing their own key. Every layer that reads the root now inherits it only
  when the URL belongs to that provider's own official endpoint family;
  DeepSeek, which owns the field, is unchanged, and any other host is still
  expressible on `[providers.xiaomi_mimo] base_url`. Key verification
  failures now also name the endpoint that was probed, so a wrong endpoint
  reads as a wrong endpoint instead of a bad key.
- The Extensions panel can act on the rows it shows. Every plugin now carries
  its on/off switch — Space, or `e` — including one that has never been
  reviewed: `/plugin enable` already routes an unreviewed bundle into the
  exact-capability review and only flips the switch once you confirm the
  digest, so withholding the control bought no safety and left the shipped
  `computer-use` bundle reading `disabled` with nothing that could enable it.
- The panel stopped saying "untrusted". A bundle that is waiting on its
  capability review now reads `needs review` — `off · needs review` when the
  switch is off too — and its detail line says what Codewhale is withholding
  and which key ends the wait, instead of restating an internal pair of
  booleans. Enter's verb on that row is `review`, not `inspect`.
- MCP servers say where they live, and mutations write there. Each row is
  labelled `global`, `project`, or `plugin: <name>`. Enabling, disabling or
  removing a workspace-scoped server previously edited the user's global file
  and failed with "MCP server '<name>' not found" on a row the panel had just
  offered; the write now lands in the file that declares the server, and a
  plugin-contributed server says it is switched off by disabling its plugin.
- The permanent "Plugins · Problems: N" footer chip is gone. It could not be
  acted on from the footer, and its count and the Plugins tab disagreed by
  construction — the chip counted bundles waiting on a review while the tab's
  Problems group lists only registry-level diagnostics, so the rows behind the
  number were never the rows you found. A bundle waiting on a review is a row
  with an action, and `/plugins` states it per bundle next to the key that
  resolves it.
- The Extensions marketplace no longer advertises what it cannot install.
  Browser Use and the sandbox runtime were pinned rows whose only possible
  action was `unavailable`; they are removed, along with the browser-driver
  and sandbox-runtime component vocabulary that existed solely to describe
  them.

- Configuration parsing keeps the parsed base config boxed, so loading a
  profile no longer carries the full `Config` by value through the
  deserializer and overflows a default 2 MiB test-thread stack; the
  runtime-store binding test that also overflowed is split into phases and
  pinned to that budget so CI's larger stack cannot mask a regression
  ([#6362](https://github.com/codewhale-hq/Codewhale/issues/6362)).
- Stopping a turn revokes its pending approvals. A late approval cannot resume
  the cancelled action or save an automatic approval for later turns.
- Expanding and collapsing selected reasoning now matches its rendered state
  when verbose mode and the default-expansion preference are both enabled.

- Branch navigation preserves sibling histories, stable entry IDs and timestamps
  through autosave, resume and forks, and synchronizes the selected branch into
  the live engine. Thanks to @7jrxt42BxFZo4iAnN4CX for the report (#6367).
- Compaction now writes local diagnostic records for completed passes and
  automatic refusals. `codewhale metrics` reports them with summarizer token
  usage; text-mode exec retains its session after attempting compaction.
  Historical artifacts are not backfilled. Thanks to @7jrxt42BxFZo4iAnN4CX
  for identifying the missing producer (#6368).
- The opening workbench retains the canonical Codewhale mark in a responsive
  header while keeping recent-session titles full-width and actions clickable.
- `serve --acp` no longer ignores the approval posture. The ACP adapter
  hardcoded `ApprovalMode::Suggest`, so `--yolo` (and any configured
  `approval_policy`) never reached admission: every mutating tool parked
  behind a permission request no unattended client answers, and sessions
  executed zero tools. Posture now derives from server config — `--yolo`
  pre-approves prompts and flows into the sandbox policy — while hard blocks
  (safety floor, repo law, reviewer consult) and the Plan read-only guardrail
  stay intact (#6337).
- Plain agent spawns could not resolve. `built_in_members()` seeded both
  `general` and `worker`, and the role parse boundary migrates `worker` to
  `general`, so both canonicalized to the same role — and `role:general`, the
  selector the roster advertises for the default posture, matched two members
  and raised `Ambiguous` every time, permanently. The duplicate built-in is
  gone. The legacy name still resolves: `general`, `member:general`,
  `role:general` and `default` all land on the `worker` posture through the
  identity selector rather than through a second member, which is what allowed
  the duplicate to be removed (#6244).
- Clicking a `path:line` in tool output no longer spawns `$EDITOR` detached
  while the TUI still owns the terminal, and no longer spawns one editor per
  matching line. The launch goes through the single terminal-handoff path, and
  a click is one request to open one file (#6235).
- A write-scope contention refusal now names a remedy that works. The `agent`
  tool's description claimed `release` was "the remediation a write-scope
  contention refusal names"; the refusal did not name it, and pointing back at
  it would have been worse, because `release` only clears claims whose owner is
  no longer running while a contention refusal names a live one. The refusal
  itself now says to wait for that owner to settle or cancel it (#6272).
- The session picker no longer refuses a saved session whose Runtime store
  exists but holds nothing. A force-quit leaves the store on disk, ownerless
  and empty, and the switch path refused it because recovery only covered a
  *missing* store. A switch now also adopts a store that is provably empty
  (every work directory, plus the event sequence that remembers pruned
  appends) *and* provably unheld (the process-owner lock, which a live
  manager holds from open to close), with no automation pinned to its
  execution scope — and the save gate treats that shape as abandonable too,
  so the repaired binding persists. The first fix was reverted on a race
  (emptiness without liveness); this reland checks the lock first (#6207).
- Double-tap Enter now sends every queued follow-up into the running turn,
  oldest first. The second Enter used to steer only the most recent message
  and leave older ones queued; a failed steer restores the failed message
  plus everything unattempted in original order, so nothing is lost or
  reordered.
- Only the most recently sent prompt carries the elevated-surface background
  now; every older prompt renders on the bare ground. The fill used to sit
  behind every user row (striping), then behind none; newest-only keeps the
  eye on the turn in play. Sending a new prompt moves the highlight and
  un-highlights its predecessor.
- Diff rows tint whole: added/deleted line numbers now share the row's green
  / red background instead of sitting bare next to a painted body. Context
  rows stay on the bare ground.
- MCP connections are supervised now: a background task notices a dead
  server within one sweep, reconnects on the existing backoff ladder, and
  reports each transition, so Extensions rows flip with liveness instead
  of parking on stale-ready or a silent [reconnect]. Five consecutive
  failures park the server with a notice naming `/mcp retry`; an explicit
  retry or a fresh connection resumes watching. Tool calls also retry
  once across a dead pipe/socket (not just stale sessions), and a
  reconnect that fails reports both errors instead of swallowing the
  original (#6187; `list_changed` catalog refresh stays open).

- A steer the engine never delivered is no longer reported as sent. The runtime
  API persisted the steer item as already-`Completed` and emitted
  `turn.steered` + `item.completed` the moment the text entered the engine's
  mailbox — before the engine decided anything. The engine discards a steer
  whose turn has moved on, and an interrupted or failed turn drops whatever it
  had queued, so a GUI could show "Guidance sent", clear the composer, and lose
  the user's words. The engine now returns a verdict for every steer on every
  exit path, the item settles `completed` or `canceled` to match, a dropped
  steer emits `turn.steer_dropped` and answers `409` so a client can resend,
  and `steer_count` counts steers the model actually received (#6276).
- `<recommended_plugins>` suggestions stop nagging: a plugin id is now
  injected at most once per engine lifetime, and a plugin whose name a
  loaded skill already covers is never suggested — the local skill owns
  the domain, so the nudge was noise. Dismissals still apply, and the
  fragment stays append-only on the user turn (#6274).
- A canceled automation run now settles with a transcript receipt that names
  the cancellation (by request, cancel timeout, or shutdown) instead of
  vanishing from the live band silently. The receipt wears attention ink and
  never lights the failure demand; the run record keeps the cancellation
  reason as its error detail. (#6162)
- A failed workflow run no longer settles silently: its terminal failure
  raises a sticky error toast naming the cause (dispatch, schema, or script
  errors), alongside the existing panel state (#5528).
- MCP OAuth re-login now forces the provider's consent screen: logout only
  clears the local token, so without a prompt the provider silently
  re-granted the same account/workspace and a re-login could never change
  it. `/mcp logout` and `codewhale mcp logout` also say plainly that they
  clear local credentials only (#6040).
- Session retention no longer deletes transcripts once the store reaches the
  cap: the oldest active session is archived — still openable from the
  picker's archived view — instead of being unlinked, and archived records
  sit outside the cap until they are pruned (#6136).
- Empty auto-created "New Session" stubs are capped separately (the ten
  newest are kept) and can no longer occupy a real transcript's slot in the
  session cap (#6137).
- A failed resume or session load is now a durable transcript error instead
  of a status line the next footer update replaces, so a resume that cannot
  restore its target no longer looks like a silent new session (#6138).
- Compaction no longer retains a tool result whose tool call was summarized
  away: an older turn that mixes text with a tool result keeps its text and
  drops the orphaned result blocks, which providers reject outright (#6119).
- Automation runs that need a tool approval no longer die as silent
  idle-timeout cancels: a pending approval suspends the idle watchdog for its
  decision window, and an unanswered window settles the run Failed with the
  recorded reason instead of a silent Canceled (#6118).
- `/mcp` no longer freezes the console while a turn is running: the panel
  opens immediately from the last known MCP snapshot with a receipt naming
  the wait, and live-pool mutations say their refresh is deferred instead of
  parking the UI event loop behind the running turn (#6159).
- MCP OAuth login no longer fails with "Authorization server response missing
  required issuer" against servers that implement RFC 9207, such as
  Cloudflare's `mcp.cloudflare.com`. The local callback listener now keeps the
  `iss` parameter from the redirect and hands it to the token exchange so the
  callback binds to the discovered issuer; servers that do not send `iss`
  keep working unchanged. (#6157)

## [0.9.13] - 2026-09-13

Codewhale v0.9.13 addresses integrity issues in 0.9.12:
multiline paste is one paste again, truncated tool arguments can no longer execute, strict
ACP clients connect again, concurrent instances stop destroying each
other's queued text, and the Computer Use bundle includes plugin 0.3.1
with an accessibility-first pointer that no longer steals focus. DeepSeek V4.1 Flash
(`deepseek-flash`) is the default DeepSeek model, reasoning-capable routes
keep reasoning out of the answer even when a model id carries no version
number, and `/mcp reload` no longer freezes the interface while servers
reconnect. The Codewhale pet arrives with `/pet`: a full-screen habitat that
shows what the Engine is doing and reveals the answer when it is done.

### Added

- `/pet` turns the terminal over to the Codewhale pet. `/pet on` (or bare
  `/pet`) gives the habitat the whole content viewport now and on every
  accepted turn, reveals the actual answer or error when the turn completes,
  and Escape returns to the composer without cancelling anything. `/pet off`
  closes the view and stops automatic entry while the durable companion keeps
  the pet alive; `/pet appearance|window|source|sound|export|status` address
  the shared companion. The pet no longer lives in the workbar: the Watch
  panel and `/workbar watch …` are gone (#6109, #6110).
- Codewhale Computer Use 0.3.1 ships as its own notarized Mac app. Download
  the disk image from [codewhale.net/computer-use](https://codewhale.net/computer-use)
  or the [v0.3.1 release](https://github.com/Hmbown/codewhale-cu-plugin/releases/tag/v0.3.1)
  (`Codewhale-Computer-Use-0.3.1-macos-universal.dmg`, drag into
  Applications; the ZIP stays for the in-app updater). The bundled plugin and
  the first-party marketplace pin the same 0.3.1 sources, so the app, the
  `computer-use` plugin and `/mcp` see one implementation.

### Fixed

- The website's Computer Use download page resolves its state without the
  GitHub API (using `GITHUB_TOKEN` only when bound), and every page regenerates
  on the Worker again: the Open Graph image route read brand SVGs at import
  time, which the Workers runtime cannot do, so codewhale.net had been serving
  its build-time snapshot.
- Operate can run structured workflows directly, with named phases, model
  assignments from Fleet, prerequisite results and shared budgets. Independent
  steps run together; dependent work waits for its required results and gates.
  Detached runs return their outcome to the owning conversation, and headless
  sessions stay alive between phases until the final handback is consumed.
- Computer Use 0.3.1: mouse actions no longer steal focus or reclaim the
  foreground when the user switches apps mid-action; background typing,
  scrolling and selection use semantic input, and screenshots stay scoped to
  the targeted app. The bundled plugin and the first-party marketplace pin
  carry the same 0.3.1 sources. A registered macOS helper stays in charge of
  input through its Pause and Stop controls; an unavailable registered helper
  produces an error instead of silently bypassing those controls.
- The Fleet editor uses the standard model picker to manage sub-agent model
  and thinking assignments. Enter edits the selected row without changing the
  running session's model. Unconfigured providers are refused, failed saves
  retain the previous assignment, and a changed or removed team file must be
  reopened before a pick can overwrite it.
- The provider catalog includes Baseten and the other compatible-provider
  templates as selectable rows, opening their existing prefilled setup forms.
  DeepSeek routes with clock-based pricing show the current peak or off-peak
  tier beside session cost, with translated labels.
- Extensions, teams, workflows and automations support mouse-wheel scrolling.
  Plugin and MCP rows have keyboard enable/disable controls and two-step
  removal; MCP OAuth can retry with narrower scopes after a scope rejection.
- Healthy sub-agents continue after an ordinary parent reply. Headless runs
  keep the existing Engine alive for child results within the run deadline.
  Explicit cancellation remains authoritative when result queues are full or
  a completion starts a followup turn.
- Sub-agent followup supports multiple targets and all parked children, keeps
  old IDs connected to their current continuation, and saves continuation
  identity before starting work. Repeated followup does not fork duplicates.
- Sub-agents validate declared output files and distinguish real edit claims
  from file citations and unrelated workspace changes. Disjoint file claims
  can run together; overlapping writers receive the actual conflict and remedies.
  Explicit read-only shell analysis requires an enforcing native sandbox and
  refuses execution when that protection is unavailable.
- Delegation depth stays absolute through saved profiles, nested workers and
  continuations. Per-call token, step and time limits narrow inherited limits;
  continuation retains ancestor usage and deadlines. Workers reserve room for
  one tools-disabled partial report inside those limits, then run the declared-
  output checks. Missing usage or unavailable reporting room produces an
  explicit fallback; partial work is never marked complete.
- Agent rosters and detail pages have bounded output, visible continuation and
  descendant relationships, and usable handles for full diagnostic evidence.
  Completion receipts include measured worker and descendant token usage,
  count each continuation once, and distinguish unreported usage from zero.
- Localization and native helper builds resolve the active checkout when the
  build script runs, so a shared Cargo target keeps working after a worktree
  moves or is removed.
- Selecting a saved agent profile that is malformed, unreadable or duplicated
  now fails before any child request, including when its name matches a
  built-in role; the parent's default route is never substituted silently.
  `agent(action: "roster")` lists affected profile identities and paths, Fleet
  run creation performs the same check, and `docs/SUBAGENTS.md` documents the
  valid personal profile format with `[permissions]` (#6117, thanks
  @Gabriel-Degret).
- Interactive startup no longer mistakes worker scheduling delays for an
  unresponsive terminal. Terminal ownership checks and shutdown cleanup remain
  enforced (#5929).
- Interrupted conversations whose saved runtime store is missing recover into a
  fresh scope without restoring old tasks or approvals. Stale session saves
  cannot resurrect the broken binding (#6102).
- Permission checks distinguish literal heredoc data from executable commands,
  including substitutions and shell stdin (#6098).
- Automatic compaction runs quietly from live context pressure, preserving the
  current task and recent tool exchanges while keeping the system/tool prefix
  stable. Original history and the handoff are saved before context replacement;
  failed or canceled compaction retains the conversation (#5620, #6047).
- Custom and gateway providers can override context limits for each exact model,
  so switching models also switches the meter and compaction budget (#6108).
- Plugin suggestions explain their matching term and remember explicit dismissals
  across restarts. Generic words and repository-host domains no longer trigger
  unrelated installation prompts (#6031).
- Plugin trust and automation deletion have keyboard and mouse confirmation
  controls bound to the exact reviewed content; users can still copy the command
  and changed content requires a fresh review (#6039).
- The model-facing MCP start tool can reconnect an existing configured name after
  login without changing its credential key or restarting healthy siblings (#6030).
  A decreasing token-expiry countdown no longer makes a revoked credential look
  like a new login from another session.
- Gemini setup uses the official endpoint's supported reasoning-effort field,
  avoiding the rejected top-level Google thinking object. Gemini 2.5 and 3 keep
  their supported effort ranges; signed tool history still survives reasoning
  changes and restart (#6018, thanks @vmakarov-uk).
- Missed automation occurrences coalesce without overlapping a running job;
  restart reconciles durable receipts without replaying accepted work. Damaged
  neighboring records are isolated while preserving their original bytes.
- The bundled first-party marketplace lists the actual plugin bundles and uses
  the existing install, review, trust and update paths. A read-only connection
  check verifies catalog and skill mirrors on changes and weekly. Already present
  bundles lead to their local review and management controls; catalog installs
  refuse name collisions before downloading, without overwriting or granting trust.
- Windows deny checks preserve native path separators while retaining the
  conservative POSIX scan for shell wrappers and substitutions.
- The config example agrees with the telemetry disclosure: usage analytics are
  optional and enabled by default; local diagnostics do not require telemetry (#6011).

- Sub-agents enforce the session's typed shell and file deny rules, including
  Full Access, delegated edits, and policy updates after a child starts (#6097).
- Live permission edits reach running engine clones atomically. Deny rules
  support token wildcards and Windows command spellings while keeping POSIX
  arguments literal (#6054, thanks @h3c-hexin and @asto18089).
- Tool-result images use the existing typed inline path and are fully decoded
  under size and allocation limits before admission or provider projection.
  Invalid images keep a visible omission receipt alongside the text result
  (adapted from #6053, thanks @h3c-hexin and @asto18089).

- A lone `y`, `Y`, or `r` remains composer input after selecting transcript
  text. Clicking the composer releases transcript and dock focus.
- The Agents register updates from live worker events while the parent is
  busy, including when the register opened before the workers spawned.
- Ghostty no longer shows a second graphical whale over the launch mark.
  The selected model stays visible before the first message; unused session
  metrics stay quiet. Scheduled counts open `/automation` directly.
- Marketplace installation accepts compatible Claude plugin bundles and roots
  their relative sources outside `.claude-plugin`; unsupported components fail
  explicitly. Native trust and enablement review still applies.
- Config table and nested reads work consistently and redact credentials.
  Unsupported dotted writes fail without changing the file, and `config doctor`
  no longer labels runtime settings as never applied (#6083).

- Esc during a worker fanout keeps the parent stopped when cancelled workers
  finish. Their receipts remain available for the next explicit user turn.
- Undo preserves messages even when older state lacks a timestamp. Session
  journal entries retain their append-time stamps when saved again.
- Extension actions refresh in place, and inspection output uses the text
  pager. The empty launch shell hides unused metrics and keeps recovery hints
  readable when several MCP servers need attention.
- The large-output router defaults to bounded spillover; adaptive routing
  remains opt-in through `CODEWHALE_ADAPTIVE_OUTPUT_ROUTING=1`.
- Direct composer paste (`Ctrl-V`) preserves rich clipboard headings, lists,
  links, tables and code as Markdown, with plain-text fallback. Whole-answer
  copy preserves authored Markdown without terminal wrapping; settings fields
  still paste literal text.
- The embedded Computer Use bundle includes the marketplace's JPEG capture
  and bounded-payload fixes, so new built-in installations receive them too.
- A provider's missing-`thought_signature` HTTP 400 now explains how to recover:
  use the built-in Google provider and start a new session, or verify the
  gateway's signature handling. Working gateways are not blocked by a broader
  preflight check (#6048, #6081, thanks @nightt5879).
- Chinese documentation links resolve to the contributor guide and Windows
  screenshots from their translated paths (#6080, thanks @c020627).
- GLM-5.3 reasoning controls follow the forced-thinking contract on Z.ai and
  BigModel routes, including low effort when a prior configuration requested
  thinking off (#6051, thanks @h3c-hexin and @asto18089).
- Finance calls respect the session network policy, and model-facing shell,
  verifier, notification and Fleet guidance matches executable contracts
  (#6052, thanks @h3c-hexin and @asto18089).
- The sandbox-elevation prompt shows every option, `Abort` included. The card
  was a fixed 22 rows centred on the frame; inside its border and padding that
  left at most 18 usable rows against 20 to 23 rows of content, with no scroll
  rail and no truncation hint. The safe exit — the one choice that grants
  nothing — was painted past the bottom edge at every terminal size. The card
  is measured from its content now and reserves the option rows before the
  denial detail, which is what shortens; below roughly 60x24 the per-option
  descriptions shed and every choice keeps its row. Option hitboxes were also
  counted in unwrapped source lines, so a description that wrapped put every
  hitbox below it out of step and a click committed a different choice than the
  one under the pointer.
- `d` in the Hotbar setup modal asks before it clears every slot. It persisted
  `hotbar = []` on the first keystroke, in a view that takes bare letters as its
  filter — the destructive key and the search key were the same press. It arms a
  confirmation that owns every key until answered, and the prompt takes the
  intro's place in the header so it cannot be the line that falls off a
  five-row budget.
- Failure red means failure again. The metrics line painted the context reading
  in the error colour from 80 %, while the posture bar one row above called the
  identical threshold Attention; the workflow panel painted a `Waiting` row like
  a crashed one, though its own `is_running` counts Waiting as healthy; and the
  work surface spent `error_fg` on to-dos that were merely waiting, blocked or
  stale, and on the routine approach to auto-compaction. `WorkTone` has a real
  `Failure` variant now, and a guard test holds the reservation across every
  selectable theme rather than the default alone.
- The command palette runs the row you highlighted. `refilter` clamped the
  selection index but never re-anchored it, and every keystroke re-sorts the
  list, so refining a query could move the highlight to an unrelated entry that
  Enter then ran. Long labels also overran their column and pushed the
  description off the card, because the padding never truncated and the capacity
  was measured against the popup rather than the content area.
- The work surface's first `Down` reaches the first row. Nothing is selected
  when the dock opens, and that resolved to "row 0 is selected", so the first
  press moved to row 1 and row 0 could not be reached by pressing Down at all.
- Auto-compact could not fire mid-turn. The gate read `max(last billed
  prompt, /4 estimate of the whole list)`, so as soon as the estimator
  undercounted the full list below the last bill, every tool result appended
  after that prompt was invisible to it, and a long turn could exhaust the
  context window with nothing compacted. It now reads live tokens — the
  billed prompt plus the growth since it, watermarked when the parent usage
  is recorded — and is still evaluated at the pre-request boundary after
  tools complete, never mid-tool-call.
- Esc or Ctrl+C during a compaction that is serving an in-flight turn now
  stops the turn. It previously cancelled only the compaction pass, so the
  turn resumed against the context that had just failed to shrink. A manual
  `/compact` with no request in flight still cancels only the pass.
- The `request_user_input` dialog is a bottom-anchored sheet instead of a
  centered 22-row overlay. It leaves the transcript visible above it, grows
  with its content, and scrolls internally so the highlighted option and the
  custom response being typed stay on screen at 141x38 and 80x24. Left arrow
  or `h` goes back to the previous question; Esc still cancels the whole
  request. PageUp/PageDown, Home/End and modified arrows now review the
  transcript while the sheet stays open; the mouse wheel scrolls the surface
  under the pointer. Wheel browsing preserves the answer, and option navigation
  or typing restores the focused content to view. The transcript viewport ends
  above question and approval prompts, so End keeps the newest rows visible.
  Documented in GUIDE.md and KEYBINDINGS.md (#6045).
- `/mcp reload` no longer freezes the interface. The reload was awaiting the
  whole reconnect batch on the TUI event loop; it now joins the same
  supervised background pass the session boot uses, the status chip counts
  the batch down live, and the finished receipt arrives as an event. With
  23 configured servers (11 live, 10 awaiting auth, 2 failing) the first
  echoed keystroke after a reload lands in ~5 s instead of ~42 s (#5974).
- The posture bar no longer states the same duration twice on a first turn
  (#6041).
- Reasoning-capable models whose id carries no version substring
  (`deepseek-flash`) keep `reasoning_content` in the thinking block instead
  of the answer text. The gate only ever matched the literal `deepseek-v4`
  version string, so it now consults the model catalog as well; the older
  literal arms remain for the V4 aliases they were written for (#6044).
- `codewhale model resolve` accepts a provider's declared default even when
  its registry row is missing — `deepseek-flash` failed resolution against
  the provider that declares it as default — and a test now resolves every
  provider's `DEFAULT_*_MODEL` for its own provider (#6043).
- A configured `default_text_model` is honored when a new thread's route is
  resolved. That path consulted only the provider's catalog default, so a
  config naming one model silently created threads on another, and the
  unset default disagreed with the shipped one; the active provider's
  configured default now wins, matching `provider_default_model`, and both
  unset defaults name `deepseek-flash` (#6043).
- Markdown `_italic_` requires both delimiters to be flanking per CommonMark,
  so math subscripts no longer italicize the prose between them
  (`[t_, b_p]. Actually — hold on, do we even tile all the way from t_?`
  rendered 60 characters italic) (#6042).
- An MCP server configured for OAuth that answers 401 before its first login
  now points at `/mcp login <name>` in the failure hint instead of a bearer
  token that does not exist (#6030).
- Cancelling a foreground shell wait stops its owned process group even when
  the tool future is dropped. Explicitly backgrounded jobs retain their
  ownership. Interrupted tool receipts distinguish work that started from
  calls skipped before execution, and returned tool failures remain errors in
  the next model request.
- Saved Fleet model identifiers retain exact spelling through selection,
  role pins, and roster changes, so changing one saved model does not modify
  another identifier that differs only in letter case.
- Chat wrapping reserves its scrollbar gutter consistently, keeping long
  identifiers readable when the viewport changes.
- The Engine keeps large send-message futures off the event loop's stack,
  preventing stack exhaustion when a restored session starts a provider turn.
- New, imported, and live session titles skip runtime handoffs and use the
  first real user prompt. Explicitly renamed titles retain priority
  (#6012, thanks @SparkofSpike).
- UI dispatch acceptance now precedes Engine execution, so a delayed acceptance
  callback cannot overwrite a turn that has already started or completed.
  Cancelling before acceptance preserves the prompt and leaves the next
  dispatch usable.
- Bottom-chrome effort is omitted when the route cannot prove an effective
  tier; `/status` retains the full explanation. Cost remains visible when
  known, and `cost: unknown` remains on metered routes lacking a reading (#5950).

- Pasting multiline text is one paste again. 0.9.12 gated the
  paste-burst heuristic off whenever bracketed paste was *requested*, but
  a terminal can accept `EnableBracketedPaste` and still deliver a paste
  as individual keystrokes — on those terminals (observed on Windows 11)
  every pasted line was submitted as its own message. The heuristic is
  again armed whenever `tui.paste_burst_detection` is on, and the
  existing `bracketed_paste_seen` guard still disarms it for the rest of
  the session once a real `Event::Paste` arrives, so fast typing on
  terminals with working bracketed paste is unaffected (#5981, thanks
  @nsfoxer).
- `serve --acp` no longer breaks strict JetBrains clients: the
  `initialize` response advertised `sessionCapabilities.list` as a
  boolean and carried an undefined nested `load` capability; it now
  sends `{"list": {}}` with no `load` key, per the ACP schema (#5969,
  reported by @Lujc0523).
- Concurrent Codewhale instances no longer destroy each other's queued,
  unsent text. The offline input queue was one global file that boot
  cleared on session-id mismatch, so a second instance deleted the
  first's parked messages. Queues are now keyed per session (mirroring
  per-session checkpoints), an existing global file is adopted by its
  owning session rather than discarded, and the adoption race between
  two instances of the same session tolerates the loser's cleanup.
- A tool call truncated at the provider's output limit can no longer be
  repaired into valid JSON and executed: repairs that had to synthesize
  structure (append or discard closers) are routed to the existing
  malformed-arguments path so the model is asked to re-issue — including
  when the stream is cut before the closing content-block event (#5986).
- `codewhale metrics` reads Codewhale's own receipts again: the
  deepseek-home fallback resolved `$HOME/.deepseek` unconditionally, so
  the rollup reported all zeros from a directory nothing has written
  since 2024. The Codewhale audit log is primary, with a checked legacy
  fallback.
- The goal-continuation loop's promised stall bound actually bounds
  stalls now, and undeclared fleet role names fail closed to read-only
  `explore` in both fleet drivers instead of resolving to write-capable
  customs in one and not the other.
- `allow_insecure_http = true` under a `[providers.<name>]` table works
  again. 0.9.12 tightened plain-HTTP base URL handling in a way that
  silently dropped the per-provider key, leaving the process-wide env
  var as the only opt-in — LAN llama.cpp and internal-gateway users
  had to export `CODEWHALE_ALLOW_INSECURE_HTTP=1` to connect at all.
  The key is honored again (parsed, settable and unsettable through
  `codewhale config set providers.<name>.allow_insecure_http`, and
  listed in the custom-provider field hint), it stays distinct from
  `insecure_skip_tls_verify`, and the refusal message now leads with
  the config key. Loopback hosts remain auto-allowed and telemetry
  endpoint validation deliberately still consults neither switch
  (#5991, thanks @Gabriel-Degret).
- A Fleet task that selects a roster member with `worker.agent_profile` now
  runs with that member's posture. The launch-time resolver only consulted the
  resolved member when the legacy `worker.role` label was absent, so a task
  labelled `manager` that selected `member:reviewer` ran as a write-capable
  manager instead of a reviewer and was never leased. The member's canonical
  slot now wins whenever one resolved; the label remains the posture only when
  no member resolved at all. To keep the fix from widening authority in the
  mirror case (a read-only label on a write-capable member), a spec whose
  `worker.role` names a different posture than the selected member's role is
  rejected at run creation with a message naming both postures; casing and
  legacy aliases of the member's own role are still accepted (#5945, thanks
  @gaord).
- A queued message keeps one transcript entry from queue through steer to
  dispatch. It no longer reads as delivered while it is still pending, and the
  second Enter the strip prompts for no longer paints a duplicate bubble.
- The Tasks rail panel no longer swallows the first keystroke of typed input:
  bare `y`/`Y` yank only when the work surface holds keyboard focus, so a
  message that starts with "yes" stays intact (#2000).
- `wait` results reach the parent intact. The parent-context summarizer
  projected every `agent` tool result through the sub-agent snapshot renderer,
  which replaced a wait envelope's `settled`, `still_running`, `timed_out`,
  `waited_ms`, and `note` with `unknown (agent) status=unknown`. Only
  snapshot-shaped results are summarized now.
- A sub-agent's final summary uses the same adaptive tool-result budget as
  every other tool result (`tool_result_max_bytes`, 12k floor, 2 MiB ceiling)
  instead of a hardcoded 12k/4k/4k copy that cut a 12,001-character report to
  8,000.
- The metrics line no longer carries a permanent `/help` hint. The route still
  appears until its binding has been used and then retires with the other
  footer hints, so the row stays quiet once help is learned.
- `request_user_input`'s description names when asking is the right move
  (ambiguous scope, an irreversible or expensive choice, a missing preference)
  and when it is not, so the model stops guessing at discoverable facts.
- `config.example.toml` states the real search default (keyless Firecrawl, not
  DuckDuckGo) and the preferred `CODEWHALE_SEARCH_*` environment names.
- Startup type-ahead is preserved whole, and a slash command interrupted by
  startup keystrokes is no longer re-read as prose and sent to the model
  (#5925).
- The model picker renders a large catalog without lag and supports per-column
  sort and mouse selection (#5975).
- Session metrics report effective request throughput from provider receipts
  instead of a live estimate divided by the whole turn, and cost stays honest
  when a route has no price reading (#5976, #5977).
- An offline-queue submit delivered as late raw keystrokes no longer wedges the
  composer behind stacked session notices (#5999).
- Fleet setup with a model filter that matches nothing renders a hint instead
  of panicking (#5953), and shortlist enrollment plus authoritative role pins
  are honored on dispatch (#5915).
- A mid-turn MCP catalog refresh keeps the tool pool deferred and the active
  set narrow (#5939).
- Computer Use on macOS verifies application activation before sending
  keystrokes and guards input by the frontmost app (#5927).
- Onboarding tests isolate `CODEWHALE_HOME`, so a fixture provider can no
  longer leak into the real setup state (#5932).
- Snapshot availability is retained with one warning per session, and a
  diverged engine session id is a visible notice rather than a log line
  (#5930).

### Changed

- How long Codewhale waits for a human is configurable. `[tools]
  user_input_timeout_seconds` governs the wait for an approval decision or a
  `request_user_input` answer; it was a hardcoded 300 seconds, which silently
  cancelled the work of anyone who stepped away mid-task. An explicit `0`
  waits indefinitely, the value is clamped to 24 hours, and omitting the key
  keeps the previous 300-second default. Documented in
  `docs/CONFIGURATION.md` ([#6003](https://github.com/codewhale-hq/Codewhale/issues/6003)).
- `docs/PROVIDERS.md` lists every beginner setup template, not the four it
  happened to mention when the page was written. Baseten, Groq, Cerebras and
  Command Code have shipped as supported OpenAI-compatible hosts for a while
  and appeared nowhere in the provider documentation, which reads from outside
  exactly like not supporting them. The page now carries the full table —
  host, default model and key env for each — and states the rule it follows:
  a plain Chat Completions backend is a template row, while `ProviderKind` is
  reserved for distinct wires. A test now fails if a shipped template is
  missing from that page.
- Reasoning capability for the Kimi coding routes and the qwen3.x Model Studio
  deep-thinking ids is catalog data now rather than hardcoded match arms, and
  `model_reasoning_capability` reports a model nothing knows about as unknown
  instead of silently not reasoning-capable. `model_supports_reasoning` keeps
  its `bool` shape for existing callers, where unknown still reads as false.
  The ids that have no cited source yet keep their literal arms (#6032).
- The website uses Shannon Sans with versioned local font assets and retained
  serif, monospace, and language fallbacks. Terminal fonts are unchanged.
- `codewhale metrics` reports recorded model requests and stream recovery
  separately from provider-reported token usage, with coverage for missing
  and duplicate receipts. Status messages and cumulative snapshots do not
  add requests or count tokens again.
- Runtime turn receipts retain the Engine's terminal model-request, stream-retry,
  and resume counters separately from displayed status and provider-reported
  usage. These counters do not count HTTP retries inside a provider client or
  establish provider billing.
- Initial tool definitions no longer repeat shell interpreter guidance and
  agent lifecycle/scope instructions in multiple description fields. Parameter
  schemas, approval rules and dispatch behavior are preserved. This reduces
  prompt schema size; it does not establish a provider billing regression.

- The built-in Computer Use plugin bundle is refreshed to the standalone
  plugin's 0.2.1 runtime (vendored from `Hmbown/codewhale-cu-plugin`
  at `724ad258`): the native macOS accessibility backend with an
  a11y-first pointer strategy (covered points are refused, previews are
  drawn), the permission-owning desktop-app socket transport, remote
  computers over ssh and HarmonyOS HDC with contained temp handling,
  truthful win32 PowerShell failure reporting, and the shared
  allow-listed request handler for the app socket and ssh agent.
  The bundle retains the hardening port from
  [standalone plugin PR #12](https://github.com/Hmbown/codewhale-cu-plugin/pull/12).
  Single left clicks on macOS element targets now revalidate and press
  the observed element directly, without substituting a point hit-test
  or raw-pointer fallback. Explicit event clicks retain the app-ownership
  guard; a sent press still requires visual verification. Background mode
  permits application-bound keys and accessibility actions while refusing
  shared-pointer gestures; it does not provide an isolated desktop. The
  embed list gained the five new runtime files, and a consistency test
  now pins the embed list to the vendored tree so the bundle cannot rot
  silently again. Because the bundle's content hash changes, Computer
  Use deactivates and asks for a fresh review after upgrading — that is
  the designed fail-closed path for a desktop-driving plugin. The
  current macOS source candidate also embeds the compiled native helper,
  so its bundled server can run directly without a separate Computer Use
  app or a compiler on the user's machine. Accessibility and Screen
  Recording permissions belong to the hosting app or terminal and remain
  user-controlled. The CLI's Computer Use server requires Node.js 20 or newer. These are source
  candidate changes; they do not establish published-package or platform
  qualification. See the [included plugin guide](crates/tui/plugins/computer-use/README.md)
  for platform requirements and limitations.
- `/statusline` drives the bottom chrome again. Since the 0.9.12 shell
  redesign the posture bar and the metrics line were built independently of
  `tui.status_items`, so every toggle in the picker except the balance fetch
  was decoration. Each remaining item now shows or hides exactly one thing:
  `model`, `context_percent`, `cost`, `balance`, `cache`, `tokens` and
  `session_metrics` are metrics-line segments, and `mode` is the posture
  bar's plan/act/operate chip. The `status`, `agents`, `reasoning_replay`,
  `prefix_stability`, `git_branch`, `last_tool_elapsed` and `rate_limit`
  items drove nothing and are retired; an existing `config.toml` still loads
  and those keys are ignored (#5950, #5962).
- The context reading is back on screen at every fullness. 0.9.12 painted
  `ctx NN%` only from 50% up, which left most of a session with no context
  signal at all; it now paints from 0% and keeps its warning colour from 80%
  up (#5950).
- A child agent parked because its parent's turn ended is shown as `parked`
  in the Agents panel, the sidebar and Agent Details, with `resume_from` /
  `cancel` as the recovery, instead of wearing the same "waiting for input"
  label as a child that asked a question. Parked work sorts below live and
  answerable work and no longer inflates the `blocked` chip; the receipts
  roster and the wire `state` gain `parked` (#5906, #5921).

- `codewhale account keys set|remove|list` no longer carry a hardcoded
  eight-provider list. Provider ids come from the control plane's public
  catalog (`GET /api/model-providers`), are validated locally against
  `^[a-z0-9][a-z0-9-]{0,63}$` before they reach a URL path, and `list` shows
  every catalog provider with its label and stored-key state. `--from-local`
  maps a catalog row onto the local runtime provider through the catalog's own
  `runtimeProvider` field, so a newly supported provider needs no CLI release.

- `/mcp` lists the servers that need a login first, as their own
  `Needs login` group above `Needs attention`, and opens with the cursor
  already on the first such row so the Enter the screen advertises runs
  `/mcp login <server>` straight away; translated in all 15 packs. A
  snapshot test pins the footer shape the chip landed with (`MCP · N
  connected · N ◆ auth required · N failed`) so an expired login never
  regresses into the failed count (#5926).

### Fixed

- Five of the load-flaky tests tracked in #5929 no longer depend on shared
  state or live local daemons. Background-hook capture tests wait for the
  capture file to hold bytes instead of merely existing (the shell's `>`
  redirection creates the file empty before `cat` writes, which read as
  `valid JSON: EOF` under load); the session-picker acceptance test drives
  the real picker over a private store instead of a process-global
  `CODEWHALE_HOME` redirect that concurrent tests could observe mid-flight;
  the DeepSeek-Anthropic translate test holds the test env barrier so its
  request-time and assertion-time `max_tokens` reads cannot straddle another
  test's `CODEWHALE_MAX_OUTPUT_TOKENS` override; the unit-test binary no
  longer probes a real local Ollama daemon (`127.0.0.1:11434`) from the
  fire-and-forget provider catalog refresh, whose merged tags could flip
  another test's live-snapshot assertions; and the tmux clipboard test's
  attach/OSC 52 wait bounds tolerate a fully loaded machine (3s -> 30s)
  without changing what they verify (#5929).
- The posture bar states how long the session has been working and how long
  the current turn has run, distinguishing actively working from waiting on
  a tool, a sub-agent or the operator; the 0.9.12 shell had dropped the overall
  working-time indicator from the place a glancing user checks (#5914).
- A background runtime turn whose own store record could not be read, parsed
  or written (`Failed to read turn …`, `Failed to read item …`) was only a log
  line. The runtime now publishes a `runtime.store_failure` event naming the
  file, the root cause and the next action (move the file aside, or check free
  space and permissions); the TUI shows it as a warning toast and a transcript
  line, the task timeline records it, and the runtime API streams it. A turn
  whose own record is unreadable or unwritable is reported as terminal, so its
  task fails at once instead of idling out (#5931).

- An MCP token refresh that fails to parse the provider's answer keeps the
  endpoint's receipt — status line, content type, and a 200-byte excerpt
  with every credential-shaped value (`access_token`, `refresh_token`,
  `client_secret`, `id_token`, bearer schemes) masked before the cut —
  instead of rmcp's bare `Failed to parse server response`, so a provider
  outage answering an HTML 502 reads differently from a parser defect, and
  the login remedy stays named (#5926; remedy wording landed in #5959).

### Added

- `POST /v1/threads/{id}/file-revert` restores exactly one file from the
  exact `tool:`/`pre-turn:` snapshot the client selected, checking the
  reviewed file hash before and after the mandatory safety snapshot. Literal
  file names, regular files only, thread trust and active-turn admission are
  enforced, and `patch-undo` no longer forks a conversation whose file
  rollback failed (#6111, thanks @gaord; engine half of
  HengQuWorld/CodeWhale-VSCode#3).
- Authenticated Runtime API workspace file suggestions reuse TUI `@file`
  matching and discovery, with bounded queries/results and workspace-contained
  relative paths only (`GET /v1/workspace/files/search`, #6095, #6120, thanks @wuisabel-gif; reported by @LmeSzinc). Shared discovery
  now honors disabled symlink following for AI-tool directory scan roots too.
- Serply is available as an opt-in `[search]` provider for the Web tool
  (`provider = "serply"`, key from `[search] api_key` or `SERPLY_API_KEY`).
  Preflight fails closed without a key; Firecrawl remains the default and
  existing configurations are unchanged (#6100, thanks @googio).
- Linux terminals: finishing a transcript or composer mouse selection copies
  the text to the PRIMARY selection without touching the regular clipboard, and
  middle-click inside the composer pastes PRIMARY at the pointer without
  submitting. Native X11 and Wayland data control are used through one bounded
  background worker; SSH sessions without a forwarded display keep their
  terminal's own selection behavior (#6116, thanks @dmt4).
- `codewhale sessions export <id-or-unique-prefix>` saves a `.tar.xz` archive
  with the durable record, portable session container, manifest and artifacts.
  Prefix exports preserve unfinished tool calls; confined reads reject linked
  artifact roots, and existing outputs require `--force`. Archives retain
  unredacted content; `/load` opens the extracted record without installing
  extracted artifacts (#6056, thanks @h3c-hexin and @asto18089).

- `deepseek-flash` (DeepSeek V4.1 Flash: 1M-token context,
  reasoning and tool calls) joins the catalog as DeepSeek's declared default,
  and the offline catalog seed matches it; the DeepSeek Pro listing no longer
  overstates the published price (#6025).
- DeepSeek's September 11 reversal is reflected in provider notices and cost
  estimates: V4 Pro remains available after September 14 at Pro rates. Explicit
  Pro selections remain unchanged; Flash remains the default (#6025, thanks @ronohara).
- Native plugin authoring guides now cover English and Chinese. The explicit
  offline converter supports selected portable Skills and static Streamable
  HTTP MCP declarations from OpenCode and DSH. Unsupported executable hooks,
  automatic OAuth and policy-bearing configurations are refused; generated
  bundles still require native installation, review and trust. Legacy SSE
  fallback is not reproduced (#5827, requested by @giancarlocp).

- Signed cloud model facts can refresh provider capabilities and prices while
  preserving verified cached data when a refresh fails. A dispatched request
  keeps its selected price snapshot so later catalog updates cannot change its
  recorded cost (#5752).
- Saved sessions preserve exact provider routes. Auxiliary model calls settle
  their usage once against the route and price snapshot that executed them,
  including recovery, rather than resolving a new price at completion
  (#5726, #5848).
- `[tui].posture_bar` and `[tui].metrics_line` accept `full`, `compact`, or
  `hidden`, also available through `/config`. Compact preserves the existing
  rows' essential fields; hidden returns their space to the transcript (#5973).
- Optional model-bound tool-output redaction opt-out, with two explicit startup
  confirmations and a receipt bound to the readable config contents and
  modification time. Unconfirmed requests keep masking enabled; routing and
  stored goal summaries remain redacted (#5982, thanks @SparkofSpike).

- The `rusty-alloc` cargo feature on `codewhale-tui` and `codewhale-cli`
  opts the binaries into the `rusty_alloc` global allocator (the mimalloc
  v2.4.5 architecture remade in pure Rust — no C compiler or build script
  on that path) instead of the default mimalloc. It is off by default and
  the default build is unchanged; build with
  `cargo build -p codewhale-tui --features rusty-alloc` (#5872).
- The `/theme` picker now discovers valid user-authored `custom:<name>`
  overlays, previews their colors, highlights the active overlay, and preserves
  it when the picker is opened and committed without navigation (#5901).
- Compaction has two standing knobs next to `[context]` in config.toml:
  `[compaction] summary_instructions` (appended to the summarizer prompt on
  every manual and automatic pass; `/compact <focus>` still composes after
  it) and `[compaction] retained_user_message_tokens` (default 20 000,
  clamped 2 000..=200 000) for the verbatim user-message budget. Both are
  absent by default and absent means the pre-existing behavior. The
  `/compact` receipt names the effective budget and whether operator
  instructions applied (#5956).
- `[tools] user_input_max_questions` (default 6, 1..=10) and
  `[tools] user_input_max_options` (default 4, 2..=10) replace the hard-coded
  `request_user_input` limits; the validator, the tool schema and its
  description read one value, spawned children inherit the parent's ceilings,
  and a rejected payload names the ceiling it hit and the key to raise (#5949).
- The slash menu shows a command's usage line and its subcommands as soon as
  a space is typed after the verb, filtered by what follows, so Tab completes
  `/workspace wor` to `/workspace worktrees`; `/help` states the focused
  command's usage in its detail slot (#5952).
- The three `/fleet` views (roster, live workers, saved teams) are one
  back-navigable stack: `Esc` in workers or saved teams returns to the roster
  with its cursor intact and still closes the window at the root or on direct
  entry; the `Esc` footer hint says `back` or `close` accordingly (#5954).
- `/fleet` presents its prioritized core (`members`, `setup`, `teams`,
  `workers`, `help`); every other verb stays dispatchable and is documented
  under explicit groups in `/fleet help`. The roster no longer shows the
  untouched built-in `general` alias next to `worker` (#5888).
- `codewhale` provider: account-backed model access over the provider keys a
  customer connected to their Codewhale account. One base URL
  (`https://api.codewhale.net/v1`, overridable with `CODEWHALE_API_BASE`;
  HTTPS required except on loopback), one `cwc_key_…` account API key with the
  `models:infer` scope (`CODEWHALE_API_KEY`), and a per-model wire chosen from
  the account's authenticated `GET /v1/models`: ids are `provider/model` and
  each row states `chat-completions` (`/v1/chat/completions`) or
  `anthropic-messages` (`/v1/messages`). Both protocols authenticate with
  `Authorization: Bearer`, never `x-api-key`. The key is read from
  `CODEWHALE_API_KEY`, the `codewhale` secret-store slot, or
  `[providers.codewhale] api_key` / `api_key_env`, and a missing key fails
  before any request instead of dispatching unauthenticated. If the catalog
  cannot be fetched the route falls back to three bootstrap ids and says so.
- `codewhale account api-keys create --scope` now accepts `models:infer`
  alongside `account:read` and `agent:run`, and an omitted `--scope` sends all
  three explicitly. `--use` saves the new secret as this machine's local
  `codewhale` provider credential in the same secret store `codewhale auth`
  uses; nothing is uploaded.
- `Git` grows a `commit_plan` action: a propose-only planner that splits the
  working tree into ordered atomic commits (#3999). It groups whole files —
  lock files ride with their manifest, tests ride with the source they name —
  orders the groups so a commit that defines a symbol lands before the commit
  that uses it, and refuses the whole plan when that dependency graph has a
  cycle. It reads `git diff HEAD` plus the untracked-file list and writes
  nothing: no `git add -N`, no `git apply --cached`, no `git commit`, so
  staging and committing stay with the ordinary `git add` / `git commit` shell
  path where the approval gate already applies. Thanks
  [@goransh-walia](https://github.com/goransh-walia) for the original
  implementation (PR #5870, fixes #3999).

### Contributors

- **[@LmeSzinc](https://github.com/LmeSzinc)** — requested Runtime API access to the TUI's fuzzy file search ([#6095](https://github.com/codewhale-hq/Codewhale/issues/6095)).
- **[@googio](https://github.com/googio)** — added the Serply web-search provider ([#6100](https://github.com/codewhale-hq/Codewhale/pull/6100)).
- **[@dmt4](https://github.com/dmt4)** — requested Linux copy-on-select and middle-click paste ([#6116](https://github.com/codewhale-hq/Codewhale/issues/6116)).
- **[@Gabriel-Degret](https://github.com/Gabriel-Degret)** — reported that saved agent profiles were silently ignored when spawning sub-agents ([#6117](https://github.com/codewhale-hq/Codewhale/issues/6117)).
- @nightt5879 — Gemini signature recovery guidance and transport regressions (#6081).
- @c020627 — Chinese documentation link repairs (#6080).
- @h3c-hexin and @asto18089 — GLM-5.3 reasoning controls and tool-gating/documentation fixes (#6051, #6052).
- @Hmbown — dependency updates (#6057) and the Gemini signature recovery report (#6048).
- **[@gaord](https://github.com/gaord)** — contributed the file-scoped restore endpoint and the trust-gated whole-tree rollback ([#6111](https://github.com/codewhale-hq/Codewhale/pull/6111)), Fleet schema inspection, role precedence and worker deliverable receipts, and linked the community VS Code frontend ([#5944](https://github.com/codewhale-hq/Codewhale/pull/5944), [#5945](https://github.com/codewhale-hq/Codewhale/pull/5945), [#5946](https://github.com/codewhale-hq/Codewhale/pull/5946), [#5992](https://github.com/codewhale-hq/Codewhale/pull/5992)).
- **[@goransh-walia](https://github.com/goransh-walia)** — contributed the propose-only commit-planning rework ([#5870](https://github.com/codewhale-hq/Codewhale/pull/5870)).
- **[@7jrxt42BxFZo4iAnN4CX](https://github.com/7jrxt42BxFZo4iAnN4CX)** — documented turn budgets and goal configuration, and reported gaps in command discovery, Fleet navigation, human waits, state hooks, history and provider routing ([#5996](https://github.com/codewhale-hq/Codewhale/pull/5996), [#5952](https://github.com/codewhale-hq/Codewhale/issues/5952), [#5954](https://github.com/codewhale-hq/Codewhale/issues/5954), [#6003](https://github.com/codewhale-hq/Codewhale/issues/6003), [#6004](https://github.com/codewhale-hq/Codewhale/issues/6004), [#6006](https://github.com/codewhale-hq/Codewhale/issues/6006), [#6007](https://github.com/codewhale-hq/Codewhale/issues/6007)).
- **[@SparkofSpike](https://github.com/SparkofSpike)** — contributed two-stage consent for opting out of model-bound credential redaction ([#5982](https://github.com/codewhale-hq/Codewhale/pull/5982)).
- **[@aboimpinto](https://github.com/aboimpinto)** — moved session lifecycle and session-control commands onto shared command contracts ([#5902](https://github.com/codewhale-hq/Codewhale/pull/5902), [#5951](https://github.com/codewhale-hq/Codewhale/pull/5951)).
- **[@EvanProgramming](https://github.com/EvanProgramming)** — reported Windows input and CRLF-write defects, and contributed CRLF preservation and an injectable Windows input runner ([#5908](https://github.com/codewhale-hq/Codewhale/issues/5908), [#5909](https://github.com/codewhale-hq/Codewhale/issues/5909), [#5910](https://github.com/codewhale-hq/Codewhale/pull/5910), [#5911](https://github.com/codewhale-hq/Codewhale/pull/5911), [#5912](https://github.com/codewhale-hq/Codewhale/pull/5912)).
- **[@wuisabel-gif](https://github.com/wuisabel-gif)** — exposed workspace file suggestions through the Runtime API ([#6120](https://github.com/codewhale-hq/Codewhale/pull/6120)) and added custom-theme discovery, preview and selection in the theme picker ([#5907](https://github.com/codewhale-hq/Codewhale/pull/5907)).
- **[@zhuowp](https://github.com/zhuowp)** — matched model-visible shell guidance to the interpreter selected for execution ([#5900](https://github.com/codewhale-hq/Codewhale/pull/5900)).
- **[@nsfoxer](https://github.com/nsfoxer)** — reported the multiline-paste regression and incomplete provider model lists ([#5981](https://github.com/codewhale-hq/Codewhale/issues/5981), [#6009](https://github.com/codewhale-hq/Codewhale/issues/6009)).
- **[@Nefelibata1024](https://github.com/Nefelibata1024)** — confirmed the multiline-paste regression's impact ([#5981](https://github.com/codewhale-hq/Codewhale/issues/5981)).
- **[@Gabriel-Degret](https://github.com/Gabriel-Degret)** — reported the loss of the allow_insecure_http provider setting ([#5991](https://github.com/codewhale-hq/Codewhale/issues/5991)).
- **[@Lujc0523](https://github.com/Lujc0523)** — reported the ACP initialize schema violation affecting strict IDE clients ([#5969](https://github.com/codewhale-hq/Codewhale/issues/5969)).
- **[@mo-vic](https://github.com/mo-vic)** — proposed storing evicted context on disk so it can be retrieved later ([#6008](https://github.com/codewhale-hq/Codewhale/issues/6008)).
- **[@giancarlocp](https://github.com/giancarlocp)** — requested a plugin authoring guide and OpenCode plugin conversion ([#5827](https://github.com/codewhale-hq/Codewhale/discussions/5827)).
- **[@hxfhd](https://github.com/hxfhd)** — supplied a Windows reproduction of a turn stopping before its stated next tool action ([#6010](https://github.com/codewhale-hq/Codewhale/discussions/6010)).

### Notes

- **DeepSeek V4 Pro continues after September 14.** The vendor reversed its
  earlier retirement notice. Codewhale preserves Pro selections and Pro pricing;
  `deepseek-flash` remains the default for new direct DeepSeek configurations.
- Upgrading from 0.9.12 with Computer Use trusted and enabled: the
  bundle's content hash changes with the 0.2.1 refresh, so the plugin
  deactivates and asks for a fresh review — that is the designed
  fail-closed path for a desktop-driving plugin. Re-trust it from the
  Plugins page.
- The multiline-paste fix restores v9.11 behavior on terminals that
  accept `EnableBracketedPaste` but deliver pastes as keystrokes
  (reported on Windows 11 / PowerShell). Verified at the input-contract
  level and in CI; a manual paste check on a real Windows terminal is
  still welcome — please comment on #5981 with your terminal if anything
  still misbehaves.

## [0.9.12] - 2026-09-03

Codewhale v0.9.12 puts computer use in the binary, opens two new routes —
Alibaba Model Studio through the data-driven provider table and Concentrate
as an opt-in BYOK Responses gateway — and adds cloud dispatch plus a
config-gated per-session control socket. The shell is the other half of the
release: a new launch card, the work surface docked under the composer, one
focus owner for Tab, tool cells that carry their own state, and `fleet` as
the public word for the live collective. The complete item-level change
record is retained below the categorized release highlights.

### Added

- Computer use ships with the binary. The `computer-use` plugin — 38 tools
  across macOS, Windows, Linux and HarmonyOS, accessibility-first observation
  with pixel fallback, screenshots, zoom, screen recording, and registered
  remote computers over ssh and hdc — is embedded in Codewhale and written to
  `$CODEWHALE_HOME/builtin-plugins` on first run, so every install channel
  carries it. It lists as `builtin · not-reviewed` and stays disabled until
  you review and enable it: shipping it is not consenting to it. The
  stdlib-Python computer-use server it replaces is gone.
- Alibaba Model Studio joins the data-driven provider table as an
  `openai-compatible` descriptor: international compatible-mode endpoint,
  `DASHSCOPE_API_KEY` credential, live `/v1/models` discovery. Qwen 3.8
  Flash and Qwen 3.8 Max arrive through the catalog authority — never a
  hard-coded id.
- Concentrate: first-class opt-in BYOK Responses gateway with live models
  discovery and typed SSE streaming (#5725).
- Cloud dispatch: remote runner offloads coding agent tasks to isolated
  cloud sandboxes with machine token auth and structured job tracking
  (#5701, #5712).
- Per-session control socket: config-gated `[control_socket]` table binds
  `<sessions-dir>/<session-id>/control.sock` per running session, exposing
  message, interrupt, relaunch, and status JSON-RPC verbs (#5533, #5831).

### Changed

- **Anonymous usage counting is on by default.** The 0.9.11 release asked
  first; 0.9.12 counts the same aggregate version/platform, session, feature
  and error totals unless you turn it off, and says so once at first launch
  (policy notice version 5, schema 3, `notice_version` replacing
  `consent_version`). Every recorded opt-out stays off: a durable
  `telemetry = false`, a decline recorded under the old opt-in notice,
  unreadable privacy state, and the `CODEWHALE_TELEMETRY=0` / `--telemetry
  false` kill switches. Showing the disclosure records only that it was
  shown, never an acceptance. `codewhale config set telemetry false` turns
  it off and wipes queued counts; `codewhale config set telemetry true`
  deliberately re-enables it. Nothing new is collected: no conversations,
  code, prompts, files, names, model content, credentials, or IP.

- The launch screen is our own card take: a thin top line
  `⑂ branch  path`; a centred bordered card with the whale mark,
  `Codewhale` + version, one announcement line only when it is true (the
  no-model warning, or MCP news), and the menu New worktree / Resume
  session / Changelog / Quit with their real chords right-aligned. Enter
  runs the highlighted entry, Up/Down move it, and typing goes straight to
  the composer. The card dissolves on the first keystroke or command
  (≤240 ms, instant under reduced motion) (#5826, #5801, #5815, #5286).
- The work surface sits under the composer by default, keeping history
  readable and leaving the stage unencumbered (#5809).
- Skills command shapes: FEAT-022 command shapes and retained-host
  validation (#5825, #5829).
- After the card dissolves, the working screen shows `⑂ branch  path` with
  `⋮ MCP n/m` on the right, the transcript starts with the
  `◆ session_start` receipt (naming the configured session-start hooks),
  and the composer's bottom rule carries `model (effort) · permission` —
  the route's one launch reading. The posture bar and metrics line appear
  only once a session exists.
- Vocabulary: `fleet` is the public term and `Pod` is retired from copy —
  roster, setup, detail, worker-runtime and managed-API messages now say
  Fleet (`/fleet` canonical, `/pod` alias). The workflow wire accepts the
  canonical role spellings (general/explore/planner/reviewer/implement/
  test/advisor) with the pre-rename ones kept as load-time aliases, and
  serializes canonical names.
- The Operate mode-picker hint is shortened to fit 80 columns.

- The footer always shows the permission posture; when only one chip
  fits, the permission chip outranks the mode word (#5796).
- Local Ollama: the header names a model only when the local catalog can
  serve it, and says `unknown` until it knows. The startup mark, web and
  app icon carry the new side-view prompt-eye whale (#5795).
- One focus owner: Tab and Shift+Tab work regardless of what is in the
  composer; Alt shortcuts survive mid-draft; Ctrl+Tab no longer cycles the
  mode by accident (#5798).
- Tool cells carry their own state: a running, failed or warned tool
  reads as such in the transcript itself, with per-entry rail dots and
  family-coloured glyphs (#5799).
- Web: docs hub with task search, shared empty/loading/error states, an
  offline-to-back-online banner, `/changelog` in every locale, and real
  404s with correct metadata (#5743).
- Web: the app's colours come from one file generated from the TUI whale
  palette; re-typing a colour by hand fails web CI (#5797).
- Web: a one-way ceiling on `isZh` branching outside the i18n spine —
  the count may only go down (#5805).
- An internal agent handoff doc was removed from the public tree and
  `.playwright-mcp/` is now ignored (#5800).
- First run paints the composer immediately: the Welcome/language/provider/
  trust gates no longer precede the first keystroke. Missing-key and
  workspace-trust recovery stay for returning users; language and provider
  remain in `/setup`.
- The canonical whale-tile mark propagates beyond web to the exact-raster
  surfaces that can carry it (#5738).
- Dead-code sweep: delete proven-unreferenced helpers (unused builders,
  wrappers, and leftover identifiers) and drop stale `#[allow(dead_code)]`
  where the item is production-called or test-exercised. No runtime behavior
  change (#5791, #5587).
- Remote-control recovery gives every fresh pre-dispatch attempt a new lease
  generation while preserving that generation across its typed start (#5605).

- Public roster language is Pod. `/pod` is the customer surface; fleet remains
  the internal wire, storage, and migration name (#5776).

- Compaction replacement history keeps a bounded last user round (assistant +
  tool results) instead of dropping them behind a summary. `/context` names
  the compaction path and `/anchor` survival. Failed compact still does not
  replace live history (#4394).
- Provider catalogs: compatible hosts (Baseten, Groq, Cerebras, SenseNova,
  Command Code) no longer compile a frozen model roster. Descriptors name the
  wire, URL, and env; live `GET /v1/models` and a Codewhale-owned catalog
  layer are the offering list. Command Code is a catalog/descriptor row, not
  a `ProviderKind`. Catalog presence is not an availability, entitlement, or
  provider-acceptance claim (#5783).

- Provider selection no longer probes or adopts an external CLI credential on
  ordinary picker use. Reuse requires an explicit "Use external CLI credentials"
  choice, exact-path confirmation, and Codewhale-owned revoke (#5772).

- TUI: startup no longer presents an approximate ASCII or block-glyph whale as
  the product mark. It keeps the direct Tideline prompt while exact-raster
  surfaces remain responsible for the canonical asset.

- TUI: the active-session composer paints the same three-cell `[↑]` send
  target as Startup and clicks it through the existing Enter submit
  dispatcher (#5771). Compact/quiet composers still omit the control.

- TUI/CLI: Pod is the public roster surface. User-facing Fleet wording
  moves to Pod; durable receipt keys stay compatible (#5776).

### Contributors

- **hexin ([@h3c-hexin](https://github.com/h3c-hexin))** — provider-native web
  search across four routes (#5682, #5683, #5685, #5687), authoritative
  edit-last-turn boundaries (#5621), Kimi Code k3-256k (#5622),
  post-compaction input-token reporting (#5623), and preserving the scheduled
  model selection in automations (#5650).
- **秋月凉梦 ([@qiuYliangM](https://github.com/qiuYliangM))** — co-authored the
  edit-last-turn boundary fix (#5621), Kimi Code k3-256k support (#5622), and
  post-compaction input-token reporting (#5623).
- **Isabel Wu ([@wuisabel-gif](https://github.com/wuisabel-gif))** — live
  session token totals (#5624), persisted context-pressure warnings (#5629),
  discoverable Fleet roster editing (#5604), the capability-gated cursor
  accent (#5599), and `/copy` for the latest completed response (#5692).
- **Paulo Aboim Pinto ([@aboimpinto](https://github.com/aboimpinto))** —
  Windows verbatim-path operands preserved through POSIX word splitting
  (#5610), the plugins group moved onto the command shapes (#5657), FEAT-022
  skills command shapes with retained-host validation (#5825), and FEAT-020
  plugin command shapes re-landed on main (#5865).
- **Alex Musichen ([@musichen](https://github.com/musichen))** — a stable
  DeepSeek heading in the configured-view model picker, keeping every official
  catalog model for the active provider visible (#5689).
- **[@gaord](https://github.com/gaord)** — the `GET /v1/fleet/profiles`
  runtime API endpoint, reusing the FleetManager validation path (#5688).
- **Sh1Zuku ([@SparkofSpike](https://github.com/SparkofSpike))** — corrected
  English documentation inaccuracies and the first zh_hans translations for
  the Tier-2 docs (#5613).
- **[@M-Maciej](https://github.com/M-Maciej)** — goal continuation cadence
  (#5591) and the per-session control socket (#5533, #5831).
- **Serephus ([@serephus](https://github.com/serephus))** — nixpkgs update
  (#5669).
- **[@whp233](https://github.com/whp233)** — `wire = responses|anthropic` for
  openai-compatible custom routes and opencode-zen muse-spark (#5716, landed
  as #5719).
- **Gabriel Degret ([@Gabriel-Degret](https://github.com/Gabriel-Degret))** —
  found the reasoning-only retry gap and built the first fix; landed as the
  `[reasoning_only]` retry ceiling with a request-scoped nudge (#5867).
- **[@huangxianzhan](https://github.com/huangxianzhan)** — the
  `x-opencode-session` header for OpenCode Go and Zen gateways (#5868).
- **[@zhuowp](https://github.com/zhuowp)** — task origin preserved in job
  snapshots (#5869).
- **AdityaG ([@AdityaVG13](https://github.com/AdityaVG13))** — a ten-commit
  performance pass: a zero-copy LaTeX fast path for streaming render,
  single-pass token accounting on the per-turn pressure paths, memoized
  provider resolution in the catalog cutline, parsing the bundled and
  on-disk models.dev catalogs once per process (interactive boot -70%),
  adaptive poll cadence for foreground shell completion, and worker caps
  across the read-only diagnostic family.
- **Nightt ([@nightt5879](https://github.com/nightt5879))** — isolated remote
  recovery lease generations (#5790).
- **[@yiheng-kkk](https://github.com/yiheng-kkk)** — replaced stale todo
  transcript snapshots so a rewritten todo list renders its current tasks
  (#5871, #5873).
- **[@Lfanxing](https://github.com/Lfanxing)** — Moonshot routes now degrade
  incompatible tool definitions per request instead of failing the turn.
- **WissssleyL ([@Lstarsky0](https://github.com/Lstarsky0))** — moved
  `docs/subagents` and `docs/mcp` onto the dictionary spine (#5337), with the
  metaTitle probe that keeps them there.

Reports and reproductions that shaped this release:

- **[@slowly247](https://github.com/slowly247)** — reported the Ollama input
  budget collapsing to 1,024 tokens on 32K local models (#5820).
- **[@ronohara](https://github.com/ronohara)** — reported the engine stopping
  after recoverable network errors, with the reproduction that pinned the
  discarded approval (#5769), and surfaced DeepSeek's model-service notices and the subsequent reversal
  retaining V4 Pro (#6025).
- **[@Lujc0523](https://github.com/Lujc0523)** — asked for ACP session
  configuration of mode and model (#5863).
- **[@senka9h](https://github.com/senka9h)** — reported that `serve --acp`
  lacked `session/list` and `session/load` (#5864).

### Added

- Native ChatGPT sign-in for the `openai-codex` route: `codewhale auth
  chatgpt` opens a browser PKCE flow and stores refreshable tokens in
  Codewhale-owned credentials — no Codex CLI install required.
  `/auth chatgpt-revoke` clears them off the event loop (#5784, #5778).
- MCP servers and plugins can be connected self-serve from the session: a
  unified auth flow with rotation-safe token handling, a spoken authorization
  URL, and catalog refresh when stored credentials stop working (#5747).
- `/operate` reads match the landed CWC `OperateRecord` contract: fetching an
  absent record returns a truthful not-found view instead of a fabricated
  operation, and an empty evidence path is rejected rather than resolving to
  the workspace directory (#5703).
- TUI: scheduled automations project into the top strip
  (`⏱ N scheduled · M running`, compact `⏱ N·M`) with typed
  `HistoryCell::Automation` receipts when a run this session watched settle.
  `/automation` acknowledges failures. The merged footer does not carry the
  work fact (#5748).
- The app-server can listen on a unix domain socket and advertise a
  `daemon/attach` handshake, so a local client can attach to an already-running
  engine instead of spawning its own. The socket is created with
  owner-only permissions and stale sockets are reclaimed on start. Non-unix
  hosts return a typed unsupported-platform refusal; the Windows named-pipe
  endpoint is named but not yet implemented (#5749).
- The engine's internal `Op`/`Event` types and the wire protocol's `Op`/
  `EventMsg` now carry a compile-enforced twin for every variant: adding an
  engine variant without a protocol counterpart fails the build instead of
  drifting silently. Internal durability work — no user-visible surface
  change yet (#5751).
- Machine tokens: with `CODEWHALE_API_KEY` set, the CLI authenticates as the
  Codewhale account with no local session file and no browser — the CI
  authentication path, with a typed token shape and redaction (#5721).
- Compaction publishes a structured survival contract for session-tree
  journal entry types (`crates/tui/src/compaction/SURVIVAL_CONTRACT.md`) and
  fails closed when the last user round, tool results, `/anchor` text, or
  checkpoint receipt would vanish (#4394, #5782).
- Internal: `codewhale-config` gains `RouteAuthoritySnapshot`, one immutable
  authority that owns a compiled provider catalog together with the route
  resolver projected from it, so a picker, a readiness view, and an execution
  path can no longer resolve against different catalog snapshots without a
  type-level signal. Resolution still goes through the sole resolver; the
  returned receipt distinguishes an exact catalog row, a custom-endpoint route
  whose provider facts are deliberately not reused, and an allowed
  pass-through route with no catalog row. All state is secret-free. No call
  site changed and no user-visible behaviour changed yet (#5766).

- Computer session records now count only time a provider actually accepted the
  session as active, at per-second granularity. Idle, queued, stopped, and
  teardown time are excluded, and a session whose allocation does not match a
  standard profile is refused rather than recorded approximately. Covered by
  hermetic fixtures; no live provider call and no deploy (#5781).

- Website: the public site moves to the Tideline deep-ocean design language
  (dark by default with an opt-in light documentation sheet, palette grounded
  in the TUI's WHALE_* tokens) and the new whale brand mark across the favicon,
  app icons, web manifest, nav wordmark, and social card (#5573).

- Add `codewhale dispatch` / `/dispatch` so a local session can propose a
  Codewhale cloud agent against an explicit `github`, `cnb`, or `gitee` remote.
  Confirmation is required; missing credentials fail closed; cloud jobs share
  the existing `/jobs` surface as `kind=cloud`. See
  [DAYTONA_CLOUD_DISPATCH.md](docs/DAYTONA_CLOUD_DISPATCH.md).
- `/login` reports the Codewhale account session and provider-key next
  steps. The internal cloud-agent credential is not user surface: there is
  no `auth set-slot`/`auth clear-slot` command, no hint, and no completion
  entry for it — signing in with `codewhale login` is the only door.
- Add the Tideline component family from the ratatui translation spec
  (#5698's screens, riding the #5699 work-strip layout): hero startup
  surface with quick actions and option strip, notifications inbox, merged
  footer band, pod ledger, receipt
  stream, theme list with motion toggles, live preview, settings rail,
  and the left rail — each a standalone render module pinned by 28 new
  byte-exact golden buffers. Frame wiring follows the Tideline acceptance
  gate; `NO_COLOR` is now honored by palette depth detection.
- Route Contract Phase 1: `RouteResolver` is the runtime path for
  `resolve_runtime_options`; `codewhale providers export --json` ships the
  owned descriptor catalog; CLI `--provider` accepts any catalog route id
  (the closed `ProviderArg` enum is deleted). Catalog layers are
  bundled → models.dev → provider `/v1/models` → config.toml → user, with
  policy DENY last and never overridden.
- Add provider-native web search for documented Xiaomi MiMo 2.5 Pro and 2.5
  chat routes while keeping neighboring models and custom gateways fail-closed.
- Add structured provider-native web search for exact Z.AI global and Zhipu
  China general API routes, using each site's documented search engine value.
  Existing `open.bigmodel.cn` configurations now share Z.AI's official model
  namespace and credential scope; unknown model IDs still pass through.
- Add provider-native web search for documented Qwen models on ModelStudio
  Token Plan's Responses Harness, without enabling Coding Plan or Anthropic
  routes.
- Added `/copy` to place the latest completed assistant response on the
  clipboard without copying tools, system messages, hidden reasoning, or
  active partial output (#5668).
- Add provider-native web search for documented DeepSeek V4 routes through the
  Responses API, with fail-closed capability gating for compatible custom
  endpoints.
- Add provider-native search for exact Moonshot K3 Formula, legacy K2.6
  built-in search, and Kimi Code membership `/search` routes. Treat the exact
  Moonshot China endpoint as a first-party direct route.
- Z.ai `GLM-5.3-Flash` and OpenRouter `z-ai/glm-5.3-flash` are first-class
  picker rows (`/model GLM-5.3-Flash`). Flash is the faster/explore sibling
  of `GLM-5.3`; the Z.ai default stays `GLM-5.3`. List price is $0.15/$0.50
  per 1M (durable; the 50% promo through 2026-09-09 is not the catalog row).
- Baseten, Groq, and Cerebras are bundled OpenAI-compatible setup templates
  (`[providers.<id>] kind = "openai-compatible"`), not new `ProviderKind`
  variants. `/provider` fills URL, model, and env from one catalog row.
- MCP manager copy now names the server, the failure, and one recovery
  command (`The X MCP server requires OAuth reauthentication. Run /mcp login X`).
- Settings Advanced exposes clickable MCP Connect, Reconnect, and Diagnose
  actions, while Extensions and Problems route recovery through the existing
  `/mcp login`, `/mcp reload`, `/mcp validate`, and `/plugin validate` commands
  (#5643, #5655).
- Plugin prompt matching (#5579): sending a task can toast the next review
  step when the prompt strongly matches an installed-but-idle plugin or a
  marketplace catalog you added (for example a Supabase prompt suggesting
  `/plugin trust supabase`). A live composer CTA offers the same review
  command while you type, matching turns append a bounded
  `<recommended_plugins>` user block, and `request_plugin_install` surfaces
  review without installing. `/plugin suggest` now ranks manifest keywords and
  catalog candidates, never auto-installs, and on-disk plugin changes also
  nudge `/plugin reload` between turns.


- Added `/import-claude` (#5557): reads `~/.claude.json` and
  `~/.claude/settings.json` read-only and renders an explicit, reviewable
  migration plan plus a written report. MCP servers route through the
  existing `/mcp import <name> --approve` consent flow, allowlisted env keys
  become an unapplied portable bundle for `codewhale config import`, and
  permissions/hooks map to manual follow-ups; secret-shaped values are named
  but never echoed or imported.
- Added the managed Chat relay: account-owned Chat commands now execute on the
  native runtime thread engine through a new `runtime_chat_relay` module
  instead of a second execution path. Each Chat thread is a dedicated,
  isolated runtime thread with an empty model-visible tool allowlist, a
  chat-only system prompt, and no workspace, memory, skill, credential, or
  local-path context; the active interactive TUI thread is never reused. The
  relay keeps one durable state file per scope behind a session-wide owner
  lock, binds a saved session permanently to its first remote target, replays
  exact duplicate commands from retained results instead of starting a second
  inference turn, terminalizes orphaned reservations on restart, and rejects
  (never queues into) a busy local Work turn with an immediate retryable
  result. Stop commands require the exact run and turn identity, and a worker
  aborts on any account or target mismatch during token refresh.

- Added a verified Omarchy/AUR packaging path: `packaging/aur` now carries a
  `PKGBUILD` template, `.SRCINFO` renderer and tests, release-candidate
  render step, and docs; the live AUR compatibility alias `codewhale-tui ->
  codewhale` is preserved, AUR dependencies match the live package, and the
  self-updater refuses to fight an Omarchy/package-manager installation.
- Added a Fleet run-wide cost/token usage ceiling (R6, #5567): an accumulator
  plus admission gate and alert bound a whole run's spend across workers.
- Added the real provider cache hit rate to `/context` (C3, DSH #3): the
  token-weighted provider-reported rate from the same telemetry `/cache`
  aggregates, plus an honest "no cache telemetry yet" state when nothing is
  recorded.
- Added per-step cost movement: `TurnUsage` receipts are priced as they land
  so the live cost surface moves during long agentic turns instead of only at
  `TurnComplete` (#5578), and the footer now honestly shows `cost: unknown`
  when nothing can be priced instead of hiding the line.
- Added the underwater session surface: the ambient water reads session state
  from across the room, its animation shape follows live agent activity, and
  attention tints are steady (no breathing) with a distinct reading treatment
  and no mid-water seam on tall windows.
- Added `@path:START-END` ranged line mentions (#5550, picker half to follow),
  a predictable last-copy backup file for clipboard exports with the path
  named when the clipboard fails (#5555), `wait` on multiple background shell
  tasks with `until=any|all` (#5549, named-kill half to follow), and composer
  double-click word / triple-click line selection.
- Added workflow parallel-`schema` slot partial success (#5583): a slot that
  produces a schema-valid partial result no longer fails the whole parallel
  branch.
- Added plugin load-failure startup hints (#5579) and OpenRouter app
  attribution headers on outbound requests.
- Added `codewhale doctor --fix` (with `--yes` for non-interactive consent):
  the doctor now computes a concrete repair plan — delete stale `.tmp*`
  leftovers from interrupted atomic writes, tighten secret-store file
  permissions to `0600`, disable structurally broken (Error-status) MCP
  entries through the atomic MCP save path, and scaffold missing user-global
  `skills`/`tools`/`plugins` directories — shows it, and applies it only after
  explicit consent. Every action is narrowly scoped and idempotent; JSON and
  context-JSON modes stay read-only and conflict with `--fix` (#5552, v1).
- Added `/context` reporting of the real provider prompt-cache hit rate (C3)
  alongside token pressure.

### Changed

- Provider-native web search now applies domain constraints before accepting an
  attempt, discards generated answers when returned citations violate those
  constraints, and preserves the caller's configured/local timeout as an
  independent fallback budget (#5681).
- Idle session metrics omit zero facts (`0 turns`, `LLM 0s`) until the
  runtime has evidence. Working chrome says `in the current` instead of a
  generic `working`.
- Deleted nine uncompiled `runtime_contract/` staging files. Live contracts
  remain `model.rs` and `termination.rs`.

- The first #5587 dead-code sweep converts audited test-only helpers to
  `#[cfg(test)]`, keeping production builds free of test-only APIs without
  changing runtime behavior.
- `/plugin reload` is now discoverable when on-disk plugin bundles change: the
  next send and `/plugin list` nudge once with `Run /plugin reload to apply`
  instead of silently keeping the stale catalog (#5579). Trust is unchanged;
  this does not auto-reload.
- Context-pressure warnings and critical alerts now remain visible in sticky UI
  status until compaction or explicit dismissal, instead of disappearing into
  scrolling turn metadata (#5620).
- The Runtime thread store defaults to a per-session root
  (`$CODEWHALE_HOME/sessions/<id>/runtime`) so multiple Codewhale processes on
  one machine no longer share one owner lock (#5630). The exclusive lock is
  unchanged; `CODEWHALE_RUNTIME_DIR` still selects a shared root when that is
  intended.
- Session token totals now include display-only per-model-call deltas while a
  turn is running, including input/output and cache-class counters; the
  authoritative `TurnComplete` totals still reconcile exactly once (#5581).
- Transcript focus now exposes per-block actions: `y` copies content, `Y`
  copies the rendered metadata view, Enter opens a fullscreen block pager, and
  `r` opens raw detail; the existing Tasks rail shortcuts remain unchanged
  (#5551).
- Provider neutrality (#5588): model resolution of omitted/aliased models is
  now provider-relative, OpenAI-native defaults no longer route through
  another provider's table, CLI credentials stay provider-scoped, and NVIDIA
  credentials no longer leak into the DeepSeek keychain. Neutrality test
  matrices exercise several providers instead of standing in with one.
- The workflow engine module was decomposed out of its 3k-line mega-file into
  `journal`, `usage`, and `report` modules plus a module directory (#5586
  slices 1a/B/C/D), a semantic-free move verified by normalized-content hash.
- Compaction refusals are now always named (#5577): silent holds under context
  pressure are gone, the context meter honors the same provider-billed prompt
  the trigger uses (T1), and prune outcomes are projected without cloning the
  transcript.
- Step budgets now end honestly: an ~80% soft landing nudges the model to
  write its final report, and exhaustion grants exactly one bounded
  report-on-exhaustion turn instead of dying silently (A1/A2).
- Contributor credit: recognized agent contributors (e.g. `Codewhale Agent`)
  may now carry `Co-authored-by` trailers; unknown bot/tool trailers are
  still rejected by the credit gate.

### Fixed

- Read-only Fleet workers no longer send `"action": {"enum": null}` in their
  projected `bash` schema. The read-only projection probed the action enum
  with a mutating index, which auto-vivified the key on schemas that have no
  action property, and strict OpenAI-compatible validators then rejected the
  whole request (`null is not of type "array"`). The probe is non-mutating
  now, in both the read-only projection and the `Run` arm next to it, and a
  regression test walks the whole projected catalog for nulls
  (#5944, thanks @gaord).
- Fast typing no longer corrupts the composer. The paste-burst heuristic ran
  on every session until a real bracketed paste arrived, holding, buffering,
  retro-grabbing, and absorbing Enter on timing guesses; it is now
  fallback-only (gated off when the terminal provides bracketed paste), the
  retro-grab is deleted, and Enter on held command text flushes and submits.
- Ctrl+C works on the pre-session launch menu and speaks everywhere: the
  first press arms the two-second exit window with a visible localized
  "Press Ctrl+C again to quit" hint (previously silent), the second exits.
  The worktree name input keeps Ctrl+C as cancel-input.
- Fresh interactive sessions no longer leave a phantom one-message duplicate
  behind. The TUI claimed one session id (Runtime store lock, turn-start crash
  checkpoint) while the engine minted a second one; the first `SessionUpdated`
  re-keyed the App, the completion commit cleared only the engine id's
  checkpoint, and `codewhale --continue` later "recovered" the orphaned
  checkpoint as a duplicate session instead of the real one. The engine now
  adopts the host-owned id at spawn (`EngineConfig::session_id`) and `/clear`
  mints the next id in the App like `/new`. A non-TTY `--continue` no longer
  promotes and consumes the crash checkpoint before failing the terminal
  check, and the root `codewhale --resume <id>` / `--session-id <id>` flags
  documented in the operations runbook now parse instead of being swallowed
  as a prompt.
- Website: `/signin`, `/signup`, and `/auth/callback` are locale-aware public
  routes instead of localized 404s. Sign-in and create-account send the person
  to the CWC app; OAuth callbacks hop to `app.codewhale.net` with the query
  intact; `/login` and `/register` are aliases. Local CLI use is not presented
  as requiring an account (#5767).
- The sandbox read deny-list matches a rule's resolved path as well as its
  literal spelling. On macOS `/etc` and `/var` are symlinks into `/private`,
  so a read of `/private/etc/sudoers` walked around the `/etc/sudoers` rule,
  and a rule written against a symlinked directory never fired for the real
  path that `canonicalize` and the process cwd hand back.
- Background shells are first-class work-strip rows (`▾ Shells N`) you can
  open, watch, and cancel by the `shell_*` id on the row. `/jobs cancel all`
  cancels running shells; it no longer looks up a task named `all`. The
  composer hourglass crumb no longer stands in for a shell surface.
- `codewhale logout` and `/logout` now clear the Codewhale account session
  and the Daytona secret slot, not only provider API keys. The TUI crate's
  leftover `login --api-key` path no longer claims to save a key.
- Account sessions no longer read the macOS Keychain. Unsigned or rebuilt
  `codewhale` binaries were a new Keychain ACL principal every time, so
  `codewhale web` and the TUI popped a password dialog on start. Sessions
  now use `~/.codewhale/secrets/secrets.json` (mode 0600), the same store
  as provider keys. Extracted from the Keychain-retirement half of #5632.
- Hardened the dispatcher-side config parse the same way: `ConfigStore`
  loads and project-config parsing now deserialize `ConfigToml` on a
  dedicated 16 MiB-stack thread (the guided-setup save path could overflow
  a 2 MiB worker stack the same way the TUI's `ConfigFile` parse did), and
  the #5585 setup-confirm toast test runs its runtime on an equally sized
  thread instead of overflowing the default libtest stack.
- Fixed detached interactive agents reporting worker usage after the parent
  turn ends with the usage missing from the session/live `/cost` total
  (#5597): interactive turns acquire an owner-scoped runtime usage lease,
  late usage enters the session cost pool without reopening the sealed
  mailbox, and worker/session/reload accounting share one hashed response
  identity so retried deliveries stay exactly-once.
- Fixed the sub-agent fiasco class: in-workspace absolute `git -C` no longer
  trips the read-only child shell gate with a coherent bounded gate (#5595),
  turn end parks turn-owned children resumably instead of silently cancelling
  them (#5596), stale write-claims are released by liveness with coordinate
  release (#5562), and the verifier role description matches its real
  surface (#5562).
- Fixed workflow responseSchema failure handling (#5583): bounded repair with
  typed receipts (kind + attempt), raw-output receipts persisted as artifacts
  that survive reload, and failures surfaced instead of null success (#5528).
  Degraded owner snapshots no longer project as ordinary Completed runs
  (#5582), typed task error kinds enable fail-fast parallel/pipeline (R9), and
  `/workflow confirm` finds the draft despite interleaved messages.
- Fixed the DeepSeek session that never auto-compacted at 842k/1M (#5577): the
  exact `billed_842k_on_a_1m_window_compacts_despite_a_small_estimate`
  regression pins the trigger.
- Fixed sandbox escapes and authority gaps: workspace-write tools resolve
  hard-linked files before writing (S2, #5569), an opt-in read deny-list
  bounds full-disk reads in any posture (S1, #5568), and session grants match
  the command family instead of widening the whole tool (R2).
- Fixed provider-transport robustness: non-streaming HTTP requests are bounded
  by a read timeout (R4), and Chat-Completions mid-stream error frames surface
  instead of hanging (R3). MCP OAuth expiry now reacts to 401/403 with a named
  recovery path and login hint instead of looking like a broken server (T4,
  #5572).
- Fixed Fleet lifecycle gaps: detached workers are reaped when their manager
  dies (R7) and the per-task wall-clock timeout is enforced (R5); the local
  Fleet host compiles on non-Unix targets; detached schema repair stays within
  the runtime budget.
- Fixed prompt-cache correctness: the tool catalog is fingerprinted in
  provider wire order (C1), prefix changes between requests are always
  declared (C5) — including the newly named mid-turn `change:tool_surface`
  reasons for deferred-tool admission, tool-search activation, and runtime MCP
  tool arrival — and a reasoning-only response stopped at the output limit now
  fails the turn honestly instead of re-issuing a request that can only
  reproduce the stop.
- Fixed docs/public-surface drift: REBRAND.md no longer presents
  `api.deepseeki.com` as a China fallback (#5564), and the web facts/tool
  counts match the repository.

- Heavy pull requests now run the real Linux workspace nextest, doctest, and
  lockfile lanes directly on GitHub Actions instead of reporting a green
  placeholder while waiting for a branch-specific CNB mirror (#5547).
- Fleet roster members in a selected Fleet now expose a visible edit affordance
  and a direct `m` model-picker shortcut, while the coordinator row remains
  read-only (#5604, covers #5589).
- The context inspector now attributes bounded tool-schema cost to the built-in
  catalog and each discovered MCP server without changing prompt assembly or
  cache behavior (#5553).
- The goal-continuation quiet period (`[goal] continuation_delay_seconds`,
  added in #5508) now applies on every dispatch path. Previously the
  within-turn dispatch hook fired the next continuation prompt immediately
  whenever a model step ended with the goal still active, so CLI-resumed and
  host-managed sessions never observed the configured delay (measured
  end-to-start gaps of ~9 s with a 300 s delay configured). The wait now
  runs unconditionally inside `goal_continuation_message_if_needed`,
  remains cancellable, and pause/clear/terminal `update_goal` calls still
  cancel a pending pass (#5534).
- Two unit tests no longer depend on process-global environment state that
  sibling tests mutate concurrently.
  `route_budget::tests::v4_trigger_uses_window_percent_when_it_fits_spendable_input`
  asserted no-override output-budget values while reading
  `CODEWHALE_MAX_OUTPUT_TOKENS` / `DEEPSEEK_MAX_OUTPUT_TOKENS` without
  holding `lock_test_env()`, so a concurrent writer could flip the value
  mid-assertion (the order-dependent full-suite flake).
  `prompts::tests::system_prompt_prefix_never_leaks_private_content` read the
  real home via `HOME`/`USERPROFILE`, so a machine with
  `~/.codewhale/instructions.md` leaked its absolute path into the prompt and
  failed the no-private-paths assertion unless a sibling's temporary `HOME`
  guard happened to be live. Both tests now hold the env barrier and pin the
  variables (the route-budget test removes the overrides; the prompts test
  points `HOME`/`USERPROFILE` at a scratch dir). Assertions unchanged.

- TUI sessions now apply a subtle OSC 12 cursor accent only on explicitly
  supported terminals and restore the terminal default through normal, panic,
  and signal cleanup paths (#5554).

## [0.9.11] - 2026-08-22

Codewhale v0.9.11 tightens the long-running agent loop, makes workflow
failures visible instead of successful-looking, adds an experimental
vision-capable DeepSeek route, and prepares reproducible Codewhale-versus-Pi
evaluation without publishing a result before a real run. The complete
item-level change record is retained below the categorized release highlights.

### Added

- Added first-party `deepseek-v4-flash-vision-exp` discovery and selection for
  DeepSeek, including the `flash-vision` alias, bundled offline metadata,
  registry and picker entries, and image-input capability on the chat route.
  Context and output limits inherit from V4 Flash until DeepSeek publishes
  distinct values; pricing remains unknown rather than guessed.
- Added a provider-controlled Codewhale-versus-Pi parity harness with three
  hermetic coding tasks, route and reasoning-effort receipts, doctor/dry-run
  modes, and bounded result artifacts. The repository ships the harness, not a
  benchmark verdict; comparable real runs remain an acceptance gate.
- Added portable, secret-free config export/import with a reviewable plan,
  explicit headless consent, backup and rollback, and idempotent re-import.
- Added bounded multi-file diagnostics through the existing model-facing `lsp`
  tool without increasing the tool-catalog count. Thanks to **Isabel Wu
  ([@wuisabel-gif](https://github.com/wuisabel-gif))** for PR #5524.
- Added portable presentation, media-attachment, and operation-digest facets to
  the command contract, then moved all seven utility handlers onto the
  contract-backed dispatch path. Thanks to **Paulo Aboim Pinto
  ([@aboimpinto](https://github.com/aboimpinto))** for PR #5525.

### Changed

- Sub-agent, Fleet-worker, workflow-task, and thread-runtime model turns no
  longer inherit a hidden role-based step ceiling. An omitted or zero
  `max_steps` is unbounded; a positive user/config value remains an explicit
  cap and is still clamped to the runtime safety ceiling. Wall-clock, provider,
  heartbeat, cancellation, and admission safeguards are unchanged.
- `/rc` now mirrors one shared session rather than transferring terminal
  ownership: local and web prompts remain available while idle, approvals use
  first-decision-wins semantics, and transport/integrity failures remain
  fail-closed.
- The terminal status rows around the composer are now two stable bands:
  `provider · model · thinking level` is the persistent identity row below
  the composer in every phase, and a separate activity row above the
  composer carries the live phase, notices, and cost/metrics. Sending a
  prompt no longer relocates the route identity above the composer, and
  neither row ever duplicates it.
- The embedded local Web client now uses the current CWC Ocean hierarchy and
  readable control sizing, follows the shared Enter/Shift+Enter composer
  grammar, and chooses a provider plus model per new thread without mutating
  Runtime defaults. Exact image-input capability is labelled honestly; a
  vision-capable route does not imply that browser attachments exist.
- The runtime now has one authoritative model-turn loop. The placeholder
  `crates/core` engine tree is gone, while the active TUI loop and its extracted
  tool-call stages retain existing policy, hook, cancellation, and budget
  behavior. Thanks to **Sun Zhenyuan
  ([@bistack](https://github.com/bistack))** for PR #5523.

### Fixed

- Chat Completions streams now require terminal proof from `[DONE]` or a
  non-empty `finish_reason`. Protocol-only frames no longer count as answer
  content or time-to-first-token, and a provider continuation that ends after
  tool results with no answer or tool call fails durably instead of producing
  a false `Completed` receipt.
- A selected v2 Fleet now drives one bounded, deterministic Agent roster across
  terminal and runtime surfaces. Fleet operator/member/explicit-route
  precedence, resolved member identity, and exact `vision` requirement
  admission now fail visibly instead of silently falling back, first-matching,
  or rerouting.
- A workflow whose `task()` dispatch was rejected no longer loses that failure
  inside a `parallel()` null slot or presents a successful-looking run. Rejected
  dispatches now fail the run, persist as typed bounded receipts with an exact
  count, and appear in transcript, activity detail, and workflow-panel views.
- Provider readiness, credential-source explanations, focused-agent scrolling,
  compact `/status` and `/help` rendering, shell/web output bounds, MCP
  lifecycle reporting, and narrow-terminal onboarding received the detailed
  fixes recorded below.
- Portable config import/export now preserves typed tables, arrays, numbers,
  booleans, and datetimes without stringifying them, while refusing
  machine-bound trust overlays, credential readers, automatically executable
  hooks/LSP definitions, local-path authority, machine-local network proxy
  routes, cookies, redaction placeholders, and nested or camel/dotted
  credential keys. Project and global bundle operations now load and validate
  the document for their actual scope in both directions, including a
  workspace whose document still lives under the legacy app directory.
- `codewhale login` now means Codewhale account sign-in (the same browser
  device flow as `codewhale account login`, with `--no-open` and
  `--timeout-seconds`); provider API keys are configured exclusively through
  `codewhale auth set --provider <provider>`, and the hidden legacy
  `--api-key`/`--provider` flags redirect loudly instead of silently writing
  a key.
- Account sessions prefer the OS credential manager and now fall back
  automatically to the private `0600` Codewhale secrets file on headless
  hosts, SSH boxes, and containers; the `CODEWHALE_CLOUD_ALLOW_FILE_SESSION_STORE`
  opt-in is deprecated and ignored.
- `/update` gained a `Ctrl+Shift+U` install chord (catalogued in
  `docs/KEYBINDINGS.md`, localized in all 15 packs) and a startup hint that
  names the previous and current version on the first launch of a newer
  build, pointing at `/change`.
- Fleet product-model copy pass: the TUI, docs, and locale packs now use
  Fleet / Member / Role / Model / Access / Saved consistently so a user can
  say "scout", "DeepSeek V4 Flash", or the member name and mean the same
  thing.
- A reasoning model that returns only hidden reasoning and a clean stop (no
  answer, no tool call) is now re-requested automatically up to twice before
  the turn fails, instead of dead-ending with "the provider response was
  incomplete." The retry reuses the cached prefix so it is cheap; an
  output-length stop (`length`/`max_tokens`) is never retried, and a
  persistently answerless model still fails honestly after the bound.
- Model-bound tool results now use a credential-shaped redaction policy:
  only values that look like secrets (known prefixes, JWTs, bearer tokens,
  PEM private-key blocks, long opaque strings) are masked before a `read` or
  shell result reaches the model, so code such as `password:
  credentials?.password` or `"password-validator": "^5.3.0"` stays byte-exact
  for edits and read-back. Exact configured credential values are still always
  replaced, and logs/previews/exports keep the broad key-based scrubber. (#5546,
  reported by @ronohara)
- Terminal input shutdown no longer waits forever on a wedged TTY read,
  Windows launch receipts can atomically replace an existing record, and the
  complete `/status` report now follows the active locale without rewriting
  exact custom-provider identities that contain brace-like text.
- Localized READMEs again match the English install and third-party-notice
  surface, including shell-completion guidance in all 18 translations.

### Security

- Unified OAuth device-code polling now validates verification URLs before
  opening them, redacts token-bearing types, honors server slowdown intervals,
  and keeps credential save/logout mutations serialized.
- Project instructions, rules-directory traversal, secret-shaped config data,
  URL fingerprints, and shell network authority now retain the explicit bounds
  and fail-closed behavior described in the detailed record.

### Contributors

- **xyzs996 ([@xyzs996](https://github.com/xyzs996))** — DeepSeek V4 whole
  Beijing weekends are billed as off-peak, and the scorecard window comment now
  matches (#5545).
- **Sun Zhenyuan ([@bistack](https://github.com/bistack))** — tool-call stage
  extraction with the existing execution and policy contracts preserved
  (#5523).
- **Isabel Wu ([@wuisabel-gif](https://github.com/wuisabel-gif))** — bounded
  multi-file `read_lints` support (#5524), plus independently reviewed
  completion-routing overlap in #5530.
- **Lstarsky0 ([@Lstarsky0](https://github.com/Lstarsky0))** — maintainer review
  hardening for truthful per-file states and truncation metadata in #5524.
- **Paulo Aboim Pinto ([@aboimpinto](https://github.com/aboimpinto))** — portable
  presentation/media/digest facets and the seven utility-handler migrations
  (#5525).
- **RepentStar ([@RepentStar](https://github.com/RepentStar))** — reported and
  reproduced the stale completion-generator path and missing `codew`
  registration fixed for #5526.

### Detailed change record

The notes below are preserved in full so the categorized highlights do not
erase behavior, migration, security, compatibility, or verification details.

- Provider completion is now evidence-based. A Chat Completions stream reaches
  `MessageStop` only after `[DONE]` or a non-empty `finish_reason`; raw EOF
  without either is a typed failure. Message-start, ping, usage/terminal
  deltas, block-stop, and message-stop frames do not count as productive
  content or mint time-to-first-token. After tool results, a terminal provider
  step with no answer or tool call now emits a durable failed turn and never
  fabricates an empty assistant message.

- A selected v2 Fleet is the single effective Agent roster across terminal,
  Runtime threads, direct Workflow, Fleet execution, doctor, and
  setup/readiness; legacy profile layers are consulted only when no Fleet is
  selected, and invalid selections fail visibly with bounded, redacted errors.
  Member references resolve exact id first and otherwise require a unique
  display name, role, pinned model, offline model name, or provider/model route;
  `agent action=roster` exposes that same bounded roster. The Fleet operator
  supplies fresh-root and inherited-member routing unless an explicit launch
  route or member pin wins, the resolved member is shown separately from the
  requested alias, and `requires = ["vision"]` is admitted only on an exact
  route with verified offline `image_input` support—never by silent rerouting
  or custom-proxy inference. Fleet selection remains an explicit user/folder
  contract independent of legacy project-profile loading.

- **Breaking (app-server):** `/prompt`, `prompt/request` and `prompt/run` now
  execute a real model turn instead of reporting success for work they never
  did. `Runtime::handle_prompt` called no model: it resolved config, ran a
  local `ModelRegistry` lookup, emitted three canned hook events
  (`ResponseDelta` was literally the string `model-selected`), and returned
  HTTP 200 with `output` set to a stringified JSON echo of the caller's own
  routing metadata — the prompt included. Worse, when a `thread_id` was
  supplied it appended a real user row, flipped the thread to `Running`, and
  then wrote that echo into durable history as an **assistant message** plus a
  `prompt_response` checkpoint. Nothing marked the row synthetic and nothing
  ever moved the thread out of `Running`. All three endpoints now route
  through the same `RuntimeBridge` that stdio `thread/message` has always
  used, so `output` is the model's streamed text, `model` is what the runtime
  reports for the thread that ran the turn, and `events` are the real
  streaming frames. `Runtime::handle_prompt` and its synthetic history write
  are gone.

- **Breaking (app-server):** a failed prompt is now a typed failure rather
  than a success-shaped body. `POST /prompt` returns
  `{"error":{"code":...,"message":...}}` with `400` (invalid request), `404`
  (thread not found), `503` (`runtime_unavailable`) or `500`, instead of HTTP
  500 carrying a `PromptResponse` with the error text stuffed into `output`
  where model text belongs. The stdio surface gained JSON-RPC `-32005`
  `runtime_unavailable` for "the turn engine could not be reached, so nothing
  ran" — distinct from `-32603`, and retryable. There is no configuration in
  which a prompt silently echoes instead of running.

- **Breaking (app-server):** `POST /thread` with a `Message` body runs the
  turn. It previously replied `status: "accepted"` with a
  `ResponseDelta("queued")` frame while starting no worker and calling no
  bridge — the stdio path for the same request has always done real work, so
  the two transports disagreed about what `accepted` meant. HTTP now replies
  `status: "completed"` once the turn reaches a terminal state, with the
  streamed frames in `events` and the turn id in `data`. `Runtime::handle_thread`
  no longer accepts `ThreadRequest::Message` at all: it owns thread
  bookkeeping, not the turn engine, and returns an error naming
  `POST /v1/threads/{id}/turns` rather than a canned acceptance.

- **Breaking (app-server):** `AppRequest::SubmitUserInput` now refuses
  explicitly (`ok: false`, `error: "user_input_reply_unsupported"`) instead of
  returning `resolved: true` and filing the answers in a map that had no
  reader anywhere in the crate — every answer submitted was silently
  discarded. It cannot be made to work on this transport: while a turn
  streams, the stdio loop executes only `thread/interrupt` and queues
  everything else, so an answer sent there would wait on the very turn
  waiting for it. The refusal names the surface that does accept it,
  `POST /v1/user-input/{thread_id}/{request_id}` on the runtime API. The
  client-visible refusal is whitespace-clean, and the `/tool` path that mints
  the `UserInputRequest` is unchanged and still genuine.
- Split the coordination ledger out of `tools/subagent/coord.rs` into
  `tools/subagent/coord/ledger.rs`. The file held two unrelated things: the
  model-facing `agents/*` tool wrappers, and the durable decision/claim/
  contention records those wrappers happen to write — records whose consumers
  are mostly *not* in the tool layer (`tui::coordination_detail`,
  `tui::work_surface`, `tui::ui::tests`, `core::engine::tests` all name these
  types). At 3.8k lines, reading either one started by scrolling past the
  other. A pure move with a glob re-export from `coord`, so every
  `crate::tools::subagent::coord::{…}` path still resolves and no consumer file
  was edited; the only content change the move required is one constant going
  from private to `pub(super)` because its caller stayed behind. `coord.rs` is
  now 2.3k lines and `ledger.rs` 1.6k.

- `agent` is now the only sub-agent tool the model can see. `AGENTS.md` has
  said "the model-facing sub-agent surface is `agent` only" since the lifecycle
  tools were removed, but six more were reachable: `agents/list`,
  `agents/message`, `agents/followup`, `agents/interrupt`, `agents/coordinate`,
  and `agents/wait` all defaulted to model-visible, so they shipped in the
  catalog and `tool_search` could load any of them — and the `agent`
  description told the model they existed. They now declare
  `model_visible() -> false`, the same shape `rlm` and `exec_shell` use: still
  registered, still executable by name so a persisted transcript replays
  against the same implementation, never advertised and never returned by
  either `tool_search` matcher.

  Five of the six were already duplicates of an `agent` action. The sixth was
  not: `agents/coordinate action=claim` was the *only* way to widen a write
  claim, and write enforcement fails closed, so hiding it would have left a
  refusal ("expand it first with…") pointing at a tool the model could no
  longer call. `agent` gains one action, `claim`, taking the write scope
  vocabulary `action=start` already uses (`write_roots`, plus parse-accepted
  `exact_files` and `coordination_contracts`). It keeps `agents/coordinate`'s
  `Auto` approval — gating it deadlocks autonomous fan-in — and it can only
  widen the caller's own scope; peer contention still fails. A scopeless claim
  is refused rather than reported as granted, because `expand_write_claim`
  returns the unchanged claim with `Ok` when every list is empty.

  Collapsing six tools into one action set also collapses the gating: `agent`
  is deliberately exempt from both name-keyed gates (`posture_permits_tool`
  short-circuits it so delegation depth governs spawning, and
  `execution_envelope` classifies it `Bounded` so a read-only member can fan
  out read-only work), so a capability folded into it inherits no gate. `claim`
  is therefore gated per action, reproducing the envelope check that kept
  `agents/coordinate` off a read-only role's catalog — in the catalog and again
  at dispatch, since catalog shaping is not an authority boundary. The other
  actions keep exactly the visibility they had.
- One placement table now decides which wire channel a message role belongs
  in, and unrepresentable role/dialect pairs are refused at the outbound seam
  (`DeepSeekClient::prepare_outbound_request`) instead of at the provider.
  Chat Completions and OpenAI Responses used to drop an unfamiliar role
  silently, Anthropic Messages forwarded `message.role` verbatim and took an
  opaque provider 400 for it, and Google cloud-code was alone in failing
  closed. Positioned `system` and `developer` history — including compaction
  and branch summaries — is carried natively by Chat Completions and Responses
  and projected, in place, onto Anthropic's user channel. It is neither
  hoisted nor dropped. Genuinely unknown roles keep the previous
  dialect-specific fail-closed/omit behavior, now decided in one table. The
  dead `"tool"` arm in the Responses adapter is gone — nothing constructs that
  role.

- Message roles are a closed `Role` enum (`crates/core/src/role.rs`) instead of
  a free-form `String` on `Message`. Four wire adapters each decided
  independently what an unfamiliar role meant, and a typo in a role string was
  a silent transcript edit rather than a compile error. `Role` keeps an
  `Unrecognized(String)` variant and serializes via `as_str()`, so a saved
  session's bytes are unchanged, a transcript written by a newer build still
  loads here, and `assistant_interrupted` stays a distinct session item — no
  session schema bump and no migration ladder.

- Portable config bundles: `codewhale config export --portable` writes a
  deterministic, secret-free bundle (credential and machine-specific keys
  dropped), and `codewhale config import <FILE|URL|->` applies one with a
  strict versioned envelope, a printed added/changed/skipped/conflicting/
  rejected plan, consent gating (`--yes` required headless), a timestamped
  backup with rollback, and idempotent re-import. Credential-shaped entries
  are rejected by key name and value shape — rejections name the field,
  never the value. Remote imports revalidate the HTTPS-or-loopback-HTTP policy
  on every same-scheme redirect hop, and duplicate keys across applicable
  section labels fail before any backup or write instead of silently resolving
  by section order. Structured TOML values round-trip with their original
  types and exact named-provider identity; recursive sanitization covers
  arrays/tables and camel-case, dotted, cookie, and access-key spellings while
  retaining ordinary token-count metrics. Machine-bound project trust,
  credential-source consent, automatic hook/LSP execution, and local-path
  authority are non-portable and fail before mutation. A missing target is
  created transactionally and removed again on rollback, and project/global
  scope validation rejects the wrong document before import or export.
  Imported tables deep-merge portable fields into the target, so omitted
  machine-local provider credentials, endpoints, and executable definitions
  remain intact instead of being erased by a sanitized bundle.

- `/rc` is now a shared-session mirror instead of a terminal takeover.
  Attaching the web app no longer locks the local composer or hides
  approvals: both surfaces can prompt while idle (one turn runs at a
  time), approval cards stay visible in the terminal and are shared with
  the web with first-decision-wins semantics (the losing side is told, a
  web decision dismisses the local card), and structured questions are
  answered locally instead of cancelled. Fail-closed behavior survives —
  the post-failure reconnect lockout, integrity-gated `/rc stop`, and the
  fail-closed shared-approval channel on transport loss are unchanged.
  The takeover vocabulary ("web owns prompts and approvals") is gone from
  every surface.

- Auto-mode provider readiness no longer reports "key saved · not checked"
  forever. Readiness checks are recorded against the concrete model the
  router ran, but auto-mode reads resolved against the literal `auto`
  identity, which never matched any recorded check — so the setup receipt,
  model picker, and fleet setup view showed an eternal unchecked badge
  even after hundreds of successful turns. The read now falls back to the
  most recent check on the same route (provider + endpoint + auth class);
  concrete-model reads keep exact per-model scoping.

- The focused sub-agent transcript now scrolls like the main transcript.
  The frame renderer sampled the ocean column through a `ChatWidget` whose
  constructor consumed `pending_scroll_delta` — every PageUp/PageDown and
  wheel event was swallowed by an invisible widget before the focused pane
  could read it. The delta is now parked across the sample; the pane pins
  on user scroll-up, follows new child activity at tail, and
  jump-to-bottom releases the pin.

- Every Codex OAuth Responses request carried `max_output_tokens`, a parameter
  that endpoint rejects outright ("Unsupported parameter: max_output_tokens"),
  so every gpt-5.6-sol turn — including every sub-agent on that route — failed
  at the first request. Codex Responses bodies now ship without a client-side
  output cap; the backend applies its own. Every other Responses route keeps
  the central cap on the wire, exactly as before.

- The model-facing `lsp` tool now supports a bounded `read_lints` operation
  for multi-file, workspace-relative LSP diagnostics without adding another
  tool catalog entry (#4070).

- HTTP 400 classification no longer calls an unsupported-parameter error a
  context-window overflow. Responses shape errors such as "Unsupported
  parameter: max_output_tokens" name a token-shaped field, which the generic
  keyword rules read as prompt-size exhaustion and pointed users at compaction
  that could never help. Such responses now classify as invalid requests.

- xAI device login validated nothing about the URL it opened. The
  `verification_uri` from the device-code response went straight to
  `webbrowser::open` with no parse, no scheme check and no credential check, so a
  spoofed or compromised issuer could hand the platform's "open this" call a
  `file:` path, a custom application scheme (`vscode://`, `slack://`), or a
  credential-bearing URL. The shared primitive now refuses anything that is not a
  web page before the URI is printed or opened. **Behaviour change:** a
  non-loopback plain-`http:` verification URI now aborts login where it
  previously opened; `http:` on a loopback host is still allowed, because local
  runtimes legitimately use it.
- The xAI OAuth types no longer print bearer material through `Debug`. Five types
  holding tokens (`GrokAuthEntry`, `TokenResponse`, `DeviceCodeResponse`,
  `DeviceCodeGrant` and the poll outcome) either redact or no longer derive
  `Debug` at all, so a token has no printable path through a `{:?}` on any
  surrounding struct. The shared `DevicePollOutcome` derives nothing, which the
  compiler enforces.
- **Behaviour change:** an `interval` of `0` from the authorization server now
  falls back to RFC 8628's five-second default rather than a one-second floor,
  in both the xAI and account device flows.

- OAuth device-code login is now one implementation. xAI/Grok device login and
  Codewhale account login each carried their own hand-rolled RFC 8628 polling
  loop with nothing shared between them; both now call a single primitive
  (`codewhale-config`'s `device_code`), ported from pi. Three fixes come with
  it. `slow_down` now honours a server-supplied `interval` instead of always
  adding five seconds, which is what stops polling from running early forever
  under WSL and VM clock drift. Timing out after a `slow_down` now says so and
  names clock drift, rather than reading as a plain timeout. And the xAI
  verification URI is validated before it is handed to the browser opener —
  Codewhale previously opened whatever the device-code response said, so a
  spoofed or compromised issuer could point the platform "open this" call at a
  `file:` path or a custom application scheme. It must now be `https:`, or
  `http:` on a loopback host for self-hosted issuers. Stored credential files
  are unchanged and existing logins keep working. MCP OAuth is untouched: it
  delegates to `rmcp`/`oauth2` and was never hand-rolled.
- Shell output truncation now stays inside its own budget. A truncated shell
  result keeps a 6 KB head, a 24 KB tail, and any high-signal lines rescued
  from the omitted middle — but that rescued block was bounded only by a line
  count. One rustc `error:` line carrying a long inferred type or a minified
  bundler frame is routinely hundreds of kilobytes, so a "30 KB" result could
  arrive at 430 KB with the omitted line pasted back in whole. Each rescued
  line is now clipped and the block has a 4 KiB ceiling; the signal survives,
  the payload does not.

- Fetched web pages in non-Latin scripts no longer arrive half-read. Page text
  was reflowed against a column budget measured in bytes, so Cyrillic and Greek
  wrapped at roughly half the intended width and CJK at two thirds — and since
  the page view is delivered by line count, the surplus lines pushed real
  content off the end of the window. A Russian or Japanese URL returned a
  fraction of the text an English one did, for the same call. Wrapping now
  measures display width.

- The `bash` tool no longer tells the model it has no default timeout when it
  does. An omitted timeout has always been bounded at 120 seconds and the
  command killed there, but the tool description and its `timeout` field both
  claimed otherwise — steering the model away from the one parameter that
  would have saved a longer build. Both now name the real bound.
- MCP servers no longer restart because an unrelated setting was saved. The
  lazy config reload re-reads every watched source whenever one of their
  mtimes moves and keeps the live connections only when the content hash
  matches — but the hash was taken over `serde_json` bytes produced straight
  from the config's `HashMap`s, and two `HashMap`s with identical contents do
  not iterate in the same order. Any touch of any watched file therefore hashed
  differently, tore down every connection, and SIGTERMed and respawned every
  stdio child. Keys are now sorted before hashing.

- An MCP server marked `required` now still tells you *why* it failed to start.
  `connect_all` appended a generic "required MCP server failed to initialize"
  entry after the real per-server error, and the snapshot folds those pairs into
  a map keyed by server name — so the contentless entry replaced the diagnosis
  and /mcp showed the marker instead of "No such file or directory". The marker
  is now only synthesized when nothing else reported a cause.

- A crashed stdio MCP server is now rebuilt instead of being handed back dead.
  A failed transport *read* disconnected the connection; a failed *write* did
  not, so after the child exited the connection stayed `Ready`, the pool reused
  it on every later tool call, and /mcp kept listing the server as connected.

- An MCP response carrying neither `result` nor `error` is now an error rather
  than an empty success. It previously reached the model as a successful tool
  call with a `null` payload, indistinguishable from a tool that did nothing.
  An explicit `"result": null` is still a valid empty success.
- Stdio MCP server requests are answered while the client is idle instead of
  blocking behind the zero-capacity response rendezvous until an unrelated
  client call. `ping` receives its prompt empty result, unsupported methods
  receive JSON-RPC `-32601`, and the reader keeps only a weak stdin handle so
  dropping the client still delivers graceful EOF.
- `base_url_fingerprint` is a persisted-key change for two input shapes.
  The digest is serde-serialized into `ProviderCatalogCache` and
  `LiveOffering`, pricing defect receipts, and
  `TurnRecord.routed_usage_source_ids` — it is not an in-memory-only cache
  label. Empty or whitespace-only values (and scheme-less query-only strings
  that strip to an empty authority) now hash the invalid-or-secret-bearing
  sentinel instead of SHA-256 of the empty string. Scheme-less URLs that
  contain `@` now strip `userinfo` before hashing, matching the
  scheme-bearing branch, so a typed `user:pass@host/v1` no longer embeds the
  password in a stored digest. `routed_usage_source_fingerprint` feeds
  arbitrary scheme-less source ids into the same function, so a turn
  rehydrated from an older build can fail to dedupe one routed-usage row.
  Recovery is a cache miss and a re-fetch, not corruption. Empty input was
  not restored to the old digest: an empty authority is not a usable
  endpoint, and mapping it to the same sentinel the scheme branch already
  uses for an empty host keeps invalid inputs from minting a unique cache
  scope.

- Diagnostic lines that mention token *counts* are no longer swallowed by
  secret redaction. A stream error such as `max tokens = 8192 but budget =
  4096` was matching the `token` hint as a substring of the English word
  `tokens`, and the spaced-assignment pass then dropped the rest of the
  line, leaving `max tokens = [redacted]`. Token counts are not credentials;
  the hint now matches a credential identifier (`token`, `api_token`) rather
  than an English word, so the numbers survive while `token = Bearer …` is
  still redacted.
- Dashboard thread search no longer loads every thread's transcript to decide
  whether the row matches. `GET /v1/threads/summary?search=` walked the full
  thread list and called `get_thread_detail` on each row before matching, and
  that detail read is itself a whole-store walk of every turn JSON and every
  item JSON. A non-matching keystroke was therefore
  O(threads × (all_turns + all_items)) file reads — on the order of 10^8 JSON
  parses at a few thousand threads. Search now matches `id`, title, and model
  from the thread record (and, when the title is unset, the single latest-turn
  file that supplies the displayed title) and loads detail only for matches, so
  preview stays a display field rather than a search key. Session summary
  already refused to search last-message text for the same reason.

- Silent `#[allow(dead_code)]` suppressions on the modules AGENTS.md warns
  auditors not to delete — prompt zones, context budget, the route seam —
  and on the next-largest holders (palette tokens, hotbar actions, core
  events) are now `#[expect(dead_code)]`, or gone where the lint was already
  stale. A suppression that stops matching the lint fails the `-Dwarnings`
  gate instead of sitting quiet. The same gate is recorded in
  `[workspace.lints]` so member crates inherit it from the manifest rather
  than only from CI `RUSTFLAGS`.
- "missing key" now says where it looked. The provider picker reported
  credential readiness as the bare strings `missing key` / `key:not-set`,
  which named no source at all — so a home whose secret store held a working
  DeepSeek key could show `DeepSeek  missing key` in the picker while a real
  turn from that same home completed, and nothing on screen said which layer
  disagreed. Every row now resolves through one sourced resolver and states
  the place its credential came from ("OPENROUTER_API_KEY", `secret store
  "deepseek"`, `[providers.x] api_key`, "xAI OAuth", a consented external CLI
  file); a row without a credential lists the places that were probed, in
  precedence order, and the command that fixes the first of them. Where a
  durable slot is deliberately *not* read — an inactive provider whose config
  table carries no api-key marker — the row says so rather than implying an
  empty slot.

- Provider credential precedence is now stated once, in a doc comment beside
  the single resolver that enforces it, instead of being implied by a
  150-line cascade of provider special cases. No precedence decision changed:
  `has_api_key_for` is now a wrapper over that resolver, and a test asserts
  the two agree for every provider.

- Credential saves and logouts no longer interleave. Both took a snapshot of
  the durable slot, wrote it, mutated the config document, and rolled back on
  failure, with no lock held across the sequence — so a save racing a logout
  on the same slot could leave the secret store and the config file
  disagreeing. Both now hold that provider's credential write lock for the
  whole read-modify-write.

  Design ported from pi-mono (MIT, Copyright (c) 2025 Mario Zechner); see
  `docs/THIRD_PARTY_NOTICES.md`.

- Enumerating stored credentials no longer fails closed on one bad slot.
  Listing used to propagate a backend read error, so a single unreadable
  secret-store entry made `/provider` and logout treat every other stored
  credential as missing. Enumeration now skips the unreadable slot and
  continues, matching the probe loop it replaced.

- The first screen of first run no longer cuts its own headline. The welcome
  and ready titles, and the provider-step heading, were emitted as single
  unwrapped lines while the sentence beneath them wrapped, so at 40 columns
  German read "Codewhale arbeitet mit dir in diesem O", Russian lost its final
  stop, and Japanese lost "します。". Headings are prose and now wrap like it,
  in every shipped locale.

- The workspace-trust screen no longer cuts its own question in half on a small
  terminal. The question, the prompt-injection risk hint, and the trust-effect
  hint were each pushed as one unwrapped line, so at 40 columns the screen read
  "Should Codewhale work with the instruc" — severed mid-word with nothing
  marking the cut, while the workspace path directly beneath it wrapped
  correctly. Asking someone to grant filesystem trust while the question itself
  is truncated is the worst place in the product for that to happen. All three
  now wrap through the same helper the rest of onboarding uses, which also
  means they wrap correctly in Japanese and Chinese. Verified across all
  fifteen shipped locales at 40, 60, 80 and 120 columns.
- `codewhale completions <shell>` generated a script for the wrong program.
  The subcommand forwarded to the in-tree `codewhale-tui` binary, which
  rendered completions from *its own* clap tree under *its own* name, so the
  output ended in `complete -F _codewhale__tui ... codewhale-tui` (bash),
  `#compdef codewhale-tui` (zsh), and
  `Register-ArgumentCompleter -Native -CommandName 'codewhale-tui'`
  (PowerShell). Sourcing it registered nothing for `codewhale` or `codew` —
  the two commands current installers expose — so tab completion appeared to
  do nothing. The forwarded tree was also stale against the real CLI: it offered
  `pr`, `scorecard`, and `session-diagnostics`, which `codewhale` does not
  have, and omitted `run`, `rc`, `config`, `model`, `thread`, `lane`,
  `workflow`, `web`, `account`, `app-server`, `mcp-server`, `metrics`,
  `update`, `cloud`, `completion`, and `lane-log-proxy`, which it does.
  Completions are now rendered in-process from the CLI's own command tree,
  and `completions` is an alias of the existing
  `completion` subcommand rather than a second, divergent path. Regenerate any
  script you installed from an earlier release. Reported by **RepentStar**
  (#5526); part of the `deepseek-tui`-era identifier retirement in #5443.

- Completion scripts now fire for the `codew` shorthand as well as
  `codewhale`. Releases publish `codew` as a byte-identical copy of the
  `codewhale` binary, so a script bound to only one of the two names was half
  installed for anyone who types the short one. Each shell gets its own
  idiomatic hook rather than a second copy of the script: bash re-binds the
  generated function, zsh widens the `#compdef` tag line to
  `#compdef codewhale codew`, fish adds `complete -c codew -w codewhale`,
  PowerShell registers `-CommandName 'codewhale','codew'`, and Elvish aliases
  the completer with
  `set edit:completion:arg-completer[codew] = $edit:completion:arg-completer[codewhale]`.

- Documented shell completions. `docs/INSTALL.md` § 8 now gives the generate
  and install commands for bash, zsh, fish, PowerShell, and Elvish, with a
  note to regenerate after upgrading and to delete scripts produced by
  v0.9.10 or earlier. There was previously no completion documentation
  anywhere in the repository, which is how #5526 was reported as three
  problems instead of one.
- `/status` was 31 rows. On an 80x24 terminal the transcript viewport is 18, so
  typing `/status` landed you on the *tail* of the report: the version, route,
  directory, mode and sandbox rows had already scrolled past, and what stayed on
  screen was five `not reported` rows and a `$0.0000`. The report is 18 rows on a
  fresh session — `Window override:` is present unless the value is already
  configured. That is the viewport's height, so once `/status` itself occupies a
  history cell the title row still scrolls off; it does not fit that terminal
  whole. Provider, model and reasoning effort are one `Route:` lockup, the way
  the header rail already writes them. Mode and its permissions are one statement
  of posture. `Rate limits:` is gone — it was a `push_row` of a string literal
  and could never say anything but "not available from provider telemetry". The
  per-turn token ledger is gone too, because `/tokens` is that ledger's whole
  subject and `/status` was printing six rows of it at the same weight as the
  sandbox policy; the two facts that lived nowhere else, the cumulative in/out
  split and the cumulative cache totals, survive on one `Session tokens:` row.
  `Footer items:` no longer prints ten internal config keys across the full
  width — `/statusline` owns them, and the report now points there in the same
  row that points at `/tokens`. Two blank gutters do the grouping; the
  `===================` rule under the title is gone.

- The `/status` window-override key has its own labelled row. It used to be
  parenthesised onto the end of the provenance row, which pushed
  `context_window in config.toml` past the right edge at 80 columns and wrapped
  the sentence. `Window source:` states the provenance and `Window override:`
  names the exact key — and the override row is omitted entirely when the value
  is already configured, rather than advising you to set what you have set.

- `/help` no longer truncates anything. Every label and description used to run
  through a `truncate_to_width` that appended `…`, which in a two-hundred-row
  list promises text no keystroke can reveal and lands mid-token:
  `(aliases: /qin…` left the parenthesis hanging open. Descriptions now shed
  whole fields — the alias parenthetical first, then trailing clauses at their
  own joints, and only where there is no joint at all, the sentence's short form
  on a whole word with no mark, keeping the head noun of a simple verb +
  modifier + noun phrase rather than the adjectives that qualified it. The
  focused row's description is restated under the filter at the panel's full
  width, *only when the row itself could not hold it*, so a wide terminal does
  not say the same sentence twice. At 60 columns that restatement is itself
  shed — the `/advisor` detail stops before `session` — so the detail is longer
  than the row, not a copy of the original sentence.

- The `/help` label column is measured instead of assumed. It was a flat 28
  columns at every terminal size, so at 60 columns twenty blank cells sat
  between `/advisor` and a description cut down to 21. Each group now sizes its
  column to the labels it actually holds, which nearly doubles the description
  column on a narrow terminal, and the label — the string you have to type —
  reads one step brighter than the description that qualifies it.

- `/help` stopped spending rows on itself. The match count moved onto the filter
  row it describes, the blank spacer under it is gone, and the footer no longer
  repeats `type to filter` while the filter box says `Type to filter` two lines
  above — at 60 columns that duplicate was what pushed the footer onto a second
  row. A group header also stopped printing `▸ ▾`: the selection cursor and the
  collapsed chevron are the same glyph, and a focused collapsed group was
  showing it twice for two different facts. Help now opens focused on the first
  entry rather than the header above it.

- The bottom status rail is no longer one run-on sentence. At 120 columns it
  read `▌· idle · Ollama · deepseek-v4-flash · max · Anonymous usage counts are
  on. … ⌥V:output · /context:context · fn+F1:keys` — live state, route
  identity, a telemetry consent notice, and keyboard hints all strung together
  by the same middle dot in the same ink, so nothing was grouped and the eye
  had nothing to skim by. At 80 columns it simply stopped mid-notice, and at 60
  the row overflowed and was clipped by the terminal mid-word. The rail now
  divides its groups with a blank gutter instead of another dot (the dot is
  kept for peers *inside* a group), the model name reads one step brighter than
  the qualifiers that narrow it, and `Esc to interrupt` reads in the same hint
  weight as the right-hand chords rather than in the separator weight.

- Nothing on the status rail is ever truncated now. A notice sheds whole
  sentences to fit, and if one sentence is still too long it sheds at the inner
  joints — a colon, a semicolon — with the trailing mark cut so the phrase that
  survives does not itself advertise that more was coming. Route identity sheds
  the provider, then the reasoning effort, rather than rendering
  `deepseek-v4-flash-prev…`; a clipped model name is worse than no model name
  because routes share prefixes. Clauses rejoin without a Latin space after a
  full-width stop, so the Japanese receipt reads as Japanese.

- A notice now stands the standing facts down instead of queueing behind them.
  Route identity and the ledger chips are still there in ten seconds; the
  notice is not, so it takes the row and the key hints yield last. This is what
  makes the telemetry receipt readable at 80 columns, where it used to be
  simultaneously always present and never legible.

- The status rail no longer advertises `/context:context`. It was spending
  eighteen columns of a 24-row screen to name a slash command that announces
  itself the moment you type `/`; the rail advertises chords you cannot
  discover any other way. The rail now reads the same at 80 columns as at 200.

- The idle screen no longer has an absolute path stretched across it. The
  workspace caption between the wordmark and "What do you want to accomplish?"
  was composed at full length and then truncated to the lane width, which made
  the centering inset `(width - caption.width()) / 2` evaluate to zero — so a
  line that was written to be centered rendered flush-left and full-bleed,
  cutting the centered whale/wordmark/prompt composition in half. The clipping
  also destroyed the information it was supposed to carry: at 80 columns the
  line read `/private/tmp/claude-501/-Volumes-.../34267917-11f4-4d15-911a-…`,
  which tells the reader nothing about where they are. The caption now sheds
  detail instead of being cut — MCP count first, then branch, then leading path
  components — so it always fits with room to center, and the folder you are
  standing in is the last thing to go. Elisions land on a path separator rather
  than mid-directory.

- Removed the placeholder engine tree in `crates/core/src/engine/`. Its
  `Engine::run` accepted `Op::SendMessage`, appended to a journal, and emitted
  `TurnComplete { status: "completed" }` without ever contacting a model, and
  `TurnExecutor` was a struct with a field-copy constructor and a
  `step < max_steps` comparison. Nothing in the workspace referenced any of it —
  the only mention of `codewhale_core::engine` anywhere was a doc comment inside
  the tree itself — but its comments ("the real turn loop is wired here in the
  next slice") were what `docs/ARCHITECTURE.md` leaned on to claim that
  `crates/core` owns the agent loop. There is now exactly one turn loop in the
  workspace, `Engine::run_turn`, and a guard test fails if a second one appears.
  `docs/ARCHITECTURE.md` and `AGENTS.md` now say where it actually lives.

- First-run onboarding no longer silently truncates its explanation in
  languages that do not put spaces between words. `wrap_words` split on
  whitespace, so a Japanese sentence arrived as a single token, the
  line-break check (which only fires once a line is non-empty) never
  triggered, and the over-wide line was clipped by the terminal. At 80
  columns the provider screen read
  "Hosted providers need a key, but loca" and stopped — losing exactly the
  half that tells the reader local runtimes need no key, on the screen where
  they choose a provider. Space-less scripts now break by display width on
  grapheme clusters, and a line may not begin with closing punctuation
  (`。`, `、`, `」`, `）` and friends). Wrapping for languages that do use
  spaces is unchanged.

- Project instructions are bounded by one budget and no longer treat other
  agents' files as law by default. Previously `.claude/instructions.md` and
  `CLAUDE.md` sat at ranks 2 and 3 of the canonical instruction list — *above*
  Codewhale's own `.codewhale/instructions.md` — `.claude/rules/` was an
  auto-discovered rules directory, and `.cursorrules`, `.cursor/rules`,
  `.clinerules`, `.windsurf/rules`, `.gemini`, `.github/copilot-instructions.md`
  and `.github/muse-instructions.md` were all imported into the system prompt
  with no opt-in. Dropping a `CLAUDE.md` written for a different tool into a
  repository silently made it standing authority here, which is an injection
  surface rather than a convenience. Codewhale now reads `AGENTS.md`, the
  cross-agent `.agents/AGENTS.md`, and its own instruction files by default;
  every other agent's format is opt-in by name through
  `project_instruction_imports` (env `CODEWHALE_PROJECT_INSTRUCTION_IMPORTS`),
  imported files rank *below* Codewhale's own, and a workspace that contains an
  un-imported format says so in a warning naming the exact setting.
- Separately, a symlinked candidate rules directory — `.cursor/rules`,
  `.windsurf/rules`, or `.gemini` pointing outside the workspace — was
  traversed and its contents imported as instruction authority, because the
  directory check followed the link while only the files inside it were
  checked. The two instruction loaders now apply the same no-follow rule that
  `.codewhale/rules/` already had.
- The three separate ceilings on standing instructions (200 KiB for the
  root->workspace chain, 500 KiB for the rules block, 40 KiB for imported
  fragments, and a global layer that was merged in after the chain budget had
  already closed and so counted against nothing) are replaced by a single
  48 KiB aggregate budget covering all of them together. Instructions claim it
  before rules, and are trimmed from the broadest scope inward so the
  nearest-scope file is the last thing dropped rather than the first thing
  stranded. Truncation still leaves an explicit marker.

- Editing the workspace no longer grants the shell outbound network access.
  `workspace-write` sandboxes are created network-restricted; `curl`, package
  installs, and `git fetch` inside a sandboxed shell are denied by the OS
  sandbox unless network is granted explicitly. This closes a real gap rather
  than tightening a working boundary: the elevation added in #273 was justified
  by the application-level `NetworkPolicy` remaining "the only outbound
  boundary", but that policy governs `fetch_url`, `web_search`, and MCP HTTP
  and never constrained shell subprocesses, so workspace-write turns had
  unrestricted egress with nothing enforcing anything. Network now comes from
  one of three explicit places: the new `sandbox_network_access` config key
  (also `CODEWHALE_SANDBOX_NETWORK_ACCESS`), a `danger-full-access` posture, or
  the existing post-denial elevation prompt that grants network for a single
  call. Yolo and `--yolo`/Bypass are unchanged — they resolve to
  `danger-full-access`, which applies no sandbox at all. `external-sandbox`
  reports the network it was actually granted instead of hardcoding `true`, and
  `/status` reads the flag instead of printing "network on" for every
  workspace-write policy. Platforms with no sandbox backend (default Linux
  without bubblewrap, and Windows) still enforce nothing, and both `/status`
  and `doctor` continue to say so.

- The nightly Windows ARM64 artifact build works again. Every nightly from
  2026-08-16 failed while compiling `codewhale-tui`, deterministically on the
  same codegen unit across all three build attempts, with
  `thread 'optimize module codewhale_tui...-cgu.13' has overflowed its stack`.
  The trigger is stack depth in the LLVM worker threads that run per-codegen-unit
  optimization, not the workflow's `lto=off` override: holding the crate and
  every flag fixed and varying only `RUST_MIN_STACK` on aarch64 shows 1 MiB
  crashes rustc while 2 MiB and 4 MiB succeed. Unix std defaults to 2 MiB and
  passed; the Windows ARM64 runner sat under the requirement. Nightly now sets
  `RUST_MIN_STACK` explicitly for every target, because the requirement follows
  from the size of `crates/tui` rather than from the platform. The redundant
  `codegen-units` override is gone -- `[profile.release]` already sets 16, so
  restating it never changed anything. Shipped binaries were never affected;
  `release-artifacts.yml` builds `--profile dist` with fat LTO and
  `codegen-units = 1`.

- Test debt: the transcript history-cell suite has been rebuilt. It was 123
  tests across 3,964 lines, and about a third of it pinned the current skin
  rather than any behavior -- `assert_eq!(spans[1], "⣤")` for the
  reduced-motion marker, `title_span.style.fg == theme.tool_title_color`,
  `visible[1] == "▏ done: scan repo"`, four separate tests each asserting one
  shape of fenced code never takes the transcript rail, and one test whose
  only assertion was `!text.is_empty()` under a name promising it checked the
  rendered tool id. Assertions like those break on every legitimate visual
  change and catch nothing a reader of the transcript would notice, which is
  the liability `d64b9429b` named. The replacement is 40 tests, each named
  for the property it protects and asserting the property instead of the
  token: reduced motion is checked by rendering the same running card at two
  different elapsed times and requiring the frames to match -- which also
  catches an animation leak the glyph constant missed -- and a frozen marker
  must stay visible rather than landing on the spinner's invisible blank
  (U+2800). Severity colors are checked by requiring warning not to read as
  error rather than by naming a palette entry. A streaming assistant glyph
  must actually pulse when motion is allowed, checked against
  `pulse_brightness` rather than by sleeping on the 2s sine. Each of the
  invariants claimed was verified to fail the new suite when deliberately
  broken in the renderer.
## [0.9.10] - 2026-08-19

- Show the full slash-command or `/model` completion row in a bounded, wrapping hover popover whenever narrow terminals truncate it, closing the remaining scoped gap from [#998](https://github.com/codewhale-hq/CodeWhale/issues/998). Thanks [@AiurArtanis](https://github.com/AiurArtanis) and [@formp3](https://github.com/formp3) for identifying the affected surfaces.
- `registry_sync` no longer ships the full MCP Registry catalog into the
  conversation. It takes a required query, scores the local snapshot
  host-side, and returns at most eight matches; the complete catalog stays on
  disk, and the compaction and spillover exemptions that let a multi-hundred-
  kilobyte dump reach the model unchanged are gone.
- Providers that mirror the outgoing `(reasoning omitted)` replay placeholder
  back as a reasoning delta no longer have that echo ingested as real
  thinking: the exact transport placeholder is dropped on arrival, so live
  sessions stop showing a stream of placeholder reasoning blocks and saved
  transcripts stay free of them.
- Mid-stream connection drops during an interactive turn no longer persist a
  synthetic `[runtime]` user message. Retry state is a typed engine-internal
  descriptor; a thinking-only drop re-issues the request without claiming a
  partial reply was preserved, the retry budget is enforced in mechanism, and
  recovery produces exactly one authoritative final answer.

Codewhale v0.9.10 is a retention, identity, and product-clarity release: the shell and
transcript can no longer retain unbounded tool output in memory or on disk,
mid-turn history inserts no longer strand in-flight tool rows, every agent
that ran this session is visible from `/agents list`, the PTY acceptance
lane has a stall watchdog and bounded CI steps, approval outcomes are
durable and fail closed (cyq1017, #5360), and three $HOME disk leaks are
reclaimed. Extension surfaces now name their real state and act only
through the reviewed install and trust flows, and sub-agent, shell, task,
and workflow state is owned by the session that created it (#5518). Test
threads get an 8 MiB stack so the lib suite can no longer
abort under load.

### Fixed

- First run starts on the welcome screen again. A missing key no longer
  skips Welcome and auto-opens the local-provider list: Enter walks to the
  calm provider explanation, then Enter opens the picker so a first API key
  can be set. Returning missing-key recovery still opens the picker on
  launch.
- A foreground `bash` command that named no timeout is bounded again. The
  model-facing `bash` tool left an omitted `timeout_ms` at the internal
  ceiling (~24.8 days) instead of the 120 s default its own schema
  advertises, so a CLI that blocked on an interactive prompt or a hung
  network call held the turn open indefinitely — one report sat on a single
  unauthenticated CLI call for over two hours with the tool row simply
  counting seconds. The advertised default now applies, which arms the
  existing recovery: the process is killed and the model is told to rerun
  with `background=true` and poll with `action="wait"`. An explicit
  `timeout_ms` is still honored for genuinely long foreground work, and
  background and interactive runs keep their own lifetimes.
- The Extensions (`/plugin`) Marketplace is no longer a read-only list.
  Every recommendation and stored candidate names its truthful state and
  primary action — Add, Enable, Configured, or Unavailable — and Enter or a
  mouse click runs that action through the existing reviewed `/mcp add
  recommended`, `/mcp enable`, and `/plugin` install/trust controllers, so
  no second trust path exists. Browser Use and Sandbox Runtime stay
  honestly Unavailable with their real setup routing instead of implying an
  install Codewhale cannot perform.
- The Extensions MCP tab now renders one honest inventory: the header count
  and the visible rows derive from the same configured-server set, so
  `MCP (6)` can no longer sit above two rendered rows. Disabled servers
  stay visible and labeled disabled instead of silently disappearing, and
  configured servers absent from the live snapshot list as not-yet-inspected
  with their own explicit reload affordance through the MCP command
  controller.
- Installed plugin rows now act on their real state: Enter opens an active
  bundle (`/plugin show`), offers Enable for a trusted-but-disabled bundle,
  or routes an untrusted bundle to the existing trust review — through the
  same confirmation and persistence controllers as the slash commands.
- Sub-agent handoffs and rosters, background shell jobs, durable tasks,
  delayed continuations, and workflow controls are now scoped to the
  session that owns them. Records carry immutable root-session ownership,
  stale completion-channel payloads are rejected before deduplication,
  background work drains and reports only to its owning session, and
  legacy ownerless jobs fail closed (#5518 failure class reported by
  @hxfhd; the report's exact JavaScript provenance was not claimed as
  reproduced).
- The resolved route envelope now reaches every outbound model call: all
  wire dialects and auxiliary calls clamp at the shared transport seam
  under one wire/reservation budget, provider input limits are honored, and
  switching models on the same protocol no longer inherits the previous
  model's limits (#5516, #5518).
- First-run continuity: the chosen onboarding provider persists across
  restarts, a missing first-run config no longer interrupts the flow, and
  the automatic working-agreement checkpoint renders as a standalone setup
  handoff instead of regressing the onboarding rail to the full wizard's
  4/10 progress.
- Explicitly worded natural-language `/goal` declarations now create a
  durable goal in the provider-neutral engine before model dispatch, while
  ordinary tasks and quoted transcripts stay out of goal mode and prose
  acknowledgement alone can no longer stand in for goal creation.
- `/model` now keeps the current Z.ai default (`GLM-5.3`) visible when an
  older installation has an explicit `GLM-5.2` route saved. The saved 5.2
  route remains exact; choosing 5.3 sends and remembers the distinct 5.3 ID.
- The constitution checkpoint now leads with the bundled balanced agreement;
  the default path writes no custom constitution. The startup launch surface
  distinguishes read-only Chat from folder-bound, approval-gated Work.
- Tabby and other IME bridges no longer observe a stale visible caret while a
  frame diff is being painted; Codewhale hides the cursor during the diff,
  restores the canonical composer cell, and only then reveals it again
  (BrathonBai, #5023).
- Pre-header HTTP/2/SSE transport failures get one bounded HTTP/1.1 retry;
  authentication, provider-semantic, response-body, and already-pinned HTTP/1
  failures remain fail-closed (demian-welt, #4683).
- MCP `tools/call` images now travel as bounded typed content instead of
  leaking base64 through model-visible JSON. Direct and parallel calls share
  the same MIME, size, validation, and one-image boundary (PR #5515 by
  @cacdcaecawae).
- Linux npm installs and updates race GitHub Releases against the CNB mirror
  at the checksum-manifest layer, then download from the first verified source
  without making users wait through a doomed slow-source timeout.
- `CODEWHALE_PREFER_BWRAP` now applies the documented Linux sandbox override,
  and Codewhale-era names own build metadata, hook session/tool-call IDs, and
  sandbox child markers. The corresponding `DEEPSEEK_*` names remain as 0.9.x
  compatibility aliases (#5443).
- Windows default launch prefers Windows Terminal: zip archives ship
  `codewhale.bat` (CRLF, `wt.exe` then the exe), `install.bat` copies that
  launcher, and the NSIS Start Menu shortcut opens it instead of the raw
  binary (#1854).
- `fix(tui): make the header status mark honour its setting` — `status_indicator`
  did nothing for three of its four documented values. The header hardcoded a
  leading `cw` span *and* asked for a second mark beside the effort chip, then
  filtered the second one against the literal `"cw"`; because `cw`, the legacy
  `whale` opt-in, and every unknown value all normalize onto that same mark,
  the filter discarded them and left `off` with nothing to turn off. `cw`,
  `whale`, `off`, and a typo all rendered byte-identical headers. There is one
  mark now and the setting owns it: `cw`/`whale` draw the typographic mark,
  `dots` draws the activity frames, `off` removes it (thejayjetson, #5512).
- `fix(tui): rebase active-cell tool bindings on every mid-turn history
  insert` — `/rename` mid-turn left the running tool row spinning forever
  (#5478).
- `fix(tui): bound what the shell and transcript retain from tool output` —
  a 1.1 MB Bash call kept over a megabyte resident for up to an hour; raw
  streams now cap at 16 MiB in flight and release delivered output, and
  retained tool outputs cap at 64 KiB per record / 8 MiB per transcript
  (#5472).
- `fix(tui): reclaim the three $HOME disk leaks` — orphaned session dirs,
  the unbounded audit.log, and stranded writer temp files.
- `fix(tui): persist approval outcomes before execution` (cyq1017) —
  approval receipts are committed to a session-owned log before execution
  proceeds; unpersistable evidence blocks the tool; resume reconstructs
  closed and interrupted approvals (#5360).
- `test(tui): break the LazyLock/env-barrier deadlock in the test harness`.
- `ci: give test threads the 8 MiB stack they need` — the lib suite aborted
  with SIGABRT under load on the default 2 MiB stack.
- YOLO entry points honor a locked approval policy: `--yolo`, `/mode yolo`,
  `/zidong`, and Alt+Y can no longer set Full Access + trust + shell when
  config or managed requirements own the posture.
- Terminal PTY tools fail closed without a sandbox backend under a narrowed
  filesystem posture, and otherwise start `$SHELL -i` through
  `SandboxManager::prepare` like `bash`.
- Skill update refuses to replace a directory that has no `.installed-from`
  marker — same ownership gate plugins already used.
- `cp`/`mv` are workspace-safe only when every path operand stays inside
  the workspace — Auto-Review no longer auto-allows `cp /etc/passwd .`.
- Main CI no longer shares one concurrency group per branch: each SHA
  gets a verdict instead of a cancelled pending run. A hermetic Safety
  gate job runs authorization tests in under 15 minutes (test bankruptcy
  restructuring — no tests deleted).
- Config-fixture tests no longer honor `lock_test_env` as a license to
  read a populated `~/.codewhale/config.toml`; they need an `EnvVarGuard`
  like settings already did. Safety-gate and CNB workspace tests pin a
  hermetic `CODEWHALE_HOME`. `exec_persistent_service` is serialized in
  nextest and inside the cargo-test binary instead of dropped (#5355).
- Short CLI no longer waits up to three seconds for a telemetry POST on
  exit; `session_end` is recorded and the buffer ships on the next
  interactive session.
- Bare `/` now opens a deliberately small starter set: `/help`, `/setup`,
  `/model`, `/settings`, `/resume`, and `/rc`. The full command inventory
  remains searchable through `/help`, the command palette, and direct prefix
  typing, without front-loading the entire control surface (#5442, #5439).
- First run now asks only for decisions needed to become usable, keeps the
  offline route explicit, and leaves optional provider, tools, policy, and
  appearance work in the progressive `/setup` repair guide. The idle aquarium
  asks one task-oriented question instead of advertising a tutorial or command
  billboard, and the localized telemetry choice appears after the workspace is
  ready without blocking the composer.
- Provider, active model, and reasoning effort moved out of the crowded header
  into a quiet, width-aware footer identity. Compact layouts keep model and
  effort before provider detail and shed whole low-priority groups instead of
  clipping the composer or control hints.
- The model picker no longer re-parses `~/.codewhale/config.toml` once per
  provider when deciding who has a saved key.

### Added

- `/workflows` opens a live run dashboard over this workspace's durable
  workflow journal: every retained run with status, phases, child roster,
  progress, and host-side cancel — observation only, it never launches a run.
- Repository instructions now assemble from the actual containing checkout:
  applicable `AGENTS.md` files resolve repository-root to current-directory in
  order under one aggregate budget, a linked worktree is its own root, and
  scope is never inferred from path mentions or whichever branch is named
  `main`.
- `/goal` is a codex-style control plane: setting an objective dispatches it
  to the engine, which owns the goal and starts the first goal turn itself —
  the objective is never echoed back as the user's own message, pause/resume
  are real control ops, and the hunt-era vocabulary and trophy cards are
  gone.
- `/extensions` and `/plugins` open one localized inventory for Hooks,
  Plugins, local Marketplace catalogs, Skills, and MCP. Reviewed suggestions
  include Playwright, Chrome DevTools, Cua Computer Use, Browser Use, and the
  sandbox runtime without granting trust or installing anything on open.
- Turn Inspector now opens the newest turn and pages across complete recorded
  user, reasoning, tool/subagent, and assistant output with page-scoped search,
  copy, and export (sky-sun-moon, #1682).
- Background tasks have bounded incremental persistence, durable terminal
  reasons, truthful timeout/cancellation receipts, restart recovery, and
  interruptible continuous-goal delays (#5497, #5508).
- `/title` once again controls the terminal window title independently from
  `/rename`, survives session save/load, and sanitizes control, bidi, and
  zero-width characters (PR #5509 by @SparkofSpike).
- MCP snapshots preserve whether capabilities were advertised by the server,
  discovered through the bounded legacy fallback, or not observed (#4170).
- `codewhale auth status --diagnostic` reports canonical paths, isolation
  source, backend class, and value-free provider-source presence without
  opening credential stores or creating/migrating state (#2369).
- `codewhale doctor --probe-search` performs an explicit credential-free,
  policy-checked transport probe for the selected search provider; ordinary
  doctor and JSON output remain offline (#5442).
- The safe deferred `read_media` tool is available to supported read-only
  roles, and history receipts distinguish localized tool execution outcomes
  without exposing raw payloads (#5102).
- **npm Linux x64 first-party source selection.** The wrapper concurrently
  fetches the GitHub Releases and CNB checksum manifests for the exact
  package version, locks the first source whose HTTP response and manifest
  validate, and downloads binaries only from that source. Explicit
  `CODEWHALE_RELEASE_BASE_URL` / `CODEWHALE_USE_CNB_MIRROR=1` still skip the
  race; other targets stay on GitHub.
- `/workflow <objective>` and `/workflow run <path>` now produce a bounded,
  tool-less proposal for review. Only `/workflow confirm` can launch that
  reviewed draft; status, cancel, settings, and the `/workflows` dashboard stay
  host-owned and do not spend a model turn (#5439).
- `feat(tui): add cancellable cadence to continuous goals` (M-Maciej) —
  `[goal] continuation_delay_seconds` gives coordinator goals a visible quiet
  period between successful turns while reusing the existing coalesced goal
  continuation token; Esc, Ctrl+C, pause/done/blocked/clear cancel before the
  next provider request, and failures never continue (#5508).
- `feat(tui): add command context adapters and migration gate (FEAT-015)`
  (aboimpinto) — TUI-owned capability facets, a dual-path dispatch seam, and
  source-aware CI enforcement so a command slice cannot claim to be migrated
  while it still accepts concrete `App`. Zero production commands migrate in
  this slice (#5316).
- `feat(web): move docs/hooks and docs/troubleshooting onto the dictionary
  spine` (Lstarsky0) — both pages now read copy from the locale dictionaries
  instead of inline bilingual literals (#5337).
- `feat(web): move docs/constitution and docs/runtime-api onto the dictionary
  spine` (Lstarsky0) — both pages now read copy from the locale dictionaries
  instead of inline bilingual literals; another incremental phase of #5337,
  not completion of the full epic (#5517).
- `docs(i18n): complete Tier 1 of Chinese docs localization` (SparkofSpike) —
  Chinese and Indonesian docs move to `docs/zh_hans/` and `docs/id/`, with
  redirect stubs at the old paths for one release cycle (#5482).
- `feat(tui): show repository context in git chrome` (wuisabel-gif) — the TUI
  header now identifies the active repository or linked worktree before the
  branch and dirty marker, so operators can see where the agent is working
  without opening the worktree manager (#5437).
- `feat(tui): formalize the status-bar color grammar` — seven semantic
  families live in `docs/design/STATUS_BAR_COLOR_GRAMMAR.md` and
  `palette::grammar`; header and phase-strip ink resolve through named
  `ChromeInk` slots so chrome cannot invent an eighth meaning or spend
  Failure red on a dirty worktree or selected mode. The footer's session
  metrics strip resolves through the same inks, and the red reservation is
  checked against every selectable theme, not just the whale default;
  typed footer toasts resolve through the grammar too.
  Repo/worktree chrome stays metadata and still renders when Git names a
  location but not a ref (#5437).
- `feat(tui): first slice of the agents roster` — every agent that ran this
  session, receipts-only, via `/agents list` and the sidebar fan-out rows
  (#5479 spec items 1 and 5).
- `ci: bound PTY acceptance and add a stall watchdog to the harness` —
  QA_PTY_STALL_TIMEOUT_SECS aborts a wedged PTY run with a diagnostic;
  the isolated PTY step is capped at 15 minutes; Windows NSIS provisioning
  gets a bounded retry (#5496).
- `chore(scripts): dev-cache warns before it fills a disk` (#5465 class).

### Contributors

- [Sh1Zuku](https://github.com/SparkofSpike) (`@SparkofSpike`) — restored
  `/title` as an independent, persistent terminal-window title in PR #5509,
  in addition to the Tier 1 Chinese and Indonesian documentation work below.
- [Sun Zhenyuan](https://github.com/bistack) (`@bistack`) — extracted the
  turn-loop stream processor in PR #5514 while preserving retry, cancellation,
  usage, TTFT, steering, and partial-response behavior.
- [OctoBored](https://github.com/OctoBored) (`@OctoBored`) — supplied the
  working no-token Star History mirror used across the localized README set
  after the canonical chart endpoint began returning a restricted placeholder
  (#5510).
- [cacdcaecawae](https://github.com/cacdcaecawae) (`@cacdcaecawae`) — added
  provider-neutral typed MCP image forwarding in PR #5515; the harvested
  version also makes malformed image fields produce a visible omission receipt.
- [DingYong4223](https://github.com/DingYong4223) (`@DingYong4223`) — reported
  the narrow-terminal completion truncation closed by the bounded hover reveal
  (#998). Thanks also to @AiurArtanis and @formp3 for identifying the affected
  completion surfaces.
- [sky-sun-moon](https://github.com/sky-sun-moon) (`@sky-sun-moon`) — reported
  the missing full per-turn input, reasoning, tool, and assistant pages that
  shaped Turn Inspector navigation (#1682).
- [cy2311](https://github.com/cy2311) (`@cy2311`) — reported the Windows launch
  path that now ships and installs a Windows Terminal-aware batch launcher
  (#1854).
- [demian-welt](https://github.com/demian-welt) (`@demian-welt`) — provided the
  reproducible pre-header SSE transport failure behind the bounded HTTP/1.1
  retry (#4683).
- [BrathonBai](https://github.com/BrathonBai) (`@BrathonBai`) — reported the
  Tabby/CJK IME candidate-window jump that led to the hide-diff-position-show
  cursor transaction (#5023).
- M-Maciej (@M-Maciej) — the real-world organization-coordinator use case and
  5–30 minute cadence requirement behind cancellable cross-turn goal delays
  (#5508).
- cyq1017 (@cyq1017) — approval outcomes are persisted before execution can
  proceed: receipts commit to a session-owned log first, unpersistable
  evidence blocks the tool, stale decisions are rejected, and resume
  reconstructs closed and interrupted approvals (#5491, closes #5360).
- aboimpinto (@aboimpinto) — the TUI-owned dependency-injection and migration
  infrastructure that makes slash-command extraction safe: seven capability
  facets, a dual-path dispatch seam, and source-aware CI enforcement so a
  command slice cannot claim migration while it still accepts concrete `App`
  (#5506, EPIC-005/FEAT-015 under #5316, which they also filed).
- wuisabel-gif (@wuisabel-gif) — the TUI header now names the active
  repository or linked worktree before the branch and dirty marker, derived
  from Git's common directory and capped by shell density tier (#5511, the
  repo/worktree slice of #5437).
- SparkofSpike (@SparkofSpike) — Tier 1 of the Chinese docs localization:
  Chinese and Indonesian documentation moves to `docs/zh_hans/` and
  `docs/id/` with redirect stubs held at the old paths for one release cycle
  (#5507, epic #5482, which they also filed).
- Lstarsky0 (@Lstarsky0) — `docs/hooks` and `docs/troubleshooting` move onto
  the dictionary spine, retiring their inline bilingual literals in favor of
  locale dictionaries with token-aware code spans (#5504, closes #5337, which
  they also filed).
- Lstarsky0 (@Lstarsky0) — `docs/constitution` and `docs/runtime-api` follow
  the same dictionary spine: 28 inline `isZh` branches become typed English
  and Chinese dictionaries held to key and token parity, while the other
  sixteen locales keep the English fallback (#5517, a further phase of #5337).
- @thejayjetson — the header status-indicator report that pinned the
  regression to a specific setting, with every value, theme, and
  `fancy_animations` combination already ruled out (#5512).
- hxfhd (@hxfhd) — reported the deterministic cross-session contamination
  class behind this release's session-ownership boundary, plus route
  budgeting evidence (#5518).
- sfdzhmr (@sfdzhmr) — reported the route budgeting root causes that exact
  route-limit propagation now closes (#5516).

## [0.9.9] - 2026-08-18

Codewhale v0.9.9 is a truth-and-resilience release: the shell tool can no
longer wedge a session when the host runs out of disk or descriptors,
unverified context windows and output ceilings are labeled honestly at every
surface, DeepSeek V4 is priced on the published peak/off-peak tiers, SSE
UTF-8 fails closed in every dialect, Fleet shadowing is visible, bwrap gets
container essentials and extra roots, the `dsh` skin rides the bundle
profile, the `agent` tool schema is down to 12 fields, and README/website
locales grow to 18 and 8.

### Fixed

- The lowercase `bash` tool no longer wedges when its complete-output spill
  file cannot be created: a full temp volume or exhausted descriptor table
  used to fail *every* call — `echo ok` included — with the harness-internal
  "Failed to create streaming shell output" and never recover until the
  host was cleaned up. The spill is now best-effort (the bounded tail is
  still returned and the truncation notice says why the full-output path is
  missing), and any remaining spawn/stream failure names the exhausted
  resource — disk, file descriptors, memory — and says the next call is safe
  to retry (#5465; the wedge that took out the owner's own 0.9.9 session).
- A concrete route/offering output limit now outranks the conservative
  8,192-token compatibility guess for an uncatalogued model. Routes that
  publish no output limit remain fail-closed, documented model ceilings stay
  authoritative, and a route limit can never raise the requested cap (#5460).
- Context-window honesty at every surface (#5239, #5441): the
  `model-name hint` and `fallback` rungs of the context-window ladder are
  guesses, and every surface that renders one now says so — the status line,
  `/status`, `/config`, the context-pressure message, the model picker chips,
  and the auto-router inventory. Unverified windows still drive real budgets
  (compaction trigger, context meter, output reservation); they just stop
  reading as capabilities anyone checked. A window parsed from an `_Nk`
  model-name suffix (`qwen3-32b-256k` → 256K) is now its own
  `model-name hint` rung below `catalog`, because it is optimistic rather
  than conservative — a catalog or provider-reported value beats it. The
  `[providers.<name>] context_window` override remains the hard fix and
  renders as `configured` with no marker.
- Output-ceiling honesty (#5440): an Anthropic-family model the catalog does
  not describe keeps the 64K Messages floor as its clamp and the ChatGPT/
  Codex OAuth route keeps its 4K policy, but `OutputCeilingSource` gained an
  `unverified` rung for both, so exec-stream receipts and the model picker
  label them `unverified`/"assumed floor" instead of `documented`. Clamp
  values are unchanged.
- Telemetry default-on is visible (#5441): `codewhale doctor`'s
  runtime-posture section gained a `telemetry=on (default)`-style row with
  the source that decided it (cli | env | config | default), and
  `codewhale config get telemetry` reports the resolved consent with its
  source instead of `key not found` on a machine whose batches ship. Truth
  change only; resolution and behavior are untouched.
- Fleet: a scout's read-only shell carve-out (#5428) is now honored by both
  the posture gate and the execution envelope, so `git log`, `find | head`,
  `npm view` and the other bounded read-only commands run in-place instead
  of being refused as "Executes" (#5426). Delegation still never widens
  authority: the role-isolation test and docs/SUBAGENTS.md pin that a child
  cannot exceed its parent's posture (#5426, #5435).
- `/rename` and `/title` now apply mid-first-turn: the session file does
  not exist until the first autosave, so the rename fell through with
  NotFound; the shared path now prefers the per-session checkpoint and
  rebuilds from App state, with a PTY regression test through the live
  event loop (#5430).
- `integrations dsh plan` no longer refuses DeepSeek's default
  Responses-dialect route (`deepseek-v4-flash`); Responses and
  Anthropic-Messages routes are carried through pi-ai
  `openai-responses` / `anthropic-messages` instead of being approximated
  or refused; only credentialed base URLs are still refused, with an error
  that names provider and model (#5434).
- Session cost no longer sits at `unverified_live_pricing` when live pricing
  cannot be verified (control-plane 503, Models.dev capabilities-only
  overlays): provider-docs bundled fallback rates for the DeepSeek V4
  family on Fireworks / OpenCode Zen restore a usable figure, live
  per-provider rows still win, and `kimi-k3` stays unpriced until a
  published rate exists (#5241; harvested from #5402).
- Release assets: `release.yml` asset-freshness checks compare against the
  release job's own `started_at`, so job-level reruns of the npm step are no
  longer poisoned by earlier uploads (#5429).
- macOS CI: the `agent_focus_pty` auto-review receipt test waited on a
  worker that had already completed and raced the rail's focus; it now holds
  the child's wrap-up and waits for a settled live row (refs #5056, #5403).
- DeepSeek V4 pricing follows the published peak/off-peak tiers (peak
  01:00–04:00 and 06:00–10:00 UTC; off-peak is half of peak) for
  `deepseek-v4-flash` and `deepseek-v4-pro` in USD and CNY, resolved from
  each turn's recorded time; the stale single-tier rows understated cost up
  to ~4×. Because every direct DeepSeek first-party rate is now
  time-windowed, the scorecard fails closed (`missing_recorded_time`) on an
  undated DeepSeek turn instead of guessing a tier (#5470; #5241 follow-up,
  verified against api-docs.deepseek.com on 2026-08-17).
- SSE UTF-8 split across HTTP/2 DATA frames now fails closed in every
  streaming dialect: a shared strict decoder, tail flush, and
  `decode_failed` propagation (`InvalidSseUtf8`) replace the per-dialect
  approximations, with byte-chunk decoder tests (#5374; supersedes draft
  #5404).
- CI: `release_four_read_only_fleet_roles_launch_with_canonical_prompts`
  answered Fleet children with SSE while they call the blocking JSON path;
  the parse failure was retried and double-counted the worker on slow macOS
  runners (#5471; refs #5056).
- Context: every web tool surface (`Web`, `web_search`, `web.run`,
  `fetch_url`) now uses the noisy-result soft limit, so large fetches are
  compacted like shell output instead of consuming the ordinary hard limit
  (#5474, thanks @h3c-hexin).
- Routing: a lowercase saved selector such as `glm-5.2` resolves against the
  owning Z.ai / DeepSeek catalog row (case-fold fallback, only when exactly
  one provider-owned wire id matches) instead of being classified as another
  provider's bare model (#5475, thanks @h3c-hexin; diagnosis by @asto18089
  in Pinvou/CodeWhale#14).
- Model catalog brought current as of 2026-08-17 against the official
  pricing pages: gpt-5.6-terra / gpt-5.6-luna rates, `claude-sonnet-5` keeps
  $2/$10 (the announced September increase was withdrawn), `claude-opus-5`
  added, `kimi-k3` and `kimi-k2.7-code-highspeed`, `MiniMax-M2.7-highspeed`,
  Mistral first-party rows, xAI `grok-4.5` / `grok-4.3` with long-context
  tiers, Gemini and Qwen limits, and RedNote's `dots3-note` preview as an
  OpenRouter row (no first-party API exists yet) — every number carries its
  source and a pinned test (#5485).
- Website: copy on codewhale.net rewritten in plain declarative sentences —
  one idea per sentence, numbers from the generated facts, no self-narration
  — with a voice sheet at docs/design/WEB_VOICE.md (#5483).
- CI: the release workflows no longer restore npm/cargo caches after
  checking out a caller-supplied SHA — the CodeQL cache-poisoning Highs
  #88–#107 are closed with a contract test over the workflow files (#5463).
- Compact TUI rows below 60 columns no longer reserve a hidden session-metrics
  strip, so narrow terminals reclaim the row instead of clipping the
  transcript (#5486).
- Ghostty's truecolor underwater field now uses a dedicated synchronized
  60 FPS lane instead of the legacy 30 FPS compatibility cap, with continuous
  caustic fades replacing visibly stepped color changes.
- Live reasoning's advertised `Space:expand` action now runs before the
  composer's first-character paste-burst hold, while spaces in an active paste
  remain payload. The newest reasoning preview also spends only genuinely free
  viewport rows before truncating instead of stopping at the fixed 10/12-row
  fallback on roomy terminals.
- Strict `cargo doc` builds no longer fail on bare URLs in rustdoc comments;
  the remaining links are explicit Markdown targets (#5489).

### Changed


- The model-facing `agent` tool advertises exactly 12 fields — `action`,
  `prompt`, `type`, `profile`, `name`, `agent_id`, `message`, `until`,
  `detached`, `worktree`, `write_roots`, `resume_from` — down from 33
  (#5324, refs #5123). Budgets (`max_steps`, `wall_time_secs`, `max_depth`),
  routing overrides (`model`, `model_strength`, `thinking`), worktree-path
  knobs, the deliberate/spawn-contract fields and the wait/status/interrupt
  extras moved off the advertised schema. Every removed field stays
  parse-accepted and honored unchanged (same contract as `token_budget`), so
  saved transcripts, ACP/MCP clients and Fleet configs replay as-is; the
  #5426/#5435 containment clamps are untouched. Child budgets now resolve
  from role defaults (60/120 turns, 1800 s wall time, unchanged clamps) and
  new `[subagents]` keys `default_max_steps` / `default_wall_time_secs`.
  Because the tool catalog is part of the session-pinned prompt prefix
  (docs/CACHE.md), upgrading re-fills the KV prefix once per session.
- TUI prose — user messages, assistant answers, and reasoning/thinking —
  now wraps at the full content width on wide terminals, matching
  tool/status cells, instead of stopping at a 105-column rail that left a
  dead right margin on ultrawide displays (#5436).
- Configured skill prompts are stable across session roots and operating
  systems: only custom configured roots hide their physical path, ordinary
  workspace/global skills keep a discoverable privacy-safe path, warning
  replacements are boundary-aware (including non-UTF-8 Unix paths), and
  Windows separators render as `/`. The skills prompt is also 50 bytes
  leaner without raising a runtime-contract ceiling (#5492, #5473).
- Auto-router classifier requests accept `[auto.router] timeout_secs`, while
  preserving the existing default when the key is absent (#5494).
- Every `ci.yml` job now has an explicit 10–90 minute timeout appropriate to
  its workload, bounding stale assigned runners instead of inheriting
  GitHub's six-hour default (#5495).
- The docs shell and shared web components now route localized copy through
  the typed dictionary spine; these are two incremental phases of #5337,
  not completion of the full epic (#5488, #5490).
- Dependency: rusqlite 0.40.2 (#5391).
- Documentation: stale A/B/C-tier references, provider defaults, module
  descriptions, and line anchors now match the current code (#5481).

### Added

- `[transcript] prose_measure` (positive integer, optional): caps prose
  wrap at N columns for owners who want a bounded reading measure on
  ultrawide terminals. `0` or absent keeps the full width; negative or
  non-integer values are rejected with a clear config error. Tool, diff,
  and status cells never inherit the cap (#5436).
- Localization: README translations for Français, Deutsch, 繁體中文, हिन्दी,
  Türkçe, Italiano, Polski, العربية and Català join the existing nine
  (#5451); codewhale.net routes fr, de, ca, hi, tr, it, pl and ar (with
  `dir="rtl"` plumbing) as partial locales (#5453).
- Docs: README Integrations section (incl. the DeepSeek Harness `dsh` plugin
  path, docs/INTEGRATIONS_DSH.md) localized across all READMEs; RFC keeping
  the deterministic-first auto-review hybrid (#5427); Claude Code parity
  reference for agents/workflows/plugins/skills
  (docs/design/CLAUDE_CODE_PARITY.md); config.example.toml / SUBAGENTS.md /
  TOOL_LIFECYCLE.md brought back in line with the code (#5447).
- `dsh` integration: the Codewhale palette is applied through the bundle
  profile via dsh's documented `overrideTokens` (on by default;
  `codewhale integrations dsh update --skin false` turns it off), replacing
  the 0.9.8 exported-CSS skin that dsh's inline body variables overrode
  (docs/design/DSH_BUNDLE_SKIN.md, docs/INTEGRATIONS_DSH.md) (#5469).
- `dsh` integration: an ambient ocean scene behind the DSH web UI — slow
  whale silhouettes, a school of `><>` glyph fish, bubbles — drawn on a
  canvas under a translucent veil of the Codewhale palette, plus an explicit
  responsive `WHALE BROTHERS / CODEWHALE × DEEPSEEK HARNESS` lockup; light
  and dark, ~30 fps capped, paused when hidden, a static frame under
  `prefers-reduced-motion`; on by default with the skin,
  `codewhale integrations dsh update --ocean false` turns it off (#5484).
- Fleet: agent shadowing is visible — a roster-row badge, a Layers block in
  agent detail, and a `doctor` "Fleet roster layers" section (JSON
  `operate_fleet.roster.multi_layer`), in all 15 TUI locales. Layer collapse
  and `[fleet.profiles]` migration stay for 0.9.10 (#5098).
- Sandbox: bwrap containers get the `--dev/--proc/--tmpfs` essentials plus
  configurable extra roots (`bwrap_ro_roots` / `bwrap_dev_roots`) so
  toolchains that live outside the workspace stay reachable read-only
  (#5410).
- Tests: `crates/tui/tests/README.md` states the keyless assembled-journey
  rule and maps the Auto-Review guardian acceptance items to the engine
  journeys that exercise them (#5361).
- OrcaRouter's default endpoint is classified as an aggregator billing
  surface, so pricing and session-cost reporting use the correct billing
  posture instead of treating it as a first-party provider (#5493).
- Dependencies: ratatui 0.30.2, thiserror 2.0.20.

### Removed

- `dsh` integration: the exported-CSS skin file and its "skin export" status
  line (superseded by the bundle-applied `overrideTokens` skin, #5469).

### Contributors

- Lstarsky0 (@Lstarsky0) — compact rows no longer reserve a hidden
  session-metrics strip (#5486); docs shell and shared component pick-text
  moved onto the typed dictionary spine (#5488, #5490).
- Sh1Zuku (@SparkofSpike) — topbar session title (#5481).
- hexin (@h3c-hexin) — a concrete route/offering output limit outranks the
  8,192-token compatibility guess for an uncatalogued model (#5461, closes
  #5460); web tool results use the noisy soft limit (#5474); owned direct
  model casing resolves safely (#5475); and configured-skill prompts stay
  stable across ephemeral roots and operating systems (#5492, #5473).
- Gabriel-Degret (@Gabriel-Degret) — configurable auto-router classifier
  timeout (#5494; first contribution).
- @asto18089 — diagnosed the Z.ai `glm-5.2` casing collision and wrote the
  first provider-scoped fix in Pinvou/CodeWhale#14 (carried upstream in
  #5475).
- Reports and reproductions that shaped this release: @hardy922 (context-
  window honesty, #5239), @redstar (bwrap extra roots, #5410), @all-lopezg
  (SSE UTF-8 garbling on DeepSeek Flash, #5374), @alitvak69 (unverified live
  pricing, #5241), and @wuisabel-gif (the macOS filtered-suite hang
  investigation on #5056).

## [0.9.8] - 2026-08-16

Codewhale v0.9.8 ships the remaining assigned finish. Remaining web
settings polish moves to v0.9.9. Prefab third-party templates that have
a published OpenAI-compatible host ship here (#5350).

### Fixed

- `sudo` (and `su`/setuid helpers) work again for wheel-group administrators
  who want Codewhale to be able to escalate: the Linux startup hardening's
  irreversible `PR_SET_NO_NEW_PRIVS` flag — inherited by every child process —
  is now skippable with `CODEWHALE_NO_NEW_PRIVS=0` (#5413). The flag stays on
  by default; the no-ptrace and no-core-dump measures are never skipped.

- Abort-class process deaths no longer poison the terminal (#5424). A
  stack overflow, allocation failure, or double panic skips the panic hook
  and every cleanup guard, which is how a v0.9.7 user's mid-turn exit left
  mouse capture leaking SGR sequences into their shell. An
  async-signal-safe handler now restores the terminal modes and appends a
  one-line cause marker to `~/.codewhale/crashes/last-fatal-signal.log`
  before re-raising, keeping the honest 128+signal wait status. A SIGKILL
  (OOM killer) remains uninterceptable by design.

### Changed

- Prompt-cache prefix is pinned for the session. The tool loop no longer
  recomposes the system prompt from disk on every model step, so an agent
  writing a file no longer busts the provider KV prefix cache mid-turn. The
  system prompt and tool catalog are re-composed only on a declared header
  change (`/model`, mode, goal, session resume), which re-pins under a logged
  reason; an undeclared change is reported as drift and the original pin is
  kept instead of silently becoming the new baseline. Workspace, AGENTS.md,
  skills, memory, and goal drift now reaches the model as one bounded
  `<context_update>` user message at the next user turn — a history append,
  not a header rewrite. `/cache stats` shows the pin reason, the last-miss
  reason, the undeclared-drift count, and the context-update count. See
  [docs/CACHE.md](docs/CACHE.md).

- Plugin compatibility is now per-component. A reviewed, trusted, enabled
  bundle that mixes Skills or MCP with unsupported commands, agents, hooks,
  LSP, native, filesystem-roots, or lifecycle-mutation declarations keeps the
  supported adapters active and reports the rest as inactive (`full` /
  `partial` / `unsupported`). All-unsupported bundles still cannot be enabled.
  The capability hash is now v2 and binds this build's activation policy, so
  older v1 receipts and any later adapter-enablement change fail closed as
  needs-review. Skills and each MCP transport re-request their own capability
  at the consumption boundary.

### Added

- Opt-in multiline composer mode (`composer_multiline_mode = true`) makes
  Enter insert a newline and Shift+Enter send. Alt+Enter, Ctrl+J, and supported
  Ctrl+Enter/Cmd+Enter behavior stays unchanged (#5345, @AiurArtanis).

- `/plugin marketplace add|list|show|remove|install` completes the
  federated marketplace journey (#5311). `add` reads one LOCAL catalog
  document in the real published schemas (Kimi, Claude, Codex, or
  Codewhale native) — no network, regular files only — and persists it
  beside the plugin state with the same hardened, fail-closed store.
  `list`/`show` render every candidate with per-entry diagnostics,
  display-only tiers, and honest install plans that say when Codewhale
  cannot fetch a source; `install` routes through the existing reviewed
  installer, so installed bundles still enter disabled and untrusted.
  Foreign auto-install policy (Codex `INSTALLED_BY_DEFAULT`) is visibly
  ignored; nothing is auto-installed, auto-trusted, or granted vendor
  trust.

- `/rc` attach now includes an observed `owner/name` git remote when the
  folder has a GitHub, CNB, or Gitee origin, so CWC can label the paired
  session. Paths stay off the wire. Reconnect after both this client and
  CWC #202 land to backfill existing empty rows.

- The local Runtime web client keeps the thread rail clipped so **New
  thread** cannot paint over the session fact chips. Chips wrap instead
  of sliding under the rail.

- Z.ai `GLM-5.3` is live on the Coding Plan and is now the default direct
  Z.ai model: `DEFAULT_ZAI_MODEL` resolves to `GLM-5.3` in both
  `codewhale-tui` and `codewhale-config`, and it is the first `/model` row
  after `/provider zai`. Explicit `GLM-5.2` selections (`model = "GLM-5.2"`
  and its `glm-5.2` aliases) keep their own id — only the default moved.
  Limits and reasoning options still inherit from `GLM-5.2` until Z.ai
  publishes distinct 5.3 numbers. No USD price is claimed. A live call
  can still 429 with entitlement code 1311 on accounts that are not
  provisioned for 5.3.

- The TUI transcript renders Markdown blockquotes (`>` lines) with a quote
  rail — nested quotes, inline bold/code/links, wrapped continuation rows, and
  selection copy that keeps the quote text and skips the rail chrome.

- Sub-agent details show the resolved model, fleet role, and type. Labels
  use the session/role name instead of a generic Agent N (#5371, #5287).

- Documented catalogue output ceilings (including DeepSeek V4's 384K maximum)
  remain authoritative bounds, while ordinary requests start at a safe 64K
  cap and explicit overrides can raise it within the resolved route window. A
  clean output-limit stop continues the turn instead of killing it (#5373,
  #5516, #5518). Thanks @sfdzhmr and @hxfhd for the route evidence.

- Ollama Cloud is a first-class hosted provider (`/provider ollama-cloud`)
  on the official OpenAI-compatible `https://ollama.com/v1` route. Local
  Ollama stays keyless. The exact released `ollama` + Cloud URL tuple keeps
  a bounded compatibility path across saved sessions, Fleet, and nested
  subagents; neighboring remotes stay custom and fail closed against
  inherited official credentials.

- Homebrew ships a `codewhale` formula. `brew tap Hmbown/deepseek-tui &&
  brew install codewhale` is the install path; `brew upgrade codewhale`
  updates it. The legacy `deepseek-tui` formula remains a deprecated alias
  for one overlap release.

- `/title [name|off]` sets a per-session tab/window title, shown as
  `[title] …` in front of the terminal window title (`Codewhale` /
  `reasoning…` / `using tool…` / `done`). The `title` config key supplies
  the default (`/config title … --save` persists it); multi-window
  workflows can tell parallel sessions apart at a glance. `/title` is
  independent of `/rename`, which keeps naming the session in the picker
  and composer. Control, bidi, and zero-width format characters are
  stripped from both the saved session name and the window title, so the
  picker, the Runtime API, `codewhale sessions`, and the OSC 0 tab title
  all carry the same escape-free text (#5419, #5430).
- Eden AI is a named OpenAI-compatible Chat Completions provider (`edenai`,
  aliases `eden-ai` / `eden_ai`) with `EDENAI_API_KEY`, global and EU base-URL
  overrides, a live provider-scoped model catalog, and
  `deepseek/deepseek-v4-pro` as the verified default. Generic reasoning fields
  stay omitted because Eden AI routes multiple upstream model families
  (#5422, Kai Nacke).
- Children (sub-agents and Fleet workers) inherit the session's permission
  posture faithfully: Auto-Review's deterministic floor and model guardian
  decide a worker's held calls (fail closed when unavailable, never a
  prompt); under Ask a held call is raised in the parent's approval UI and
  the worker waits visibly; Full Access still fails closed on the safety
  floor. Each prompt-less decision is a one-line note in that worker's
  transcript (focus mode) and an audit-log record.
- Worker role defaults keep what the role does not intend to withhold:
  every built-in role keeps network reads; `planner` may run read-only
  shell probes; `custom` inherits the parent's write/network/shell posture
  and is narrowed only by its explicit tool list or the spawning call.
  Read-only roles (`scout`, `reviewer`, `planner`, `verifier`,
  `consultant`) still never write the workspace. The focused worker's
  header states its effective posture from the runtime snapshot.
- `/workflow status`, `/workflow cancel [run_id]`, `/workflow settings`, and
  `/workflow help` are answered by Codewhale itself from the run journal and
  live run state — no model turn — and `/workflow run <path>` launches a
  checked-in workflow as-is. `/config workflow` and `/config goal` explain
  the effective tables. The workflow tool now honors the session `[workflow]`
  table (`automatic`, `auto_start_read_only`, `require_approval_for_writes`,
  limits) instead of product defaults.
- Goal mode enters as readily as DeepSeek Harness: the agent may create the
  session goal when a direct request describes a verifiable multi-turn end
  state, and Codewhale shows a one-line `Goal set` receipt with how to pause
  or clear it. Bare `/goal` shows plain progress (and how to continue when no
  turn is running), prints usage on an empty session instead of asking the
  model, and `/goal help|status` are reserved words.

- Whale Teams in the terminal: the six Signal Cut whale identities (Scout,
  Patch, Harbor, Echo, Keel, Lantern) appear as species badges on `/fleet`
  roster rows and worker rows, with an identity portrait in the roster detail
  pane and a six-state word (Resting, Thinking, Working, Waiting for you,
  Blocked, Offline) derived only from the child's real runtime status. Colors
  come from the theme tokens, every glyph has an ASCII fallback, and the
  working wake animates only under full motion. See
  `docs/design/WHALE_TEAMS_TUI.md`.
- A session metrics strip on the phase row (`4 turns · 108 steps │ LLM
  11m46s · Tool call 1m52s │ TTFT avg 1.5s · 120 tok/s │ Cache hit 99% │
  Input 9.3M`), on by default as the `session_metrics` footer item
  (`/statusline`, `[tui].status_items`). Every value comes from engine
  receipts — turn starts, per-model-call usage with stream time,
  time-to-first-token and whole-call time, tool start/complete edges, and
  provider-reported cache and input tokens. Cells without evidence are
  omitted, never estimated. `/status` prints the untrimmed line; the phase
  row sheds its lowest-value groups to fit the columns it actually has.
- Auto-Review decisions nobody was prompted for are now visible in the
  transcript as one-line notes: model-guardian allow/deny verdicts with
  their risk tier and stated reason, guardian failures (denied, fail
  closed), deterministic policy blocks, and holds Auto-Review denied
  without pausing. The audit log keeps the full record. `/permissions`
  ends with the active posture, what it decides on its own versus never,
  and the audit-log path. The footer's `Esc to interrupt` hint is
  localized. See `docs/design/AUTO_MODE_PARITY.md` for the Claude Code /
  Kimi Code parity ledger and follow-ups.
- `codewhale integrations dsh status|plan|connect|update|launch|disable|enable|remove`
  connects an existing official DeepSeek Harness (`dsh` 0.1.0-rc.6, verified)
  through Codewhale using only its documented seams: a `--patch` overlay that
  pins the exact Codewhale provider/model/endpoint identity (native
  `deepseek-official` route, or a hand-declared `openai-completions` route
  named `codewhale-<provider>` for OpenAI-compatible providers), the
  Codewhale permission posture exported as `DSH_PERMISSION_MODE`, and an
  append-only receipt. Codewhale writes only under
  `$CODEWHALE_HOME/integrations/dsh/`, never copies API keys or edits DSH
  files, never broadens permissions (`--allow-full-access` only mirrors an
  existing Codewhale full-access posture), and reports not-installed /
  offline / incompatible / detected / connected / stale-config /
  stale-version / disabled honestly. Anthropic Messages and OpenAI Responses
  routes are refused as not carriable. The documented DSH plugin path is an
  explicit opt-in: `install-bundle` materializes a Codewhale bundle package
  (`codewhale-dsh-bundle`, MIT notice retained) and installs it with
  `dsh plugin --profile codewhale add <path>` into a dedicated `codewhale`
  profile (pnpm required, reported truthfully when missing; `web`/`headless`
  untouched), so `dsh --profile codewhale` alone carries the identity;
  `update` regenerates the bundle patch and `remove-bundle` reverses it,
  leaving the DSH-owned profile directory in place. `/setup tools` and `codewhale doctor`
  show the read-only detection state; `doctor` also lists the DSH read-only
  credential consent alongside Codex and Grok. The optional `--skin` export
  writes a Codewhale token stylesheet generated from the TUI palette
  (Blue Stage dark/light, ombre water column, mode/permission/state colors,
  reduced-motion fallbacks); DSH exposes no custom-theme API, so the sheet is
  labeled an unsupported overlay and is never injected. See
  `docs/INTEGRATIONS_DSH.md`.

### Fixed

- Selecting the `google` provider kind resolved to the `antigravity`
  TUI identity (and vice versa): the agy provider entry was inserted at
  different positions in the config-level enum and the TUI's
  discriminant-indexed lookup table. The table now matches the enum, so
  provider pickers, sorted display, and kind round-trips are correct.

- The provider picker's key-entry stage accepted typed input and pastes
  for OAuth-only providers after `antigravity` declared OAuth
  acquisition; those gates now key off the OAuth acquisition class
  instead of a hard-coded provider identity.

- The webhook hook sink no longer panics when its HTTP client fails to
  build; it falls back to a default client (#5381, EvanProgramming).

- Session-index JSONL writes are serialized behind a process-wide mutex
  so concurrent state stores cannot drop an append during compaction
  (#5382, EvanProgramming; complements the cross-process file lock).

- A billed `max_tokens` stop followed by a transport error fails the turn
  instead of continuing into a second request. A clean output-limit stop
  still continues. Mid-size context windows keep the ordinary 65K internal
  reservation so compaction does not collapse to the 1K headroom floor
  when the catalogue documents a matching output ceiling.

- Thinking cycle, `/effort`, and Settings now walk each model's real ladder
  instead of the DeepSeek off/high/max shortcut. Grok 4.6 is
  auto/low/medium/high/xhigh (cannot disable); Grok 4.5 is
  auto/low/medium/high; first-party DeepSeek keeps a documented `low` tier.
  `/effort` persists and receipts through the same path as Ctrl+T.

- Google Gemini is its own backend (`/provider google`) on the official
  OpenAI-compatible route with thought-signature capture/replay and
  fail-closed replay for thinking models. Antigravity (`agy` 1.1.13) is
  a separate provider: consent-gated read-only import of the official
  CLI's login, then a text-only cloud-code stream
  (`/v1internal:streamGenerateContent`). Tools, images, and unknown SSE
  shapes fail closed. Gemini 3.7 Flash is not advertised until a live
  turn succeeds on this wire. The website 44-count still excludes
  Antigravity.

- DeepSeek Flash SSE on macOS no longer turns mid-character HTTP/2
  flushes into U+FFFD replacement characters (#5374). Invalid UTF-8
  fails the line instead of using lossy decode.

- `[workshop] read_result_max_bytes` and `tool_result_max_bytes` raise
  the model-visible read/tool-result floor; they never lower the
  compile-time defaults and cap at 2MiB (#5367).

- Fireworks and OpenCode Zen DeepSeek V4 Flash/Pro keep a bundled
  family rate when the live control plane is down, so session cost is
  not stuck on `unverified_live_pricing` (#5241). `kimi-k3` stays
  unpriced until a published rate exists.

- Provider setup ships a typed beginner template catalog (#5350). OpenCode
  Zen/Go stay first-class key-only rows with their documented hosts and
  curated models. SenseNova fills a named OpenAI-compatible table on the
  published `https://token.sensenova.cn/v1` host (`S` or `/provider setup
  sensenova`). Agnes is listed as unpublished because this repository has
  no published URL. `/provider` `P` and Settings → beginner templates open
  the list; `T` tests `/models` and refreshes status without treating 2xx
  as model-ready. `/model` names a failed Models.dev refresh as
  `refresh failed; catalog available` instead of `cache failed`.

- Privileged release workflows no longer restore rust-cache, sccache,
  or npm caches after checking out a caller-supplied SHA (CodeQL
  cache-poisoning #88–#106). Catalog drift no longer prints raw
  bundled/upstream blobs (#107).

- Cancelling a turn now cancels its foreground child agents with it.

- Empty compaction no longer wipes conversation history.

- Wide terminals and tmux panes fill the full available width again for the
  transcript and composer (#5322). The brief v0.9 session-shell side gutter
  is gone so expanding a pane rematerializes layout the same way shrinking
  does.

- The agent tool schema rejects empty calls.

- The local web client keeps recovered stream gaps closed, user questions
  answerable, manual bootstrap access intact, and streamed prose quiet for
  assistive tech.

- Website zh-Hans copy now says 宪章, matching the TUI pack (#5397,
  Lstarsky0).

- Public website provider facts include Google Gemini and Ollama Cloud
  (44 runnable routes). Antigravity stays credential-plane-only.
  Harvested from #5398 (Lstarsky0) with that correction.

- The website models page carries a truthful read-only settings preview
  built from repository facts; it never implies the site can change local
  configuration (#5370, #5411, mvanhorn).

- The canonical `ultra` reasoning effort now maps to each provider's
  maximum tier alongside the legacy `ultracode` alias, instead of being
  silently dropped (#5303, #5409, buiducnhat).

- Session titles truncate by character count, not byte offset, so
  multi-byte titles (CJK, emoji) cut at the intended width and word
  boundary instead of past the limit (#5415).
- Wide terminals and tmux panes fill the full available width again for the
  transcript and composer (#5322). The brief v0.9 session-shell side gutter is
  gone so expanding a pane rematerializes layout the same way shrinking does.
- The background verifier test drives the current libtest executable
  instead of the rustup `rustc` shim, so the TUI suite no longer depends
  on `$HOME` or holds the process-wide test environment lock across an
  async wait (#5056, #5423, Isabel Wu).

### Removed

- The source-structure budget ratchet (CI step, checker, baseline JSON).
  It measured line counts, not quality: every legitimate feature required
  a hand-edited ceiling and the accompanying "review" was self-review, so
  it bought ceremony, not protection. Behavior-measuring gates (dead-code,
  runtime-contract, persistence-backlog) stay enforced.
- DeepSeek can reuse a key already stored by official DeepSeek Harness
  (`dsh`) after
  `codewhale auth external-consent --provider deepseek --mode read-only`.
  Codewhale reads only `DEEPSEEK_API_KEY` from the exact granted
  `$DSH_HOME/.credentials.yaml` and never writes or refreshes that file.
- The TUI markdown parser now honors CommonMark fence-length rules: a ````
  opener is not closed by a shorter ``` line, so `>` content inside a longer
  fence stays literal code instead of escaping into a quote.
- Sub-agents finalize when the parent session id changes, so a closed
  session cannot block new children as a live owner (#5372).
- Child spawn-route receipts stay live and usage is deduped by response
  (#5366).
- Doctor keeps persisted setup readiness across first-run / update
  checkpoints (#5340).
- Approval default selection is applied and explained; agents are told
  when approvals are disabled (#5293).
- A `/models` 2xx probe is a connection check, not model readiness.
- Site layout uses one container, the ticker no longer implies false
  provider readiness, and install links stay in the active locale.

### Contributors

- Arlo (@xiaoray-blip) — MCP list responses omit `nextCursor` when there
  are no further pages (#5336, reported in #5335).
- EvanProgramming (@EvanProgramming) — webhook client panic fallback
  (#5381); session-index JSONL mutex (#5382).
- Lstarsky0 (@Lstarsky0) — session peek hides internal runtime events
  (#5376); thinking-ladder test re-pin (#5378); provider-count follow-ups
  (#5383/#5384); macOS agy fixture canonicalization (#5392); zh-Hans 宪章
  terminology (#5397); regenerated website facts harvested and corrected
  from #5398.
- Matt Van Horn (@mvanhorn) — read-only models settings preview on the
  website (#5411, fixes #5370).
- Nhat Bui (@buiducnhat) — canonical `ultra` reasoning effort mapped across
  provider effort tables (#5409); session titles truncated by character
  count, not byte offset (#5415).
- Sh1Zuku (@SparkofSpike) — `/title` and the session name in the terminal
  tab/window title, plus the mid-turn title deadlock fix (#5419).
- Kai Nacke (@redstar) — Eden AI provider registration, aliases,
  `EDENAI_API_KEY`, and the global/EU endpoints (#5422).
- Isabel Wu (@wuisabel-gif) — background verifier test isolated from
  rustup and `$HOME` (#5423, slice of #5056).

## [0.9.7] - 2026-08-12

Codewhale v0.9.7 keeps the catalog ordinary. Grok 4.6 lands as a normal catalog
row instead of a provider-shaped pile of special cases, OrcaRouter joins as a
named provider, and a panic-safety advisory in `lru` is cleared by lifting the
pin that caused it rather than living around it.

### Added

- Grok 4.6 is the direct xAI default, with the `grok` alias moving onto it and
  `grok-4.5` still explicitly selectable. Its 500K context, text/image input,
  tool and structured-output support, `low`/`medium`/`high`/`xhigh` reasoning
  efforts (default `high`), and server-side web search all come from the
  Models.dev-shaped catalog rather than model-specific code. `reasoning_effort`
  reaches the wire only on the exact first-party `https://api.x.ai/v1` route,
  and the usage-aware 200K-token pricing boundary is scoped to direct xAI so
  aggregator routes reusing the model slug cannot inherit xAI billing.
- OrcaRouter is a first-class named provider: `ORCAROUTER_API_KEY`, default base
  URL `https://api.orcarouter.ai/v1`, `deepseek/deepseek-v4-pro` default,
  `orcarouter/auto` routing, CLI `--provider` selector, and TUI picker entries
  (#5321).

### Changed

- Reasoning-effort normalization and the model picker read a model's published
  `reasoning_options` list from the catalog instead of collapsing every route to
  the historic Low/Medium ladder. Any catalog row that publishes an effort list
  keeps its own vocabulary.
- Docs record DeepSeek's live `DeepSeek-V4-Pro-0813` backend label while the
  callable API ID stays `deepseek-v4-pro`. No aliases are remapped and no
  `deepseek-v4-pro[1m]` selector is sent.

### Fixed

- Auto-Review once again executes proven read/build/test shell commands and
  bounded workspace writes without opening an approval modal. Explicit policy
  blocks, unknown tools, publish operations, secret actions, MCP mutations,
  and shell commands requiring approval still fail closed (#5323; reported by
  USTHzhanglu and root-caused by Lstarsky0).
- Copying a user or assistant message takes its canonical content instead of
  reserialized transcript lines, keeping role glyphs, continuation rails, and
  visual wrapping out of the clipboard while preserving authored Unicode,
  Markdown, and hard line breaks. Tool and Thinking cells stay on the existing
  full-transcript path (#5319).
- `load_session` no longer runs crash recovery on every read. Snapshot reads go
  through a side-effect-free `load_session_snapshot` and recovery is explicit
  via `recover_session_for_resume`, so an embedding host inspecting a durable
  session while a tool is still running no longer gets a spurious crash repair
  (#5320).

### Security

- `lru` moves to 0.18 to clear RUSTSEC-2026-0253, where `LruCache::pop()` was
  not panic-safe and could leave dangling list pointers. The `ratatui-core`
  `=0.1.0` pin that transitively forced `lru` ^0.16 is lifted, so
  `ColorCompatBackend` now answers `get_cursor_position()` from tracked cursor
  state — the upstream-recommended workaround for the startup CPR race
  (ratatui/ratatui#2483, ratatui/ratatui#2640) that the pin originally worked
  around. `ratatui` itself stays pinned at `=0.30.0`, so the API surface is
  unchanged.

### Known issues

- The integration test
  `exec_persistent_service::failed_exec_kills_pending_service_and_exits_nonzero`
  is a confirmed flake under parallel load ("service pid file never appeared").
  It passes in isolation and is unrelated to any v0.9.7 change.

### Contributors

- XhesicaFrost (@XhesicaFrost) — canonical message copy (#5319).
- h3c-hexin (@h3c-hexin) — session snapshot and crash-recovery split (#5320).
- XiaoHuo888-hue (@XiaoHuo888-hue) — OrcaRouter provider registration (#5321).
- USTHzhanglu (@USTHzhanglu) — Auto-Review regression report and Windows
  evidence (#5323).
- Lstarsky0 (@Lstarsky0) — Auto-Review regression root-cause analysis (#5323).

## [0.9.6] - 2026-08-11

Codewhale v0.9.6 is a subtractive release: fewer runtime guards, one stable
prompt, truthful provider endings, and a smaller compaction path that preserves
the provider cache. The changes were grounded by matched Terminal-Bench 2.1
runs against Pi 0.8.41 and by dogfooding repeated manual compaction.

### Added

- `web_search` defaults to Firecrawl Cloud without an API key; keyless requests
  are headerless and quota-bounded, while an optional user key raises limits.
- Green web builds on `main` now emit an actionable manual-deploy reminder, so
  site changes cannot quietly appear shipped while Cloudflare still serves an
  older revision.
- Mistral AI is a first-class provider route, including Codestral models,
  first-party reasoning support, authentication, picker entries, and aliases.
- Headless `Bash` can transfer explicitly requested persistent Unix services
  out of an exec run, with ownership and cleanup receipts.
- `/remote-env` opens hosted Work from the current GitHub or CNB branch tip and
  states exactly which unpushed, dirty, ignored, secret, and session state stays
  local.
- Linux ARM64 release and nightly assets are static musl builds with native
  launch checks.
- Maintainers can report observed daily active installs from the same anonymous,
  aggregate telemetry dataset; no additional client data is collected.
- Fleet-dispatched members under a read-only evidence (no-network) ceiling now
  keep the `Web` tool's read-only `search` and `fetch` actions — parity with an
  ordinary scout — while every reaching surface (`web.run`, `fetch_url`,
  `github`, MCP) stays denied and the sentinel-backed capability envelope
  remains the fail-closed backstop.
- `/fleet setup` can show an optional, deterministic, unratified role-to-model
  advisory built only from configured ready routes. Accept, edit, and reject
  all remain inside the existing human-reviewed profile save boundary; the
  advisory never launches a Fleet or writes a second configuration.
- `/update` checks for a newer Codewhale release and installs it from inside
  the TUI, while `tui_help` gives agents the same command and key map users see.
- Markdown file paths render as OSC 8 links where the terminal supports them,
  and every agent row can open that agent's transcript directly.
- ACP editor sessions can execute multi-round file, search, Git, patch, and
  explicitly enabled shell tool calls through the shared Runtime registry.
  Shell access requires both the client's terminal capability and Codewhale's
  headless shell opt-in, and cancellation stops an in-flight tool before the
  turn returns (#5225 by @rafaelcavalheri).
- Lowercase `read` returns bounded typed PNG, JPEG, GIF, and WebP results to
  image-capable Chat, Responses, Anthropic, and ACP routes. Text-only routes
  receive an explicit omission receipt; image bytes never spill into ordinary
  transcript, export, compaction, or relay text.

### Changed

- Anonymous usage counting is on by default for fresh installs and disclosed in
  a native first-run Codewhale modal with an immediate opt-out. Prior declines
  remain off. Codewhale does not collect conversations, code, prompts, files,
  repo or branch names, credentials, model content, or per-turn activity
  timelines.
- Wide terminals use a responsive, full-screen ocean canvas with modest
  gutters: prose keeps a readable measure while tools, diffs, work surfaces,
  the composer, and status chrome can use the available width. Turn and major
  activity seams breathe without padding every call inside a tool group.
- Root CLI help describes product actions directly instead of exposing internal
  TUI/runtime layers.
- `Bash action="wait"` now blocks by default when a wait is requested; callers
  can still ask for a nonblocking snapshot, and persistent service ownership
  remains explicit.
- Compaction is one cache-stable summary request followed by one committed
  replacement summary and a bounded recent-message tail. Older saved sessions
  still restore.
- Ask, Work, Auto-Review, and Full Access share one stable base prompt. Modes
  continue to differ through permissions and the live tool catalog; the former
  Act label is now Work throughout the product and shipped locales.
- Full Access now auto-approves non-bypassable tools consistently, and the
  default choice shown on ordinary approval cards is configurable.
- Model, context-window, dispatch-name, and nested-agent spawn receipts report
  the route and limits actually used rather than silently substituting a
  guessed identity.
- Child-agent launches mint one immutable route receipt before admission and
  preserve it through status, interruption, completion, resume, Work Graph,
  and ledger projections, so provider/model attribution cannot drift (#5305).
- Goal runs no longer stop because of internal continuation, repeated-gap, or
  unanswered-question guards. Explicit user limits and terminal goal states
  remain authoritative.
- Account-owned `/rc` remote control now keeps exclusive ownership and a
  crash-recoverable delivery journal until the server acknowledges terminal,
  approval, failure, and snapshot state.
- `todo_write` is an optional progress surface rather than required model
  ceremony.
- New turns use one small, stable toolbox: `read`, `write`, `edit`, `bash`,
  `agent`, `todo_write`, and `tool_search`. The optional progress tool stays
  visible as familiar working memory; specialized native, Web, MCP, plugin,
  memory, task, and verification tools are policy-filtered and searchable;
  activated schemas stay in a bounded per-conversation cache. Every sub-agent
  keeps its own search and cache, including policy-allowed Web research, while
  forked context and parent activations remain warm starts rather than allowlists.
- The direct file and shell schemas follow Pi's deliberately small contract:
  bounded complete-line reads, hash-free writes, unambiguous multi-edit with
  BOM/CRLF preservation and conservative fuzzy matching, and one foreground
  `bash` command with a bounded chronological output tail. Modes change
  execution authority, not those primitive names.
- Codewhale no longer re-states the To-do list to the model. The model learns
  what is on the list from the tool result its own `todo_write` call returned,
  which is ordinary conversation history — the same way Pi's To-do works. The
  transient `<codewhale:work_state>` block that used to ride the tail of every
  parent turn-loop and sub-agent step request is gone, along with the stable
  system prefix being disturbed by list changes. A snapshot is still shown once,
  where a person asked for it: the `<codewhale:fork_state>` block a newly forked
  sub-agent is handed, `/relay` handoff instructions, and the agent card. The
  complete To-do stays visible in the UI. A structural test asserts real
  outbound provider request bodies do not carry the list.
- Scout and Reviewer name the read-only investigator roles. Both expose exactly
  one shell entry point — canonical lowercase `bash`, bounded by the strict
  read-only classifier — and the legacy `Bash` alias stays denied in the catalog
  and at dispatch. Previously a case-insensitive name match let a call spelled
  `Bash` execute through that carve-out, returning raw shell to a read-only role.

### Fixed

- Sending more context while a lowercase `bash` command is running now moves
  the command to `/jobs` and returns a successful running receipt instead of
  falsely reporting `Command exited with code -1`; the process keeps running
  and its completion still arrives through the normal runtime event.

- First-run usage disclosure now opens as a native Codewhale modal instead of a
  shell questionnaire before application startup. Telemetry remains unarmed
  until the native choice is made, and an in-memory Disable choice governs the
  current session even when its preference cannot be saved.
- `/compact` completion, failure, queued, duplicate, and mailbox outcomes are
  durable transcript receipts instead of short-lived toasts. A stray terminal
  event can no longer leave every later compaction stuck as already running.
- Compaction now follows Codex's simple transcript shape: recent user context
  followed by one ordinary history checkpoint. It never appends the summary,
  the To-do list, or volatile shell/worker state to the standing system prompt;
  reloads migrate the persisted carrier back into exactly one history item.
- Automatic compaction uses a percentage of the real context window, clamped to
  the route's spendable ceiling. Pressure comes from the current parent-route
  prompt, not cumulative billing or child-model usage.
- Compaction, review, verify, routing, setup, Fleet, MCP, RLM, vision,
  translation, and sub-agent calls inherit the resolved route's normal output,
  sampling, and reasoning policy. Small internal-task token caps no longer
  truncate thinking routes or special-case individual providers.
- Incomplete provider responses fail truthfully across ordinary turns and every
  internal model consumer. Partial text stays interrupted, pending tool calls do
  not execute, and billed usage is retained.
- Transport-only `(reasoning omitted)` placeholders no longer enter new
  transcripts and are filtered from restored sessions. Reasoning expand/collapse
  actions stay attached to the exact rendered cell, including after replacement,
  restore, filtering, and resize (#5291).
- Step-budget exhaustion is a typed failure and cannot release a pending
  persistent service. Cancellation after terminal usage still charges the turn.
- Deferred tools now preserve a completed result when a provider reuses its
  tool-call ID on the retry turn, preventing successful plugin calls from
  entering a repeated execution loop.
- Website setup, provider, diagnostics, Fleet, and single-runtime claims now
  match the source candidate.
- Opening the sub-agent register no longer hides the to-do list: the Agents
  panel shows the full register and the durable checklist together, and the
  register header is a two-way door that returns to Tasks on a second click.
- The ⌥V / Alt+V details chord opens the selected work-surface row's own
  inspector instead of the transcript's nearest tool cell, so a selected
  to-do row shows its own content rather than the latest reasoning.
- The first-run usage disclosure now asks a clear question — "Help improve
  Codewhale?" — with unambiguous "Yes, keep anonymous counts" / "No, turn off
  tracking" choices in every shipped locale, and states the persistent opt-out
  command. Consent semantics are unchanged: telemetry stays unarmed until a
  choice is made.
- macOS screencapture screenshots referenced in a message are copied to a
  stable attachments directory the moment the message is received, and the
  reference is rewritten to the stable path, so the image still exists when
  the agent reads it. Only files under a screencapture "Temporary Items"
  directory are touched; copies are idempotent and a failed copy keeps the
  original reference.
- Manual `/compact` during an active turn now queues even when the engine's
  bounded op mailbox is saturated. The request defers client-side, retries as
  mailbox slots free, and cannot latch as already running after it settles.
- Interactive `/load`, startup `--resume`, and `/resume` picker paths preserve
  the persisted provider, endpoint, and model identity; picker resume also
  leaves a durable transcript receipt.
- Relative `mcp_config_path` values no longer depend on the launch directory or
  silently load an empty server pool: Codewhale warns and falls back to the
  user-global MCP configuration. Explicit absolute paths remain authoritative.
- Alibaba Model Studio `qwen3.8-max` and `qwen3.8-max-preview` still stream
  their current reasoning, but no longer replay historical `reasoning_content`
  that those routes do not accept. Historical reasoning replay is now gated by
  the exact provider/API/model contract, so unknown `*-thinking` lookalikes
  fail closed while documented Qwen, Kimi, DeepSeek, Mistral, Anthropic, and
  Responses continuity rules remain intact.
- Compatibility File/patch calls retain optional content-hash guards when a
  caller supplies them. The new direct `write` and `edit` schemas do not expose
  hash or prior-read ceremony.
- Shell previews hold back incomplete UTF-8 sequences instead of emitting
  replacement characters, and compaction receipts report token deltas.
- Nested agents may narrow but can never widen their inherited depth budget
  (#5317 by @ousamabenyounes).
- Container publication now assembles AMD64 and ARM64 images in parallel on
  native runners from the already-verified static release binaries, then
  publishes and checks one multi-architecture manifest. It no longer rebuilds
  both targets through the single long-running QEMU job that lost its runner.

- Google Gemini is its own backend (`/provider google`) on the official
  OpenAI-compatible route with thought-signature capture/replay and
  fail-closed replay for thinking models. Antigravity (`agy` 1.1.13) joins
  as a separate credential-plane provider: consent-gated read-only import
  of the official CLI's login with `ANTIGRAVITY_API_KEY`/`AGY_ADC_AUTH`
  precedence; requests fail closed until the cloud-code wire protocol is
  implemented.

### Removed

- The no-progress guard, repeated-read guard, and injected tool-error strategy
  coaching. Productive polling, repeated inspection, and model-owned recovery
  are no longer interrupted by runtime heuristics.
- Never-wired decision-card, keybinding, hover, shell-execution, engine-op, and
  release-script paths were deleted so the supported runtime has one route for
  each behavior.

### Contributors

- Xavier Pestel (@xavierpestel-ai) — Mistral AI provider route (#5295).
- Ben Younes (@ousamabenyounes) — inherited nested-agent depth cap (#5317).
- Rafael Cavalheri (@rafaelcavalheri) — ACP agentic tool turns (#5225).

## [0.9.5] - 2026-08-08

Codewhale v0.9.5 consolidates the terminal application into one compiled
runtime while preserving the familiar `codewhale` and `codew` commands. It
also expands the managed Runtime API, makes session and Fleet work easier to
inspect and resume, and removes the hidden local continuation backstop that
could end productive work without a final assistant response.

### Added

- **`model = "auto"` for prompt-based tier selection**: When set, the
  dispatcher analyses the user's prompt before delegating to the TUI and
  selects `deepseek-v4-pro` for complex tasks or `deepseek-v4-flash` for simple
  tasks (PR #5257).
- Runtime API controls for persistent goals, bounded memory inspection, MCP
  server and skill lifecycle management, and durable Fleet receipt evidence.
- Append-only session-tree history with `/tree`, `/branch`, `/fork`, and
  `/resume`, plus `/rc` remote control and managed login.
- A unified Fleet roster for built-in dispatch postures and a pinned indicator
  that keeps active background work visible above the composer.
- Incremental MCP registry refreshes that return the local snapshot immediately
  and update it in the background.
- Scout and Reviewer agents can use a bounded direct-command evidence shell for
  read-only workspace, Git, and GitHub inspection, and can keep private working
  notes in their own To-do while the durable transcript retains their evidence.

### Changed

- `codewhale-cli` now contains the terminal runtime directly. Release installers
  expose byte-identical `codewhale` and `codew` commands without a separate TUI
  executable. v0.9.5 introduced deprecated `codewhale-tui-*` release filenames
  as byte-identical compatibility copies; later releases retain those filenames
  while installed v0.9.4 clients remain supported upgrade sources.
- Startup release checks cache successful lookups for one hour. The updater
  downloads and verifies the primary runtime once, then refreshes any existing
  `codew` or legacy `codewhale-tui` command paths from the same bytes.
- Headless `codewhale exec` runs and verifier benchmark rollouts no longer
  impose a 100-step default. `--max-turns` remains available as an explicit
  opt-in ceiling; Fleet workers retain their separately configured budget.
- Goal token and time budgets are telemetry rather than default stop
  conditions, and automatic goal continuation is unlimited unless the user
  explicitly configures a continuation ceiling.
- Command-palette and slash-completion shadowing now share one alias-aware
  discovery contract.
- The website install guidance, localized product copy, navigation controls,
  social metadata, and Cloudflare build pipeline now describe and deploy the
  same one-runtime release contract.

### Fixed

- The hidden 20-step no-user-input backstop no longer ends productive turns.
  Tool results, queued steering, child completions, REPL feedback, and goal
  continuations can all reach the next provider step and a final assistant
  response; explicit user-configured limits and genuine stuck-loop guards remain.
- Complete error details are directly inspectable after a failure instead of
  leaving the terminal with a clipped, unrecoverable error fragment.
- A newly minted OAuth credential is adopted in the same provider-selection
  flow instead of requiring a second picker trip.
- Fresh session titles can replace a stale cached `New Session` placeholder,
  unknown model context limits fail loudly, and release/source-install fallbacks
  no longer request binaries removed by the single-runtime conversion.

### Contributors

- [Sh1Zuku](https://github.com/SparkofSpike) (`@SparkofSpike`) fixed stale
  cached session titles that could pin the `New Session` placeholder.
- [Paulo Aboim Pinto](https://github.com/aboimpinto) (`@aboimpinto`) built the
  shared alias-aware command discovery contract and acceptance coverage.
- [Sun Zhenyuan](https://github.com/bistack) (`@bistack`) contributed the
  background incremental MCP Registry refresh.
- [SKY ZHAO](https://github.com/skyzhao1223) (`@skyzhao1223`) contributed
  prompt-based `model = "auto"` routing in PR #5257.

## [0.9.4] - 2026-08-07
Codewhale v0.9.4 ships the release-train harness work: the familiar Fleet
roster/setup face with a clear operator-leader and user/folder scope, a
work strip that keeps actionable agents instead of a permanent archive,
waiting policy that forbids polling without freezing independent work,
calmer tool output and session recovery, account/Workflow-search/
automation/handoff surfaces, a shorter translation-ready website, and
release-blocker fixes across permissions, DeepSeek Responses, SQLite,
File edits, terminal width, and Windows installation.

### Added

- Memory maintenance: `remember` gains `revise` and `retire` beside the
  default `append`. Both name the exact note they target and both require
  the evidence for the change. Append-only memory decays — a correction
  sits behind the note it contradicts and both keep reaching the model —
  so the model can now keep its own durable notes true instead of only
  adding to them.
- An audit trail for durable state the model writes about you. Every
  in-place memory edit is journalled to `memory/JOURNAL.md`, and every
  continual-harness `refine` / `remove` to a `JOURNAL.md` beside its state,
  each with before, after, and evidence. Harness removal previously left no
  record at all even though the entry leaves state entirely, so the journal
  is now the only place its content survives.
- A first-run tip that says so: the first time Codewhale saves something
  durable it points at `/memory`, translated into all fifteen complete
  locale packs. This state shaped later sessions and nothing ever mentioned
  it existed.

- Sub-agent checkpoint resume: `agents/followup` resumes an
  `interrupted_continuable` child from its checkpoint into a fresh agent loop —
  new agent id, original prompt plus the prior conversation tail — when a
  runtime is attached, and otherwise keeps queue-only semantics with the
  `continuation_handle` returned; a second followup on the same interrupted id
  returns the existing resumed target instead of spawning a duplicate (PR #5242).
- MCP Registry discovery with Registry-first tool selection: `registry_sync`
  surfaces the eligible local stdio catalog as a complete model-side candidate
  set, connect-failure messages classify early-exit and usage-help output and
  point recovery at the next Registry candidate, and a bundled `mcp-discovery`
  skill documents the flow (PR #5238).
- Progressive fresh-context disclosure: fresh sessions ship a minimal
  constitutional kernel — ground truth, user intent and scope, truthful
  completion, guarantees in mechanism, and precedence — with procedural
  playbooks disclosed on demand, an opt-in project context pack
  (`project_context_pack_enabled`) counted in context reports, and `load_skill`
  catalogue discovery via `name="list"`; the measured fresh-context budget
  drops by roughly 40% (PR #5077).
- Named Fleet store v2: one self-contained TOML Fleet per configuration
  (`schema = "fleet"`), with scope-explicit selection (user-global default vs
  folder override), migration receipts from legacy role profiles, and atomic
  saves that refuse to clobber a different Fleet on the same slug.
- Scout replaces the user-facing "faster" control: catalog-verified fast
  siblings only, never a guessed model name; pinned Scout survives operator
  changes.
- Truthful model-picker rows: vision/tools/limits chips only when the catalog
  knows, with provider → family → exact model grouping.

- **Opt-in product telemetry, off by default.** A first-run notice asks once, on
  a terminal, with declining pre-selected — Enter declines. Nothing is collected
  unless both `telemetry = true` and a recorded "Enable" answer are present, so
  a `telemetry = true` written before this release stays inert: the key has been
  settable and inert for a long time, and setting it was never consent.

  An enabled session sends its batches to the first-party ingest endpoint,
  `https://telemetry.codewhale.net/v1/telemetry`, which is the shipped default
  for `telemetry_endpoint`. That is a Cloudflare Worker whose complete source is
  in this repository under `telemetry-ingest/`; it writes to Workers Analytics
  Engine, whose row is exactly `_sample_interval`, `blob1`–`blob20`, `dataset`,
  `double1`–`double20`, `index1`, and `timestamp` — **there is no IP, country,
  or geo column**, so storing one is structurally impossible rather than merely
  disabled. The handler reads two request headers, never touches the request's
  geo properties, logs nothing, and validates against a closed field set that
  rejects an entire batch carrying any unpublished key. Cloudflare's retention
  for that data is a fixed three months. Setting `telemetry_endpoint = ""`
  instead writes each batch to `$CODEWHALE_HOME/telemetry/dryrun.jsonl` and
  constructs no HTTP client at all, so you can read exactly what would have been
  sent.

  Turning it off is an answer, not a flag: it deletes the random install id,
  truncates every buffered event, and leaves a permanent tombstone that a
  session already running re-checks before it appends and before it sends. A
  failed wipe fails closed. `CODEWHALE_TELEMETRY=0` is a hard floor that beats
  `--telemetry true` and the config key, and a value the parser cannot read
  also resolves to off. Fleet workers are hard-off. A repo-local
  `.codewhale/config.toml` can set neither key.

  Never collected: prompts, completions, tool arguments, diffs, file contents,
  filenames, paths, git remotes, repo or branch names, memory entries, chat
  history, credentials (not even a boolean asserting one exists), model ids,
  custom provider table names, MCP server names, error or panic message bodies,
  per-event timestamps, keystrokes, clipboard, screenshots, or location. The
  full schema is [`docs/TELEMETRY.md`](docs/TELEMETRY.md), and a test parses the
  field names out of that file and asserts set equality with the structs the
  serializer uses.

  This supersedes the roadmap's previous "no Codewhale product telemetry" entry,
  which moves from "Ruled out" to an opt-in framing. What stays ruled out:
  always-on or silent telemetry, per-keystroke or per-tool-call phone-home, and
  any third-party ad or analytics SDK in the runtime binary.
- Registered `GLM-5.3` (direct Z.ai) and `z-ai/glm-5.3` (OpenRouter) as
  selectable GLM routes, with their aliases (`glm-5.3`, `glm-5-3`,
  `zai-glm-5.3`, `zai-glm-5-3`). Z.ai had **not released GLM-5.3 as of
  2026-08-03** — the ids are registered so they resolve to the Z.ai/OpenRouter
  routes instead of being rewritten to another vendor's model, and they will
  fail upstream until Z.ai ships the model. Metadata (context, output,
  reasoning controls) is inherited wholesale from `GLM-5.2` pending official
  Z.ai release metadata; pricing is intentionally absent, and `GLM-5.2` remains
  the default Z.ai model. No third-party gateway roster gained the model:
  OpenCode Zen, OpenCode Go, Alibaba Model Studio, and TelecomJS publish no
  glm-5.3 entry, so Codewhale advertises none.

- Managed Codewhale account commands (`account login`, `status`, `logout`, and
  `keys`) with browser device flow, profile- and origin-scoped secure sessions,
  refresh/revocation, redacted BYOK-vault management, and a token-free Runtime
  account receipt. Provider authentication remains separate, and `cloud`
  remains a compatibility alias.
- `/automation` operator controls to list, inspect, pause, resume, delete, and
  run durable automations. Creation remains on the approval-gated
  model-visible `automation` tool.
- A provider-neutral `WorkflowSearchSpec` authoring and freeze boundary, plus
  structured 2–16-candidate experimental search in the best-of-N Workflow
  starter. It freezes baseline, route, evidence, evaluator, gate, score, budget,
  and review policy before admission; it validates gate/scoring commands but
  does not execute or certify them itself.
- The bundled generation-9 `handoff` skill for compact, decision-ready
  continuation across sessions.
- Expanded terminal LaTeX rendering for aligned and matrix environments,
  cases, arrays, text/font/accent commands, brackets, symbols, and
  command-aware scripts (PR #4981).
- Exact 40-character build provenance and secure account-session capability
  receipts on `/v1/runtime/info`; unknown source provenance continues to fail
  closed.
- Acceptance-level Gherkin coverage locking the existing user-command
  precedence, alias shadowing, fallback, and invalid-command error contract
  (PR #4992).
- Agent Plugins v1.0.0: consume, publish, and slugify packaged sub-agent
  briefs, with an install/update/uninstall on-ramp in the TUI (PR #5182). A
  plugin bundles a prompt, posture, and routing as one shareable artifact;
  on-disk migration of the older `plugin.toml` scaffold is deliberately out
  of scope for this train.
- `send_later`: a model-callable one-shot delayed continuation tool, so the
  model can schedule a single future nudge without an operator-approved
  durable automation (PR #5138).
- `/advisor`: an opt-in background advisor watcher for live turns (PR #5139).
- Notification quiet mode with per-category switches and action-first copy
  (PR #5066).
- Automation scheduling forms — one-shot `ONCE`, five-field cron, and honest
  watcher modes — created through the approval-gated `automation` tool
  (PR #5183).
- Sub-agent `resume_from` continuation chains (PR #5142), child-result
  diff-tainting when a claimed diff is not visible to git, per-turn usage
  receipts on the exec stream-json stream, and spawn receipts that report
  the model each sub-agent actually ran on.
- Transport resilience: sub-agent exec transport retries with a 600 s
  default (PR #5210), SSE header stalls retryable instead of fatal, and
  headless turn resume after mid-stream network drops with an `EX_TEMPFAIL`
  exit.
- Session durability and control: a deterministic compaction continuation
  contract (PR #5064), persisting interrupted output (PR #5206), stop-word
  cancellation (PR #5207), token-counter refresh (PR #5204), deny-by-default
  approval cards (PR #5090), and the Operate completion gate (PR #5067).
- zh-Hant promoted to a full shipped locale with complete `en.json` parity
  (PR #5143).
- A persistent update-available chip in the header, with the startup update
  check throttled and naming the right command.
- RLM static intent extraction for code blocks (`rlm_block_intent.rs`)
  landed as groundwork for a future code-mode approval flow; it is not yet
  wired into the turn pipeline and ships dormant by design.

### Changed

- `/fleet` is the familiar roster/setup face again. The operator row is the
  Fleet leader (session model); the header names the selected saved Fleet and
  whether it is user-global or folder-scoped. Named-Fleet switching lives under
  `/fleet fleets` (Enter selects in the row's own scope). Session route changes
  stay temporary until `/fleet save`, `/fleet save-as`, or `/model save-default`.
- Waiting-for-subagents directions forbid peek/status polling and sleep-as-wait,
  but allow independent work that does not depend on a child's result — the
  parent no longer freezes mid-turn with useful non-conflicting work available.
- `workflow run` no longer requires `--fleet`; a saved Fleet is an optional pin
  layer over roles + the session route.
- Homepage and getting-started copy is shorter and scannable across locales,
  with dictionary key and `{brand}` token parity preserved.

- Tool results now render as ordinary bounded previews with real expansion;
  storage, retention-ledger, and internal evidence language no longer leak into
  normal transcripts.
- Prose wrapping, goal state, modal questions, composer-tail behavior, and
  ambient motion now follow one deterministic interface contract across narrow
  terminals and fast streams.
- Scout and reviewer Fleet roles gain network access and the bounded
  verification surface for real reconnaissance while retaining the no-write,
  no-raw-shell security floor.
- Workflow runs may describe up to 1,000 tasks while admitting at most 16 live
  tasks at once through the host concurrency gate. Tournament ordering now
  supports explicit score-first selection while retaining its cost-first
  default.
- Runtime permission compatibility inputs resolve to one live
  `permission_posture`. Auto-Review can proceed without approval or structured
  question modals, unresolved holds fail closed, and a call planned under stale
  authority is retried after a posture change (PR #5025).
- Duplicate and drifting per-turn metadata has been removed in favor of
  runtime-owned authority, and large inline account and skill tests now live in
  owned test seams.
- Pinned Ratatui to 0.30.0 and ratatui-core to 0.1.0. ratatui-core 0.1.1+
  makes `Terminal::clear()` issue a blocking cursor-position report that
  raced the TUI input loop and could kill first launch; both pins are
  load-bearing, because 0.30.0 declares `ratatui-core ^0.1` and would
  otherwise resolve forward on its own (PR #5192 by @bistack; upstream
  ratatui/ratatui#2640).
- Updated globset to 0.4.19, clap-complete to 4.6.8,
  futures-util to 0.3.33, libc to 0.2.189, actions/stale to 11.0.0, and
  docker/login-action to 4.5.2. The locked graph also includes the
  event-listener 5.4.2 fix for RUSTSEC-2026-0221.
- The progress surface now speaks plainly everywhere: the last user-visible
  "Work update is pending" notices say "To-do list", the tool constructor and
  the docs name `todo_write` as the single canonical progress tool, and
  `work_update`, `TodoWrite`, and `todo` stay registered as hidden
  compatibility aliases so saved transcripts keep replaying.
- Sub-agent and `agents/wait` waits stay short by default and by cap:
  blocking waits default to 30 s and refuse to block past 120 s, because a
  blocked wait deafens the session to typed input and settled children
  already report back as `<codewhale:subagent.done>` sentinels.
- `Bash` `action=wait` honors `timeout_secs` (seconds) and bare `timeout`
  (milliseconds) alongside canonical `timeout_ms`, and `block` as an alias
  for `wait`, so a habit formed on other wait tools gets the duration it
  asked for instead of silently falling back to the 30 s default; the result
  metadata reports the real `wait_timeout_ms` applied.

### Fixed

- The memory journal is no longer indexed as memory. It is Markdown in the
  memory tree, so the source walk collected it and every retired note
  re-entered the searchable set under its `before:` line — putting the
  exact facts a revision had just removed back into the prompt.
- `memory_path` pointed at an already-native store no longer derives a
  second store nested inside it, which silently wrote somewhere other than
  the file the user named.
- `muse` and `muse-spark` resolved to `muse-spark-1.1` in the agent
  registry while config had defaulted to `muse-spark-1.2`, so the CLI and
  app-server routed those aliases somewhere the configured default never
  pointed. The registry now carries 1.2 and the contributor variant.

- An explicit `type=builder` (or its `implementer` alias) plus
  `write_authority=read_only` now fails closed at spawn instead of launching a
  labeled write role that silently had only read-only tools and then self-BLOCKED
  after burning a turn (#5123). The check is deliberately narrow, because two
  neighbouring combinations are legitimate and stay legal:
  - `type=worker` + `read_only` — worker is the unnamed default (it renders as
    `general`) and takes its capability from authority, not from its name, so a
    read-only worker is an ordinary general-purpose child. Worker, scout,
    reviewer, and verifier remain the four canonical read-only Fleet roles.
  - any `role` + `read_only` — `role` is an identity for roster resolution, not
    a capability claim, so an acceptance Workflow can still resolve
    `implementer` to its saved profile while scoping that child to verification.

  Callers that spelled a read-only narrowing as `type: "implementer"` should
  move it to `role: "implementer"`.
- User-global credentials survive an explicit workspace `CODEWHALE_CONFIG_PATH`
  that selects a route with no local key — readiness probes the user-global
  provider table before concluding a key is missing.
- Sub-agent token figures on the work bar accumulate input+output (the same
  total the worker budget uses) instead of completion tokens alone; elapsed
  time still freezes when the child settles.
- Live work-bar rows for sub-agents show how many to-dos they still have
  left (`N left`) when the child's own list has unsettled items — never a
  fabricated zero when no list exists.

- Surfaces no longer claim an OS sandbox on platforms that cannot enforce one.
  The policy resolver takes no platform input, so on default Linux (bubblewrap
  is opt-in) and on all Windows the header chip read `files: workspace` and
  `/status` read `sandbox workspace-write` while nothing was restricted. Both
  now resolve the real backend and say `(unenforced)`.
- `tool_category` hook conditions matched only retired tool names, so a
  `category = "shell"` **deny** hook — the security control `docs/HOOKS.md`
  documents — silently never fired. Categories now use the registered names,
  and multi-action tools classify by action.
- A `Retry-After` header of `-5`, `nan`, or `1e300` crashed the request task
  (`Duration::from_secs_f64` panics on a negative). Parsing is now guarded and
  bounded to one hour.
- Bearer tokens no longer leak into operator-visible receipts. `Authorization:
  Bearer <jwt>` split into two tokens and the JWT matched no redaction rule;
  prefix matching was also case-sensitive, so `SK-live-…` survived.
- `prune_older_than` destroyed the NEWEST rollback snapshots and kept the old
  ones — on every boot, for any workspace with snapshots spanning the retention
  window. Both prune paths now share one orphan-chain rebuild and preserve each
  survivor's real timestamp.
- An absolute or relative command path no longer defeats every execpolicy deny
  rule (`/bin/rm -rf /` did not match a `rm -rf /` rule), and a typed `Allow`
  rule no longer auto-approves a chained suffix such as `git log ; curl … | sh`.
- Wrong types on `File` read range params and `Bash` stdin/cwd/task_id are now
  errors instead of silent defaults — a `start_line:"1200"` string used to
  return the head of the file, and a non-string `stdin` ran the command with no
  stdin and reported success.
- Multibyte tool ids no longer panic the context inspector, wide (CJK) text no
  longer overflows the decision card, and a hostname like `127.evil.example.com`
  is no longer treated as loopback.
- Refusals name calls the model can actually make (`rlm action='open'` rather
  than a retired `rlm_open`; `Bash` rather than `exec_shell`).

- Sub-agent dispatch no longer aborts the process. The Tokio runtime was built
  by `#[tokio::main]`, leaving every worker thread on the 2 MiB default while
  only the owner thread received the explicit 16 MiB stack — and the engine runs
  on a worker. A debug-build `agent` dispatch exceeded that stack and raised
  SIGABRT, which is not a panic and so could not be caught; the process died
  mid-spawn with no child request ever issued. Release builds were unaffected.
- Fleet profiles that pin a provider no longer leak a bare model id onto the
  session route. `model_overrides` exported each role's model while dropping its
  provider, so a scout pinned to another provider's model was dispatched against
  the active client and denied at the wire — visible as an instant auth failure
  on the first sub-agent of a fan-out.
- The rail's Pinned panel no longer spends four rows saying "No active work".
  An empty panel now collapses like the Tasks panel always has, and the settings
  migration no longer folds the default `sidebar_focus = "auto"` into a pinned
  always-on strip, which had silently handed that panel to every user who had a
  settings file at all. (An *empty* panel collapses; a panel holding settled
  to-dos or finished workers is not empty — see the standing-register entry
  below.)
- The work bar keeps settled to-dos and an honest Subagents header, while
  completed/cancelled workers collapse out of the Top strip so fan-outs do not
  permanently eat the transcript. Failed or interrupted workers stay visible
  (they still need attention). Settled agents remain reachable through the
  Agents panel and catalog. To-do rows say their state in words (pending /
  in progress / completed / cancelled), and sub-agent rows carry type,
  objective, elapsed, and input+output tokens. Every work row is a door in
  every rail panel and placement: click and Enter open the row's world
  (work inspector / agent details — finished agents included) instead of
  doing nothing. A click after the detail pager closed itself reopens the
  detail rather than being swallowed by a stale toggle.
- The rail strip yields its rows to the transcript when the terminal cannot
  seat both, so the idle ocean survives at 24 rows instead of being evicted.
- `code_execution` and `js_execution` no longer describe themselves to the model
  as sandboxed. Both are ordinary local subprocesses with no seccomp, jail, or
  container (PR #5221 by @h3c-hexin and @asto18089).
- Model Studio reasoning controls now fail closed on the host rather than on the
  provider enum, so a custom `base_url` no longer receives Alibaba-specific
  `enable_thinking` fields, and `qwen3.8-max` is no longer sent a thinking
  switch it does not accept (PR #5233 by @Inference1, closing #5203).
- `config.example.toml` no longer claims Shift+Tab cycles the reasoning tier.
  Shift+Tab cycles the permission posture; Ctrl+T cycles reasoning
  (found by @vFONGv, PR #5229).

- Alibaba Model Studio reasoning controls are now route- and model-scoped
  instead of provider-wide (#5203, harvested from #5233 by
  [@Inference1](https://github.com/Inference1)). Codewhale sends
  `enable_thinking` / `preserve_thinking` / `reasoning_effort` only when the
  configured `base_url` is a verified Alibaba Chat Completions host, so
  pointing a `modelstudio-*` provider ID at a custom gateway no longer injects
  DashScope's dialect into it. `qwen3.8-max` and `qwen3.8-max-preview` are
  thinking-only and no longer receive an `enable_thinking: false` they cannot
  honor; `preserve_thinking` is sent for the models documented to accept it, so
  their reasoning trace survives into the next turn; and `deepseek-v4*` /
  `glm-5.x` map the reasoning tier onto the documented `high` / `max` ladder.

- xAI device login now recovers from a config that points at a missing
  Codewhale-owned credential generation instead of failing every attempt
  with a generic activation error, and finalize failures report the full
  error chain (#5032).
- API keys saved to the secret store no longer read as unconfigured for
  providers that are not currently active; a configured Kimi/Moonshot key
  survives provider switches and restarts without re-entry (#5033).
- Switching to the Codex provider with no saved model now lands on the live
  roster's flagship model instead of a stale static default (#5034).
- Worktree-isolated Fleet builders no longer contend on the per-workspace
  delegated-coordination lock, and a failed lock acquisition is retried on
  use instead of being memoized for the life of the process (#5036).
- Fleet dispatch now rebinds the child client when the resolved profile
  model requires a different wire protocol (DeepSeek flash on Responses),
  instead of failing deterministically on the worker's first request
  (#5042).
- DeepSeek Responses now sends `reasoning.effort: "none"` for the Off tier,
  shows a truthful notice instead of silently discarding server-side
  `web_search_call` items, and parses cache-hit, cache-miss, cache-write, and
  pricing telemetry while retaining the OpenAI-style nested fallback.
- File edits now explain no-op and missing-search failures, reject newly
  unbalanced C/C++ preprocessor replacements, handle the reported
  CRLF/non-ASCII cases, and safely relocate stale unified-diff hunks only when
  whole-file context is unique (PRs #5008 and #5030).
- Circled digits, enclosed alphanumerics, and keycap graphemes use consistent
  two-column measurement in Codewhale, Ratatui, and CJK terminals, preventing
  missing-character and phantom-space corruption (PR #5001).
- SQLite connections install their busy timeout before locking setup and avoid
  rewriting persistent WAL mode on every open, removing the concurrent-open
  release-gate failure.
- The Windows installer preserves long current-user `PATH` values, their
  registry type, and unrelated entries across install and uninstall (PR #5006).
- Provider configuration no longer contains user-reachable panic paths when
  metadata or prior credential state is missing.
- Resuming a session restores composer text only from a same-session persisted
  draft; submitted prompts and internal background-runtime envelopes remain in
  history instead of appearing in the composer (PR #5029).
- Shared CI now handles bot-authored issue-link checks, provisions cargo-deny's
  toolchain, and fetches the locked test graph before offline runtime-budget
  validation.
- Re-quote each linker argument in the Windows OpenHarmony clang launcher so a
  spaced SDK path (e.g. the default `D:\DevEco Studio\...` install) keeps its
  `--sysroot` intact through the final Rust link, and extend the no-SDK release
  guard to keep the re-quoting contract (PR #5095).

- The shell tool reports the real elapsed wait time in its result content
  instead of echoing the requested timeout (PR #5240).
- Transcript wheel scrolling under iTerm2: xterm alternate-scroll (DECSET
  1007) now stays off while mouse capture is active, so wheel events arrive as
  mouse events instead of being converted into arrow keys (#5223, PR #5234).
- A stalled model stream no longer ends the turn as `Completed` over a
  frozen reasoning block: a mid-stream chunk-timeout now counts toward the
  stream-error budget, so a stall with nothing streamed retries the request
  transparently, and a stall that exhausts the retry budget fails the turn
  with the real reason instead of reporting success.
- A finished background shell task now wakes the engine even when no goal is
  active: the idle loop starts an ordinary runtime turn so the completion
  reaches the model immediately instead of sitting unclaimed until the user
  types (a dead provider route claims the completion once and reports where
  the output lives instead of re-arming the same error every tick).
- Sub-agent final reports that exceed the summary budget are now spilled to
  a session artifact, and the truncation footer names the
  `retrieve_tool_result` ref for the elided middle instead of telling the
  model the bytes are unrecoverable; write failures degrade to the honest
  no-ref footer.
- An interactive mid-stream network drop after partial output no longer fails
  the turn: the partial reply is preserved as a committed assistant message,
  a runtime continuation message is appended, and the request is re-issued
  bounded by the stream-retry budget.
- Large pasted input is no longer sent to the model twice as inline text and
  as a backup `.md` paste file; the submitted message now carries only the
  `@`-mention so the model reads the file once.
- A builder sub-agent can run ordinary shell writes again. Write claims
  outlive the agents that register them, so a workspace accumulated one per
  builder that ever ran — six completed agents left four standing claims in
  testing — and the shared-checkout gate counted those long-finished children
  as live contenders. Every later builder was refused `Bash` writes with
  "cannot prove a bounded file target" and pushed toward worktree isolation,
  which puts the work in a checkout the operator never looks at. The gate now
  asks the question it meant to ask: is another *running* child writing in this
  shared checkout. Concurrent writers are still gated; a lone builder writes in
  the workspace you are actually watching.
- Ctrl-C during the first moments of startup no longer kills Codewhale
  outright. The terminating-signal handlers were registered inside the task
  that waits on them, and a spawned task does not run until the scheduler
  first polls it, so a SIGINT arriving in that window hit the default
  disposition — the process died with no exit code, no terminal restore, and
  no session record. The handlers are now installed synchronously, before
  the telemetry notice and before arming, so the window is closed.
- The documented tool list on the docs site named `update_plan` and
  `work_update` as coordination tools. Neither is callable by the model —
  `update_plan` replays older Plan artifacts and `work_update` is a hidden
  compatibility alias — so the page listed two tools a reader cannot use and
  omitted `todo_write`, the one they can.

### Security

- Bumped `nanoid` past GHSA-2v37-7h3g-55p8 (a custom generator given size
  zero could loop indefinitely), restoring a zero-advisory `npm audit` for
  the website.

- Google Gemini is its own backend (`/provider google`) on the official
  OpenAI-compatible route with thought-signature capture/replay and
  fail-closed replay for thinking models. Antigravity (`agy` 1.1.13) joins
  as a separate credential-plane provider: consent-gated read-only import
  of the official CLI's login with `ANTIGRAVITY_API_KEY`/`AGY_ADC_AUTH`
  precedence; requests fail closed until the cloud-code wire protocol is
  implemented.

### Removed

- The default model-facing SlopLedger implementation, its storage-oriented
  transcript language, and the `/debt`, `/cleanup`, `/slop`, and `/canzha`
  command surface.

### Contributors

- [Sh1Zuku](https://github.com/SparkofSpike) (`@SparkofSpike`) contributed
  LaTeX rendering in PR #4981, completed circled-digit/keycap width handling in
  PR #5001, and delivered actionable File-edit recovery in PR #5008; for this
  train he resumed interrupted sub-agents from checkpoints in PR #5242,
  surfaced real shell wait elapsed time in PR #5240, and kept alternate-scroll
  off while mouse capture is active in PR #5234.
- [XhesicaFrost](https://github.com/XhesicaFrost) (`@XhesicaFrost`) fixed long
  Windows user-PATH preservation in PR #5006.
- [Paulo Aboim Pinto](https://github.com/aboimpinto) (`@aboimpinto`) added the
  user-command dispatch acceptance contract in PR #4992.
- [DracheTek](https://github.com/DracheTek) (`@DracheTek`) provided the
  multilingual, CRLF-heavy File-edit failure report in issue #5003.
- [An Ziwu](https://github.com/MuRongMoQing) (`@MuRongMoQing`) reported the
  Windows PATH-overwrite defect in issue #4685.
- [shenjackyuanjie](https://github.com/shenjackyuanjie) (`@shenjackyuanjie`)
  fixed the Windows OpenHarmony linker re-quoting for spaced SDK paths in
  PR #5095.
- [bistack](https://github.com/bistack) (`@bistack`) contributed MCP Registry
  discovery with Registry-first tool selection in PR #5238.
- [vFONGv](https://github.com/vFONGv) (`@vFONGv`) wrote the zh-CN Windows
  beginner guide with screenshots in PR #5229, harvested after its base branch
  was accidentally deleted during maintainer cleanup.
- [mky](https://github.com/mky) (`@mky`) fixed the FreeBSD build (PR #5254, `rquickjs` `bindgen` on FreeBSD).
- [cacdcaecawae](https://github.com/cacdcaecawae) (`@cacdcaecawae`) contributed embedder-owned sub-agent state roots (PR #5252).

## [0.9.3] - 2026-07-31

This is the Codewhale v0.9.3 source candidate. It is not a published release
until the matching tag, packages, checksums, and release assets exist.

DeepSeek V4 Flash is now a first-class Codewhale route, and the agent-facing
tool surface has been reduced to the canonical action tools that current
models actually need. This release also hardens credential, authorization,
durability, compaction, and macOS File Provider boundaries while deleting
stale runtime and dependency surface.

### Added

- Native `deepseek-v4-flash` support over DeepSeek's Responses API, including
  stateless reasoning-item replay, semantic SSE terminal events, structured
  function calls and outputs, `apply_patch`, and model-aware wire-format
  selection. Exact current Flash IDs use Responses; future direct
  `deepseek-vN-*` model IDs inherit that route conservatively, while custom
  DeepSeek-compatible endpoints retain Chat Completions unless configured
  otherwise.
- A pipe-only `codewhale auth print-api-key` handoff for explicitly selected
  providers. It shares Codewhale's home-scoped credential authority, refuses
  terminal output, and prevents sentinel placeholders from becoming live
  credentials.
- Per-turn `max_tool_calls` enforcement at the engine admission gate, plus a
  named-file write scope with a separate read seam. The runtime now rejects
  over-budget calls before execution and keeps the operator's write boundary
  explicit (#4415).
- Runtime-contract, source-structure, and persistence-backlog ratchets that
  name drift instead of allowing large ownership surfaces to grow silently
  (#3921, #4785).

### Changed

- Model-visible built-ins now use the canonical `Bash`, `File`, and `Run`
  action schemas. `apply_patch` remains available as the one direct custom
  edit tool supported by DeepSeek Responses. The bundled stop-ship workflow,
  Fleet fixtures, shell shortcut, and engine tests use the same canonical
  vocabulary.
- Canonical `File { action: "write" }` requests now pass through the same
  semantic repo-law checks as the former write path. Approval, Full Access,
  and workflow execution cannot bypass the repository safety floor by choosing
  the canonical schema.
- Codewhale home resolution is shared across the CLI, TUI, state, and secret
  stores. `doctor` is offline by default, distinguishes credential source from
  availability, and reports one consistent path snapshot.
- Durable runtime event writes are serialized across simultaneous processes,
  blocking history waits move off async workers, and provider quota exhaustion
  remains typed and retryable through compaction (#4522).
- Skill discovery caches the merged catalog behind watched-mtime validation;
  large skill, engine, subagent, UI, and ambient-ocean test blocks now live in
  owned test seams.
- Reasoning summaries stay in the user's language, complete jellyfish
  silhouettes relocate around transcript text, and cached ocean frames include
  their palette identity (#4807).
- The authorization-order contract now documents and tests how modes, hooks,
  permission rules, safety floors, repo law, approvals, and sandboxing compose
  (PR #4980).

### Fixed

- macOS sandbox extensions cover CloudStorage/File Provider workspaces without
  broadening unrelated paths; thanks @Watcher24 for the #4085 report and
  reproduction.
- Foreground shell state detaches before steering, so an interrupted command
  cannot keep owning the composer (PR #4979).
- MCP application-level failures and malformed error envelopes fail closed
  instead of looking like successful tool output.
- Optional PDF failures are truthful and PDF classification no longer misses
  supported inputs.
- Bracketed-paste contents are redacted from traces, and credential diagnostics
  never treat placeholder sentinels as usable keys.

- Google Gemini is its own backend (`/provider google`) on the official
  OpenAI-compatible route with thought-signature capture/replay and
  fail-closed replay for thinking models. Antigravity (`agy` 1.1.13) joins
  as a separate credential-plane provider: consent-gated read-only import
  of the official CLI's login with `ANTIGRAVITY_API_KEY`/`AGY_ADC_AUTH`
  precedence; requests fail closed until the cloud-code wire protocol is
  implemented.

### Removed

- The legacy callable aliases `exec_shell`, `run_shell_command`, `read_file`,
  `write_file`, `list_dir`, `grep_files`, `file_search`, and the duplicate
  Work/RLM registrations. Historical transcript and policy semantics remain
  readable, but new model turns receive only the canonical action surface.
- The bundled PDF parser dependency chain, replacing it with the smaller
  optional extraction boundary tracked by #4382.

### Contributors

- [Turisla](https://github.com/greyfreedom) (`@greyfreedom`) documented and
  locked the authorization-order contract in PR #4980.
- [Nightt](https://github.com/nightt5879) (`@nightt5879`) fixed foreground
  shell detachment before steering in PR #4979.
- [Watcher24](https://github.com/Watcher24) (`@Watcher24`) provided the macOS
  File Provider report and reproduction for #4085.
- [Fred Leitz](https://github.com/fleitz) (`@fleitz`) retains required
  source-candidate credit for the canonical `Bash` workspace fix from PR #4673
  and issue #4674.

## [0.9.2] - 2026-07-29

This is the Codewhale v0.9.2 source candidate. It is not a published release
until the matching tag, packages, checksums, and release assets exist.

### Changed — behavior

- **Legacy `model = auto` no longer elects a network classifier on its own.**
  Holding a DeepSeek API key used to silently select `deepseek-v4-flash` as the
  classifier for every Auto turn — a per-turn cost on a route nobody asked for,
  and one provider privileged over the rest. Auto now stays local and free
  unless an explicit `[auto.router]` block names a provider and model.

  **If you relied on the implicit default**, restore it explicitly:

  ```toml
  [auto.router]
  provider = "deepseek"
  model = "deepseek-v4-flash"
  ```

  `[auto.router]` remains legacy `model = auto` configuration. It is unrelated
  to a Fleet's Adaptive Reasoning Router, which is a saved service referenced by
  name from a Fleet file and decides only how hard an already-frozen route
  thinks.

Landed since v0.9.1, not yet released. A cluster of defects found by a
read-through audit of the policy engine, the MCP proxy, the session index,
and the app-server bridge — several of them cases where the wrong outcome
was reached silently, behind a response or a log line that looked fine. The
release also adds opt-in session, reasoning, localization, and inspectability
surfaces; existing defaults remain stable unless an entry below explicitly
says otherwise.

### Added

- `/permissions` now lists the active user permission-rule source, each rule's
  effective matcher and global/repository scope, and whether that scope applies
  in the current workspace. `/permissions remove <number>` previews deletion
  and requires a snapshot-bound confirmation token, so a concurrent edit
  cannot move a different rule under the confirmed index. Appends and removals
  share one adjacent lock, preserve unrelated TOML formatting and comments,
  atomically replace `permissions.toml`, and reload the live user ruleset
  without clearing session-only approvals. `/config ask-rules` remains a
  compatibility entry; rule creation, glob/directory rules, and deny
  persistence remain out of scope (#1186, PR #4960 by @greyfreedom).

- `/preview-request` (aliases `/dryrun` and `/preview_request`) is a human-only,
  provider-free inspection of the next primary turn. Production dispatch and
  preview share one prepared-request seam across Chat Completions, Anthropic
  Messages, and OpenAI Responses, so the manifest reads the final wire model,
  reasoning controls, tool choice, tool schemas, and body hash from the same
  value production sends. Route, tool, or body facts that require Auto's
  provider classifier, an MCP connection, mutable hooks, compaction, or queued
  runtime injections remain typed unavailable. The manifest reports exact
  primary role/lane identity, upstream route-source provenance, requested and
  effective reasoning, canonical JSON sizes, conservative offline estimates,
  and provider-reported usage as unavailable because no request ran. It never
  adds a model-visible tool, sends a provider call, or prints prompt, message,
  credential, endpoint-path, or workspace-path content. The explicit
  `/preview-request base-prompt` mode prints only the exact effective base
  prompt; effective system text remains protected behind its final hash. The
  exact body includes the same authoritative transient Work/To-do tail used by
  production, including graph-backed state newer than the legacy projection.
  Preflight preserves production's separately framed base-plus-Work estimate,
  and fails closed when the authoritative projection is unavailable. An
  exhausted active goal token budget also produces a typed unavailable result
  before any outbound request is built.
  (#1004, #3928; dry-run concept harvested from PR #1099 by @GTC2080 / TaoMu.)

- Slash commands, hotbar actions, and CLI entrypoints for the same Lane/Fleet
  lifecycle operation now share one typed control-plane contract
  (`codewhale-lane::control`): a stable `<domain>.<verb>` id, read-vs-write
  authority, persistence scope, exact-identity target selection, retryability,
  lifecycle outcome, and one bounded, sanitized receipt. `docs/COMMAND_CONTROL_PLANE.md`
  documents it (#1888).

- `/lane [list|status|interrupt|restart|resume]` — durable Lane control from the
  composer, backed by the same executor `codewhale lane …` calls. `codewhale
  lane interrupt|restart|resume` are the matching CLI verbs; `lane stop` stays as
  a compatibility spelling of `lane interrupt`. Appending `@<lifecycle-seq>` to a
  lane id fences a write to the exact durable generation you observed, so a
  concurrent transition is rejected as a conflict rather than acted on (#1888).

- `codewhale fleet list` and `/fleet [list|status|interrupt|resume]` — durable
  Fleet run inspection and control from either surface, through shared DTOs that
  carry the exact provider, provider-table id, model, effective reasoning tier,
  and route source when the ledger records them, and a typed `not_recorded` /
  `not_applicable` / `redacted` reason when it does not. Requested-vs-effective
  reasoning is never back-filled: the ledger persists the effective tier only, so
  the requested tier reports `not_recorded` (#4022).

- The bundled skill pack now ships a `help` skill (catalog generation 7). It is
  `invocation: explicit-only`, so it never enters the model's ambient catalogue
  and costs no prompt budget. Its body is a routing card that points at the
  surfaces this build actually exposes — `/help` and `/help <command>`,
  `/skills` and `/skills inspect`, `/config`, `doctor`, and the `docs/` tree
  when the workspace is a Codewhale checkout — and explicitly forbids pasting a
  command list or settings table into context (#4698).

- `crates/tui/assets/skills-catalog-matrix.json`: an authored, provider-free
  expectation matrix covering every bundled skill (tier, invocation, aliases,
  ambient-catalogue eligibility, shadowed aliases). Contract tests in
  `crates/tui/src/skills/catalog_matrix.rs` assert a bijection between the
  fixture and the shipped bundle, so the starter pack cannot change without an
  explicit fixture update. The matrix covers positive eligibility and explicit
  load, non-activation negatives, alias resolution, explicit-only exclusion,
  alias-vs-canonical collision precedence, and prompt-budget invariants (no
  duplicate catalogue entries, no aliases as extra entries, and the shipped
  pack fitting inside the 12 000-char budget with no omitted-skills line). These
  are deterministic registry/catalog/resolver assertions and make no claim about
  semantic model routing (#4698).

- Locale-routing coverage for the complete bundled catalog across every shipped
  locale (`en`, `ja`, `zh-Hans`, `zh-Hant`, `pt-BR`, `es-419`, `vi`, `ko`). No
  bundled skill ships a localized routing description and none was invented;
  the tested contract is deterministic fallback to the canonical English
  description, with the rendered catalogue byte-identical across locales.
  Exact-tag match, primary-subtag fallback, and English fallback are covered
  against a synthetic authored fixture, and the parity test fails if a bundled
  skill ever gains localized metadata without source-backed coverage (#4698).

- `docs/LIVE_SMOKE.md`: copy-pasteable, opt-in live-smoke instructions for Kimi
  K3 and a second provider/model (DeepSeek). The runs are manual only — nothing
  in CI, tests, or skills invokes them. They use `env -i` plus a throwaway
  `CODEWHALE_HOME`; `HOME` is left unset rather than repurposed, and ambient
  provider variables are not forwarded. The operator names the credential
  variable explicitly, and the isolated child reads its value with echo off,
  restores the prior terminal state on exit or interruption, and never persists
  the value or puts it in a command argument. The page states the expected
  route/model/reasoning/tool receipt fields while treating provider errors as
  unclassified until provider configuration, authentication/entitlement, and
  harness behavior have been corroborated independently (#4698).

- Approval cards can now remember eligible safe shell and file-write approvals
  as exact `allow` rules scoped to the current repository. Remembered shell
  commands use complete-command matching, validated file and patch paths remain
  workspace-relative, and dangerous, critical, or repo-law-held requests stay
  ineligible and continue to require review.

- `tui.header_items` (array of strings, optional, default `[]`): an opt-in
  header chip showing cumulative session token usage as input / cache-hit /
  output. Set `header_items = ["tokens"]` under `[tui]` to enable it. The
  chip is the only elidable element of the header — the git label, context
  meter, and version stamp keep their space, and narrow terminals drop the
  chip rather than the baseline chrome. Unknown entries are warned about and
  skipped so configs written by newer builds stay loadable by older ones
  (#4520 requested by @eugenicum; PR #4610 by @XhesicaFrost, harvested with
  co-authorship).

- `thinking_default_expanded` lets reasoning blocks start open while keeping
  Space as the per-block toggle. The setting is persisted, available through
  native and runtime configuration, and documented for SSH/tmux accessibility
  (issue #4925 and PR #4928 by @M-Maciej).

- The transcript renders a conservative subset of LaTeX math as readable
  Unicode without rewriting fenced/inline code, ordinary currency, escaped
  dollars, or unknown commands. The contribution from PR #4973 by
  @SparkofSpike was hardened and landed through PR #4974; reported by
  @antarikshraya in #4957.

- Session control now includes a sessions rail, shared archive projection,
  picker archive controls, and opt-in interactive auto-resume with explicit
  handoff behavior. The work closes the remaining session-browsing direction
  from #2934; thanks @cy2311 for the original report.

- The bundled contributor-onboarding skill can sync contribution context,
  select the appropriate gate, and prepare a digest without inflating the
  ambient skill catalog. It follows the contributor-navigation request in
  #4227 by @JayBeest.

- Bahasa Indonesia now has a complete repository documentation suite and a
  registered website dictionary alongside the shipped TUI locale (PRs #4962
  and #4972 by @atmosuwiryo, closing #4789).

- Reasoning content can keep its rail, italics, cursor, and expansion controls
  while disabling only the warm background highlight. The independent setting
  is persisted and localized (#4089; reported by @elijahchan2019).

- StepFun setup now asks whether a key belongs to PAYG or Step Plan, keeps the
  two endpoint/billing routes distinct, and localizes the choice across the
  complete packs (#4526; reported by @whp233).

- OpenCode Zen is a separate model-aware API-key provider. Its curated catalog
  selects Responses, Anthropic Messages, or Chat Completions per model;
  unsupported Gemini and unknown models fail closed, and missing Zen
  credentials never fall through to ChatGPT/Codex OAuth guidance. The
  implementation from closed PR #4467 by @snail-vs (snailoniu) is preserved in
  the candidate.

- Markdown exports correlate prompts with stable workspace restore-point ids
  and say when correlation is unavailable or ambiguous, completing the
  remaining restoration/export direction from #2494 by @wywsoor.

### Fixed

- Permission setup now consistently presents the product postures Ask,
  Auto-Review, and Full Access instead of leaking the internal `never` token.
  The same resolved sandbox policy now drives execution and UI receipts: Plan
  stays read-only, Ask and Auto-Review stay workspace-scoped, and Full Access
  is actually unsandboxed unless a stricter effective configuration wins.

- Fleet setup no longer stalls when a user explicitly selects a configured
  Codex or Grok external-consent route. The selected route is activated and
  validated before saving, roster roles open directly on their Model step,
  Review saves on the first Enter, and new profiles default to the personal
  profile directory that the roster loads on the next session.

- Provider credential dialogs now share one wrapping, secret-safe API-key
  surface across every non-OAuth provider. A key already present in durable
  storage is reported as configured without rendering it, typing or pasting is
  clearly framed as replacement, narrow help text remains visible, and Codex
  and Grok OAuth flows remain token-free in this modal.

- Ctrl+O again opens the complete recorded reasoning detail for the selected,
  active, or latest reasoning block. The whole-turn Turn Inspector moved to
  Ctrl+Alt+O and `/turn inspect`, removing the shortcut collision while keeping
  raw leaf detail and post-flush reasoning discoverable.

- Failed child agents now deliver a distinct high-priority failure receipt to
  their owning parent with a sanitized failure class, elapsed work, and a full
  transcript handle. Parent-to-child message, follow-up, and interrupt tools
  now use one hierarchy-checked mailbox path, and persisted nested completion
  envelopes remain safely restorable across instruction-text revisions.

- Background-shell completion events now carry only bounded tails plus a
  retrievable exact-evidence handle. Terminal foreground Bash results are
  acknowledged at the direct tool-result boundary and are no longer emitted a
  second time as background completion artifacts.

- Providerless Fleet and child-agent fixed-model routes now reject only
  high-confidence foreign-provider model ids before creating a worktree, while
  explicit provider/model pairs, custom and local endpoints, unknown ids, and
  aggregator wire-id resolution retain their intended behavior.

- Manual compaction now preserves and reports the supplied provider failure
  class instead of replacing it with an opaque generic error. This does not
  infer quota exhaustion when the recorded failure does not prove it.

- The ambient jellyfish keep complete, readable silhouettes while animating,
  and the website favicon now uses the Signal Current desktop tile instead of
  the legacy whale mark.

- ACP JSON-RPC responses preserve numeric request ids for avante.nvim while
  retaining the negotiated string-id exception for Zed (PR #4929 by
  @atmosuwiryo).

- Restored shell cells whose job no longer exists stop displaying live
  spinners and settle into a truthful stale/no-output state across transcript,
  phase strip, and sidebar (PR #4937 by @LI-Jialu, closing #4547).

- Interrupted checkpoints and timed recovery snapshots remain checkpoints
  instead of being promoted into orphan session files, preventing duplicate
  `/resume` entries (PR #4963 by @SparkofSpike).

- Every shipped locale is admitted by the typed settings schema and native
  chooser, with complete/partial status kept independent and tested (PR #4856
  by @nightt5879, closing #4786). Context-menu hover hit-testing also accounts
  for its title row (PR #4897 by @XhesicaFrost; reported by @SparkofSpike in
  #4803).

- OSC 52 and SSH/tmux clipboard transport run on one bounded background worker
  rather than blocking input and rendering on the TUI loop; late transport
  failures still surface through the status path (PR #4896 by @nightt5879,
  closing #4159).

- Non-streaming model calls receive a generation-length response budget rather
  than the SSE header-open timeout, while actual SSE opens share the bounded
  cross-provider transport seam. The equivalent fix direction came from
  closed PR #4743 by @vibecoding-skills.

- Resumed sessions diagnose a deleted inherited workspace before shell launch
  instead of failing as an opaque Windows process error (report #4100 by
  @redjade75723). DeepSeek native tool-call wrapper tokens are also scrubbed
  from visible streaming and completed output as a grounded fail-soft follow-up
  to report #3880 by @hardy922; that report's exact emitted marker remained
  unconfirmed.

- Auto model routing now preserves the user's requested reasoning effort
  through startup, provider/model changes, session restore, the picker,
  Ctrl+T, and Hotbar actions. The tier is normalized only after the concrete
  provider route is known instead of being silently replaced by Auto
  (#4941, PR #4961 by @nightt5879).

- Auto-compaction now defaults on for every known model context window,
  including Kimi K3's 1,048,576-token routes. Persisting only an
  `auto_compact_threshold` or `auto_compact_threshold_percent` now counts as
  opt-in intent, while an explicit `auto_compact = false` remains authoritative.
  `/config` reports the effective state, percentage, and computed token trigger.

- Per-provider `context_window` overrides are now documented and visible in
  `/config`, provider setup help, diagnostics, and the example configuration.
  The effective override consistently drives preflight budgeting, the context
  meter, and compaction; this lets a user cap a 1M Kimi route to 256K when their
  Coding Plan tier has the smaller window.

- Agent Details now projects status, model, elapsed time, and step counts from
  the same row snapshot as the primary agents list, eliminating contradictory
  worker state between the two surfaces.

- Composer submission and its hints now share one state machine. Portable
  terminals use Enter to queue during a running turn and Enter again to steer;
  Ctrl/Cmd+Enter is accepted only when an enhanced terminal reports it and is
  no longer advertised as universally available.

- `edit_file` now matches LF-only model search text against CRLF files,
  preserves the file's line-ending style for replacement text, and still
  rejects newline-normalized duplicate matches as non-unique (#4764).
  Implemented in PR #4942 by @nightt5879; reported and root-caused by
  @LmeSzinc.

- `/fleet status` read the current TUI session's sub-agents while `codewhale
  fleet status` read the durable `.codewhale/fleet.jsonl` ledger — two different
  things wearing one name, so a run started by `codewhale fleet run` never
  appeared in the TUI. `/fleet status` now reads the durable ledger through the
  same code path as the CLI; the session view keeps its own name as
  `/fleet workers` (`/subagents` and `n` still work). When a workspace has no
  ledger, both surfaces report a typed `no_fleet_ledger` reason instead of an
  empty-looking "all clear", and neither creates the ledger as a side effect of
  reading it (#4022).

- `codewhale fleet status` (and `list`/`interrupt`/`resume`) created
  `.codewhale/fleet.jsonl` as a side effect of opening the manager, then
  reported `no_fleet_ledger` for the file it had just made — so the second
  invocation showed an empty Fleet where none existed. The CLI now refuses
  those verbs before the manager is constructed, matching `/fleet` (#4022).

- `fleet resume <run-id>` accepted any string. An id absent from the ledger
  reconciled nothing but still wrote a run-status record keyed by whatever was
  typed, and reported `no_change`. Unknown ids are now refused as `not_found`
  before any durable write (#4022).

- `lane interrupt` reported `transitioned` even when it changed nothing —
  another process's stop looked like our own. The Runtime backend now reports
  whether *this* call performed the transition, and a no-op is `no_change`.
  The `@<lifecycle-seq>` fence is also evaluated inside the registry's per-Lane
  lock rather than before it, so a stale fence refuses without running Runtime
  teardown instead of racing between the check and the stop (#1888).

- `/lane` no longer runs Runtime teardown on the TUI composer thread. Reads on
  the slash surface skip reconciliation (which probes tmux and takes a lock)
  and say so on the receipt instead of implying freshness; `lane interrupt` is
  CLI-only until that work runs off-thread, and reports
  `surface_not_supported` naming `codewhale lane interrupt` (#4022).

- The hotbar is no longer modelled as a third control surface. A slot binds a
  slash command and fires it with no argument, so it runs *as* the slash
  surface; the contract now declares which verb a bare press actually reaches
  (`hotbar_bare_dispatch`, true only for `lane.list`) instead of advertising
  target-taking verbs as hotbar-reachable (#1888).

- `codewhale lane list --json` and `lane status --json` keep emitting the
  `LaneRecord` shape they always have — the receipt did not replace it. The
  human `lane status` output also regained `branch`, `session`, `socket`,
  `attach`, and `log`, which the first cut of the shared DTO had dropped
  (#1888).

- No surface advertises a backend it does not have. `lane restart` and
  `lane resume` have no implementation — a Lane is re-created by
  `codewhale lane start`, and a stopped Lane's Runtime session is gone — so all
  three surfaces refuse them with `backend_not_implemented` and say why.
  `fleet restart` drives the manager loop to completion, which only the CLI
  runs, so `/fleet restart` reports `surface_not_supported` and names the CLI
  command rather than quietly doing a smaller thing (#1888).

- Deny rules in `permissions.toml` no longer miss a command because of an
  intervening flag: deny matching is token-based with flag-skipping and
  backtracking, so a `git push` rule still catches
  `git -c foo=bar push`. Path matching folds case only on platforms whose
  filesystems are case-insensitive, and the default approval branch no
  longer proposes the working directory as a network host.

- MCP tool calls run once. A failed call is no longer retried as if it
  were a failed lookup, qualified-name resolution collects every match and
  reports an ambiguity instead of taking whichever the hash map yielded
  first, and registering a server whose name collides with an existing one
  after sanitization is now an error rather than a silent overwrite.
  The equivalent call-once fix direction came from closed PR #4756 by
  @adity982.

- The session index survives a torn line: an unparseable entry is skipped
  rather than aborting the whole read, appends carry their data through to
  disk, and appends and compaction share a lock so a compaction can no
  longer race an append into a lost record.

- `Edit` counts as a write tool for workflow elevation, and the TUI's
  write/shell classification now delegates to one shared allowlist rather
  than keeping a second copy that could drift.

- A rejected `app/config/set` stays a no-op. Previously an invalid value
  still tore down the cached runtime bridge, killing the child runtime and
  orphaning every other in-flight stdio thread behind a response that
  correctly reported failure.

- A malformed project `config.toml` is no longer indistinguishable from
  having no project config. Because a project config may only *tighten*
  approval and sandbox policy, silently discarding a broken one dropped a
  repository's restrictions back to the looser user defaults; the setup
  wizard now says so, naming the file but never quoting its contents.

- An expired lane worktree no longer leaves its branch behind, which made
  reusing the same lane name fail with "branch already exists". A branch
  still carrying unmerged commits is kept — a TTL lapsing is not consent
  to delete someone's work.

- An in-flight `thread/message` turn can be stopped. The stdio loop keeps
  reading while a turn streams, so the new `thread/interrupt` request (and
  `shutdown`) can reach a runaway turn instead of waiting on the very turn
  they were meant to stop.

- Precedence is stated only in the constitution's "Whose word wins" section.
  Memory hygiene no longer ships an inverted Tier list that put the
  constitution above the user's current request; approval, compaction, and
  personality overlays describe behavior without rank vocabulary; and the
  authority recap points at the single source rather than restating a second
  ladder.

- `<turn_meta>` carries facts (mode, posture, model, workspace), not mode
  doctrine or permission-question essays re-asserted every user message.

- The project context pack (pretty-printed workspace tree) is off by default
  and opt-in via `[context] project_pack = true`. Language law is compressed
  while keeping the English-constitution / user-language-reply contract.

- Modal lists and config pickers wrap selection at both ends (Down past the
  last row returns to the top). Home-directory resolution prefers
  `HOME`/`USERPROFILE` via `effective_home_dir` across remaining call sites so
  Windows tests that fake the home env vars match production paths. The
  equivalent home-directory sweep came from closed PR #4760 by
  @EvanProgramming.

### Changed

- Prefix-cache tool catalog entries store only the SHA-256 digest, not the
  joined catalog string. Unused plan-transition validation helpers are removed.

- Settings sections now hold only what they claim (#4751). Fleet keeps
  Fleet/member concerns; `/goal` moved to a **Session** section and Workflow
  orchestration to its own **Workflow** section. The inert DeepSeek-only
  `default_model` fallback moved out of Model settings into an explicit
  **Legacy** section — exact-Fleet users switch Fleets, not fallback models;
  the config field is retained because the runtime still reads it. This is
  presentation only: the persisted keys (`goal_command`, `workflow`,
  `default_model`), their values, scopes, and runtime behavior are unchanged.

- Auto model routing is scoped to the active provider. The classifier
  inventory no longer discloses other providers' runnable routes (or the fact
  that their credentials exist), a classifier reply naming another provider is
  refused, the local heuristic no longer falls back to a different provider
  when the active one is unusable, and the implicit DeepSeek-flash classifier
  is skipped for non-DeepSeek sessions. Auto receipts and the model picker
  hint report the active-provider-only scope instead of "runnable providers".
  Cross-provider Auto is available only through the persisted `[auto]
  cross_provider = true` opt-in (an explicit `[auto.router]` route remains its
  own opt-in for the classifier call). Same-provider strong/fast selection and
  `[auto] cost_saving` are unchanged.

- The QA pseudo-terminal acceptance harness now parses frames with `rio-vt`
  behind its existing neutral frame/color surface, retaining the assertions
  while removing the `vt100` dependency (PR #4931 by @raphamorim).

- Anthropic Messages and OpenAI Responses stream opening now share the
  `client/stream_entry.rs` seam already used by Chat Completions: one bounded
  response-header wait, shared dual/HTTP-1.1 policy selection, at most one
  HTTP/1.1 fallback on a classified HTTP/2 header stall, and common idle-timeout
  diagnostics. Wire-specific authentication, headers, endpoints, decoding, and
  rate-limit behavior remain at each adapter edge. The timeout-placement
  diagnosis and fix direction came from closed PR #4743 by
  @vibecoding-skills.

### Security

- Release containers now publish an SBOM attestation and pin maximum-mode
  provenance explicitly so supply-chain metadata cannot silently weaken with a
  builder-default change (PR #4958 by @kobihikri).

### Contributors

Thank you to the contributors whose code, reports, and reviews shaped v0.9.2:

- [@greyfreedom](https://github.com/greyfreedom) — exact repository-scoped
  allow grants and cross-platform path semantics (PR #4761), plus safe
  permission-rule listing and snapshot-bound removal (PR #4960).
- [@nightt5879](https://github.com/nightt5879) — off-event-loop clipboard
  writes (PR #4896), complete locale exposure in settings (PR #4856), CRLF-safe
  edits (PR #4942), and reasoning-effort preservation across automatic model
  routing (PR #4961).
- [@XhesicaFrost](https://github.com/XhesicaFrost) — the configurable
  session-token header (PR #4610) and context-menu hover alignment (PR #4897).
- [@cyq1017](https://github.com/cyq1017) — the hooks configuration/executor
  split from PR #4087.
- [@snail-vs](https://github.com/snail-vs) (snailoniu) — OpenCode Zen's
  model-aware routes, authentication, documentation, and test isolation from
  closed PR #4467, whose contributor commits are preserved in the candidate.
- [@SparkofSpike](https://github.com/SparkofSpike) — the zh-Hans translation
  quality review harvested from PR #4908, duplicate-session fix in PR #4963,
  LaTeX implementation from PR #4973 landed through #4974, and the context-menu
  reproduction in #4803.
- [@GTC2080](https://github.com/GTC2080) — the request-preview concept from
  PR #1099.
- [@h3c-hexin](https://github.com/h3c-hexin) — non-UTF-8 `fetch_url`
  decoding direction from PR #4909.
- [@fleitz](https://github.com/fleitz) — required source-candidate credit for
  the canonical `Bash` no-`cwd` workspace fix and regression in PR #4673
  (issue #4674).
- [@LmeSzinc](https://github.com/LmeSzinc) — the Windows CRLF `edit_file`
  reproduction, root-cause analysis, and affected-code anchors in issue #4764.
- [@atmosuwiryo](https://github.com/atmosuwiryo) — ACP numeric-id compatibility
  (PR #4929) and the Indonesian documentation and website locale (PRs #4962
  and #4972).
- [@M-Maciej](https://github.com/M-Maciej) — the expanded-by-default reasoning
  setting and its original report (PR #4928, issue #4925).
- [@raphamorim](https://github.com/raphamorim) — migration of the QA PTY frame
  parser to `rio-vt` (PR #4931).
- [@LI-Jialu](https://github.com/LI-Jialu) — truthful finalization of restored
  stale shell cells (PR #4937).
- [@kobihikri](https://github.com/kobihikri) — release-container SBOM and
  explicit provenance mode (PR #4958).
- [@EvanProgramming](https://github.com/EvanProgramming),
  [@adity982](https://github.com/adity982), and
  [@vibecoding-skills](https://github.com/vibecoding-skills) — equivalent fix
  direction for the effective-home sweep (#4760), MCP call-once behavior
  (#4756), and streaming/non-streaming timeout split (#4743).
- [@antarikshraya](https://github.com/antarikshraya) — the LaTeX transcript
  rendering report in #4957.
- [@eugenicum](https://github.com/eugenicum) — the token-header request and
  output-presentation measurements in #4520 and #4468.
- [@whp233](https://github.com/whp233) — the StepFun/OpenCode subscription-route
  request in #4526.
- [@redjade75723](https://github.com/redjade75723),
  [@hardy922](https://github.com/hardy922),
  [@JayBeest](https://github.com/JayBeest),
  [@elijahchan2019](https://github.com/elijahchan2019),
  [@cy2311](https://github.com/cy2311), and
  [@wywsoor](https://github.com/wywsoor) — reports and product direction behind
  the stale-workspace diagnosis (#4100), native-tool-token filtering (#3880),
  contributor onboarding (#4227), optional reasoning highlight (#4089),
  session control (#2934), and export/restore correlation (#2494).

---

Older releases: [CHANGELOG.md](https://github.com/codewhale-hq/CodeWhale/blob/main/CHANGELOG.md) and [docs/CHANGELOG_ARCHIVE.md](https://github.com/codewhale-hq/CodeWhale/blob/main/docs/CHANGELOG_ARCHIVE.md).
