# Fleet rebuild decisions (2026-08-04)

Owner-steered requirements captured this session. Evidence-first: every claim
below is anchored to code or a user statement; nothing is aspiration.

## 1. Fleets are saved configurations (not shadowed role files)

Today two parallel stores exist:
- per-role profile files (`~/.codewhale/agents/*.toml`, `.codewhale/agents/*.toml`,
  `[fleet.profiles]`, built-ins) merged by id with `ShadowedProfile` receipts
  (crates/engine/src/fleet/roster.rs) — the "⚠ shadow" pile the UI must not show;
- named fleet files (`fleets/<name>.toml` at `$CODEWHALE_HOME` or workspace)
  consumed by the workflow crate (`crates/workflow/src/named_fleet.rs`,
  `fleet_exact.rs`) with `QualifiedFleetId` ambiguity rejection.

**Decision:** one concept — a named Fleet file. A Fleet contains:
- `name` (editable display name), `description`;
- `operator`: provider + exact model + reasoning (absent = inherit session route);
- `members`: role → model pin or inherit, provider (pins only, never inferred),
  reasoning (only when the resolved route supports it), instructions,
  capability requirements (e.g. `vision`);
- scope + source: personal (`$CODEWHALE_HOME/fleets/`) or workspace
  (`.codewhale/fleets/`), the exact file path shown in the UI.

New schema kind `schema = "fleet"` v2 owned by the TUI crate. The workflow
crate's `exact`/legacy parsers stay untouched; binding a v2 file to a Workflow
fails closed with "unknown fleet schema" until workflows are decoupled.

## 2. Selection: personal default + explicit workspace selection

- `$CODEWHALE_HOME/fleets/selected` — user-global default Fleet (plain text name).
- `.codewhale/fleets/selected` — workspace selection, intentional and labeled.
- Resolution: workspace selection wins, then personal, then none (legacy roster).
- A new session starts on the selected Fleet's operator route unless CLI/env
  override exists.
- Workspace config never silently shadows a personal Fleet: both remain listed,
  scopes labeled, ambiguity surfaced (qualified `origin/name`).

## 3. Session route changes are temporary by default

`/model` and `/provider` change only the live session route. Persistence is an
explicit choice: Update this Fleet / Save as a new Fleet / Save as my default
(no fleet selected) / Keep for session only. Never silently rewrite a saved
Fleet or the config file after an in-session route change. Every save receipt
names the exact file and scope it changed.

## 4. Scout replaces "faster"

One visible fast exploratory role: Scout. `faster`/`model_strength`/`Fast`
loadout disappear from pickers, tool schemas, and Fleet config. Legacy parsing
survives for compatibility and migrates to the Scout policy with a receipt.
Unpinned Scout may receive a suggested fast companion from explicit catalog
metadata (provider/family → verified offering), never name-guessing; the exact
resolved Scout provider/model is shown before a run; explicit Scout pins win
and survive operator changes; no verified companion → show that clearly or
inherit deliberately.

## 5. Provider → model family → exact model picker

The flat model list (model_picker.rs) becomes provider → family → exact model.
Each row truthfully shows provider + exact route id, readiness, explicit Fleet
role recommendation, input modalities (vision), tool + structured-output
support, reasoning levels, context/output limits. Compact by default; endpoint/
cost/advanced behind disclosure. No `max`/`ultra` unless genuinely supported.
Capability-aware routing: vision work never silently goes to a non-vision
member.

## 6. Config/credential scope: user-global credentials, layered selection

Verified: credentials/keychain/OAuth are already user-global
(crates/secrets, ~/.codex/auth.json, ~/.grok/auth.json,
$CODEWHALE_HOME/credentials/*). The "authorized here, locked there" failure is
a config-file-selection problem: an explicit --config/CODEWHALE_CONFIG_PATH
pointing at a workspace file makes THAT file the loaded config, and the
readiness gate (`provider_is_configured_for_active`) can then fail on a
credential that exists user-globally. Fix: readiness consults the user-global
credential sources regardless of which config file is loaded; every active
route shows its source (provider/model, credential source, config layer,
temporary vs saved, precise unavailable reason).

## 7. Members/roles pin to folders or users

Fleet save scope is either user (personal) or folder (workspace); both are
always listed with scope + source labels; same-name fleets in two scopes are
distinct and never silently shadow — the UI surfaces the ambiguity and the
user picks a qualified origin/name.

## 8. Workflows will be simplified toward kimicode-swarm/grokbuild-style
## orchestration (no fleet-file dependency)

Research lane open (scout dispatched). Workflow runs should need only roles +
session route, with a default roster from built-ins; fleet files become an
optional pin layer, not a requirement. Do not redesign workflows in this
session beyond the research; the fleet rebuild must not deepen the coupling.

## 9. Copy is truthful

Every label, receipt, status, and confirmation states only what the code can
prove: save receipts name the exact file changed; readiness states carry a
precise reason; "saved"/"selected"/"authorized" are never claimed without the
underlying write/credential check having succeeded.
