# Opt-in live smoke runs

> 阅读简体中文版：[zh_hans/LIVE_SMOKE.md](zh_hans/LIVE_SMOKE.md)。

This page is **manual, opt-in, and never automated.** Nothing in CI, no test,
no build script, and no skill runs these commands. The repository's automated
suite is provider-free by design; see
[`crates/tui/assets/skills-catalog-matrix.json`](../crates/tui/assets/skills-catalog-matrix.json)
and the catalog-matrix tests for what is actually asserted without a provider.

Run this only when you want to answer one narrow question: *does a real route
to a real model return a well-formed receipt on this machine?*

## What a live smoke run does and does not prove

| Question | Answered here? |
| --- | --- |
| Does the receipt record the provider/model I asked for? | Yes. This does not by itself prove which network endpoint handled the request. |
| Does the run emit an inspectable route/usage receipt? | Yes, when the harness reaches that stage. |
| Does one response prove my account's entitlement state? | **No.** Provider configuration, authentication/entitlement, and harness behavior remain candidate causes until corroborated. |
| Does the model semantically pick the right skill? | **No.** Not measured. |
| Is skill registry/catalog/alias behavior correct? | **No** — that is the provider-free suite's job. |

Treat these as investigation starting points, not proven failure classes:

- **Provider error response** — HTTP 401/403, unknown-model, quota, or region
  errors can reflect the configured provider/endpoint, credential
  authentication or entitlement, provider availability, or a harness
  routing/request defect. The response alone does not distinguish them.
- **Receipt or process anomaly** — wrong `provider`/`model` in the receipt,
  missing receipt fields, a crash, or failure to use the isolated state
  directory is evidence to investigate the harness, but still needs a minimal
  reproduction or other corroboration before assigning the cause.

## Isolation rules these snippets follow

1. `env -i` clears the inherited environment, so your ambient `HOME`,
   `CODEWHALE_HOME`, and `*_API_KEY` values are not forwarded. Only variables
   listed explicitly on the `env` line survive.
2. Only `CODEWHALE_HOME` points at the task-specific throwaway directory, so
   Codewhale config, sessions, and the bundled skill install land in scratch
   state. `HOME` is intentionally left unset; the smoke run never repurposes it.
3. You name the credential variable yourself (`CW_SMOKE_CRED_VAR`). Nothing is
   guessed from the provider.
4. The isolated child reads the secret with echo disabled, restores the prior
   terminal state on `EXIT`, `INT`, `HUP`, or `TERM`, and exports it only in
   that child. The value is not persisted to disk or placed in a command
   argument or shell history.
5. `PATH` is forwarded explicitly, and is the only host variable carried over.

Portable `sh` is used throughout; `stty` and `mktemp -d` are the only non-POSIX
niceties and both exist on macOS and mainstream Linux.

## Step 1 — create the throwaway state (both runs)

```sh
CW_SMOKE_CODEWHALE_HOME="$(mktemp -d)" || exit 1
mkdir -p "$CW_SMOKE_CODEWHALE_HOME/tmp"
echo "scratch Codewhale state: $CW_SMOKE_CODEWHALE_HOME"
```

## Step 2 — name the credential variable

`CW_SMOKE_CRED_VAR` must be the variable name the provider expects. Codewhale
reads `MOONSHOT_API_KEY` (or `KIMI_API_KEY`) for the Moonshot/Kimi route and
`DEEPSEEK_API_KEY` for the DeepSeek route.

```sh
CW_SMOKE_CRED_VAR="MOONSHOT_API_KEY"     # you choose this; nothing is inferred
```

The run command prompts for the value inside its isolated child process. It
does not create a credential file.

## Step 3a — run A: Kimi K3

`kimi-k3` is a model id this build knows about. The configured provider and its
resolved endpoint determine the route: `--provider moonshot` selects the
configured Moonshot route; selecting `opencode_go` would select that separately
configured route. The account does not choose between them, and the harness
does not switch between them based on a response. Set
`CW_SMOKE_PROVIDER` / `CW_SMOKE_MODEL` for the route you intend to exercise. A
model-not-found response is an unclassified result until the provider/endpoint
configuration, credential access, and harness request are corroborated.

```sh
CW_SMOKE_PROVIDER="moonshot"
CW_SMOKE_MODEL="kimi-k3"
CW_SMOKE_EFFORT="medium"
CW_SMOKE_PROMPT="Reply with exactly: SMOKE OK"

env -i \
  PATH="$PATH" \
  TMPDIR="$CW_SMOKE_CODEWHALE_HOME/tmp" \
  CODEWHALE_HOME="$CW_SMOKE_CODEWHALE_HOME" \
  CW_SMOKE_CRED_VAR="$CW_SMOKE_CRED_VAR" \
  sh -c '
    CW_SMOKE_STTY_STATE="$(stty -g)" || exit 1
    restore_terminal() {
      stty "$CW_SMOKE_STTY_STATE" 2>/dev/null || :
    }
    trap "restore_terminal" EXIT
    trap "restore_terminal; exit 129" HUP
    trap "restore_terminal; exit 130" INT
    trap "restore_terminal; exit 143" TERM

    printf "Paste value for %s (input hidden): " "$CW_SMOKE_CRED_VAR" >&2
    stty -echo || exit 1
    if ! IFS= read -r CW_SMOKE_CRED; then
      printf "\nCredential input failed.\n" >&2
      exit 1
    fi
    restore_terminal
    trap - EXIT HUP INT TERM
    unset CW_SMOKE_STTY_STATE
    printf "\n" >&2

    export "$CW_SMOKE_CRED_VAR=$CW_SMOKE_CRED"
    unset CW_SMOKE_CRED
    exec codewhale exec \
      --provider "$1" --model "$2" --reasoning-effort "$3" --json "$4"
  ' sh "$CW_SMOKE_PROVIDER" "$CW_SMOKE_MODEL" "$CW_SMOKE_EFFORT" "$CW_SMOKE_PROMPT"
```

## Step 3b — run B: a second provider/model (DeepSeek)

Set `CW_SMOKE_CRED_VAR="DEEPSEEK_API_KEY"`, then:

```sh
CW_SMOKE_PROVIDER="deepseek"
CW_SMOKE_MODEL="deepseek-v4-pro"
```

…and re-run the identical `env -i …` block from step 3a; it prompts for a fresh
credential value. Running the *same* command shape against two providers is the
point: a difference in outcome is an observation to investigate, not proof of
route, entitlement, or harness correctness. Provider/endpoint configuration,
credentials, provider health, and the generated request all remain possible
explanations.

## Step 4 — optional: tool-and-reasoning receipt

The `--json` one-shot above records the resolved route claimed by the harness;
it does not independently prove which endpoint handled the request. To also see
tool-catalog and reasoning receipts, use the streaming form (still inside the
same `env -i` wrapper, substituting the `exec` line):

```sh
    exec codewhale exec --auto --max-turns 3 \
      --output-format stream-json \
      --provider "$1" --model "$2" --reasoning-effort "$3" "$4"
```

## Step 5 — what to record

From the `--json` one-shot receipt:

| Field | Expectation |
| --- | --- |
| `mode` | `one-shot` |
| `provider` | exactly the `--provider` you passed |
| `model` | exactly the `--model` you passed |
| `success` | `true` |
| `output` | the model's text; content is *not* a pass/fail criterion |

From the `stream-json` metadata receipt:

| Field | Expectation |
| --- | --- |
| `provider`, `model` | match the flags you passed |
| `route_source` | records *why* that route was chosen |
| `reasoning_tokens` | present when the receipt reports reasoning; absence can reflect model/provider behavior, configuration, or a harness omission and needs corroboration |
| `tool_catalog_sha256` | present when a tool surface was offered |
| `approval_posture` | `auto_tools` for the `--auto` command above |
| `sandbox_posture` | the policy and its enforcement: Full Access by default with `--auto`; restrictive policies say `local OS sandbox applied` or `policy only; no execution sandbox available`; external execution backends identify filesystem/network isolation as unverified by Codewhale |
| `duration_ms`, `input_tokens`, `output_tokens` | present for a completed run |

Report the receipt fields. Do **not** paste the credential, the key file, or
raw provider error bodies (they can echo request headers).

## Step 6 — clean up

```sh
rm -rf "$CW_SMOKE_CODEWHALE_HOME"
unset CW_SMOKE_CODEWHALE_HOME CW_SMOKE_CRED_VAR \
      CW_SMOKE_PROVIDER CW_SMOKE_MODEL CW_SMOKE_EFFORT CW_SMOKE_PROMPT
```

## Scope note

A green live smoke run is evidence that the configured live attempt completed
today. By itself it does not prove endpoint identity, durable account
entitlement, or the absence of a harness defect; corroborate those claims
separately. It says nothing about skill selection, alias resolution, locale
routing, or prompt budget — all of which are covered deterministically and
provider-free in `crates/skills/src/catalog_matrix.rs`.
