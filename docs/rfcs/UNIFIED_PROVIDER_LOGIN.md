# RFC: Unified provider login (`codewhale auth login`)

**Status:** Future RFC — approved direction (maintainer, 2026-07-12);
implementation is deferred beyond v0.9.0.
**Owner seams verified against:** v0.9.0 candidate tree.

## Decision

One login surface, one grammar, for every provider that offers an
interactive auth flow:

```
codewhale auth login --provider <anthropic|openai-codex|xai|...>
/auth <provider>
```

Today the tree has one interactive flow — xAI device-code OAuth
(`crates/tui/src/xai_oauth.rs`, `codewhale auth xai-device`, `/auth
xai-device`, #4257) — shipped as a provider-specific command. That shape
does not scale: Anthropic (Claude Pro/Max browser OAuth) and OpenAI Codex
OAuth are next, and each provider growing its own verb produces a different
command per provider for the same user intent.

`auth login` becomes the canonical entry; `auth xai-device` stays as a
compatibility alias for at least one release.

## Shared contract (per provider adapter)

Every login adapter implements the same lifecycle, so the CLI, `/auth`, the
provider picker row, `auth status`, and logout behave identically:

1. **Initiate** — browser OAuth (authorization-code + PKCE, loopback
   callback, random state) or device-code flow, chosen by the provider
   adapter. Manual code-paste fallback for SSH/headless.
2. **Store** — structured credential (`access_token`, `refresh_token`,
   `expires_at`, `auth_mode`) in a dedicated auth store (0600, atomic
   writes) or keyring entry. Never written into `config.toml` as `api_key`
   — the static-key storage and logout paths are not designed for
   refreshable tokens.
3. **Resolve** — provider/auth resolution recognizes `auth_mode = "oauth"`
   as valid auth without an API key, classifies the source distinctly
   (not `missing`, not `config`), and refreshes before use. Seams:
   `crates/tui/src/config.rs` auth resolution, `crates/config/src/
   provider.rs`, the shared runtime resolver in `crates/config/src/lib.rs`.
4. **Send** — per-provider header mode. Anthropic OAuth uses
   `Authorization: Bearer` plus the required beta/identity headers and must
   not also send `x-api-key` (`crates/engine/src/client.rs` header
   construction). Clients capture credentials at construction today, so
   long-running sessions need request-time refresh or a rebuild path.
5. **Status / logout** — `auth status` shows mode, source, and expiry
   without exposing tokens; `logout` removes OAuth credentials and the auth
   mode, tells the user how to log in again after failed refresh, and never
   unsets shell-managed environment variables.
6. **Tests** — PKCE/state rejection, callback success/cancel/timeout,
   mock-HTTP token exchange and refresh, file permissions/atomicity,
   API-key-vs-OAuth header selection, picker readiness, logout/status,
   401-recovery. No real credentials in CI.

## Hard gate before Anthropic implementation

Do not copy OAuth constants, client IDs, scopes, or Claude-Code-specific
headers from reference implementations (e.g. Pi's
`packages/ai/src/utils/oauth/anthropic.ts`) without first verifying that
Codewhale is permitted to use that flow. Those details may be
client-specific or governed by Anthropic compatibility policy. This
verification is an explicit maintainer action and blocks the Anthropic
adapter, not the shared `auth login` scaffolding.

## Out of scope

- Changing existing API-key authentication (unchanged, first-class).
- Hosted/remote token brokering.
- Any billing/usage-limit interpretation beyond surfacing the provider's
  own messaging that subscription OAuth may differ from API billing.
