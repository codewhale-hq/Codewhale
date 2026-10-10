# Plugin-defined AI providers and OAuth

A reviewed plugin can declare named AI providers in
`extensions.net.codewhale.providers` in `plugin.json` (or `[providers.<name>]`
in a legacy `plugin.toml`). These declarations extend the existing named
OpenAI-compatible routes; they do not replace the inference engine or create a
second turn loop. See [the complete example](examples/plugins/oauth-provider/plugin.json).

The host owns authorization, callback handling, token storage and refresh.
Provider declarations contain public configuration only. Plugin JavaScript,
tools, the model transcript and frontends never receive OAuth token material.
No Codewhale release number is embedded in a declaration: compatibility follows
the manifest schema and the provider capability supported by the host.

## Declare and review a provider

Copy the example to your plugin directory and change its provider ID, API base
URL, model IDs, OAuth endpoints, public client ID, scopes and optional resource.
Register the public OAuth client with the authorization server, permitting the
loopback redirect `http://127.0.0.1:<dynamic-port>/<callback-path>` and PKCE S256.
The host does not use a client secret.
Authorization and token endpoints must share the declared issuer origin;
cross-origin OIDC discovery is not part of this declaration. HTTPS is required,
with plain HTTP allowed only on local loopback for development.

The provider ID must be a custom lowercase plugin-style identifier. It cannot
shadow a builtin provider, a configured custom provider or another plugin's
provider. The existing chooser may persist a model-only
`[providers.<plugin-provider-id>]` preference; that preference changes the
selected model, never the reviewed endpoint, authentication or headers. A table
containing any other provider setting remains a collision. Model IDs are exact
wire IDs, not renamed builtin model profiles.
Metadata not provided by the server remains unknown. Optional `http_headers`
can supply public project/group routing metadata to the existing HTTP client.
Authorization, cookies, host overrides and transport-control headers are
refused; never put credentials in a plugin manifest. Plugin requests use only
their reviewed public routing headers and do not inherit global headers from
other provider connections.

Install, validate, trust and enable the bundle through Codewhale's existing
plugin commands. The review receipt covers the full manifest, the provider
capability, the API destination and OAuth endpoint origins. Installing a bundle
alone does not authorize it, contact an endpoint or start a login. A changed
manifest or capability requires a fresh review; start a new runtime after
changing declarations.

Adding provider authority advances the activation policy to v5, or v6 when
the extension host is enabled. Receipts from the previous policy require an
explicit review again, including bundles without provider declarations. An
old review is never silently upgraded.

## Sign in inside Codewhale

After installation, review and enable the bundle, then start a new session.
Run `/login` to open the provider picker, or `/login <provider-id>` to authorize
an enabled plugin directly. The host runs its existing PKCE flow and then
refreshes the provider’s standard `/models` roster. Choose the model explicitly;
login never chooses the first model, changes billing groups, or copies another
application’s credentials. A provider selected from `/provider` uses the same
flow instead of asking for an API key.

`/login status` retains account status. `/logout <provider-id>` removes only that
plugin’s local grant. Bare `/logout` retains Codewhale account logout.
Plugin rosters are account-scoped and not reused from the disk cache.
The terminal is temporarily suspended during browser authorization, like the
built-in PKCE flows; device-code and remote revocation are not added here.
A successful authorization followed by a catalog failure retains the grant;
retry catalog refresh rather than copying a token or running a companion login.

## Terminal and noninteractive clients

```sh
codewhale auth plugin-login --provider example-oauth
codewhale --provider example-oauth --model example-chat exec 'Say hello.'
codewhale auth plugin-logout --provider example-oauth
```

`plugin-login` opens the system browser and waits on an ephemeral IPv4 loopback
listener. Set `CODEWHALE_PLUGIN_OAUTH_NO_BROWSER=1` to open the printed URL
manually on the same machine. This is not a device-code or remote SSH flow.
Denial, malformed callbacks, mismatched state and a conflicting callback issuer
fail without storing new credentials. An issuer may supply the RFC 9207 `iss`
parameter; when present it must equal the reviewed issuer.

The credential slot binds the exact provider ID, API base URL and complete OAuth
descriptor. A different plugin endpoint, client, issuer, scope or resource cannot
reuse that grant. Expiring tokens refresh under the existing secure-store entry
transaction, including rotation. Client construction and generic config-key
reads never resolve or expose the bearer; only the request worker accesses it.
Read-only diagnostics, including live diagnostic probes, do not refresh tokens
or migrate credential storage. A probe with an expired token therefore fails
without contacting the issuer.
Each actual request checks current plugin authority, binds its endpoint, OAuth
configuration and public headers to the reviewed declaration, then resolves the
current host-owned credential. Candidate routes or in-memory configuration edits
cannot borrow a receipt for a different endpoint or authentication configuration. Disabling or revoking a plugin stops a previously built
client before its next request. Requests and token exchange refuse redirects
that could forward credentials to another destination.

Logout removes the local credential. Removing a plugin or changing its
manifest does not silently erase stored grants, nor does local logout revoke
an authorization-server session. Use that server's account page to revoke the
remote grant if needed.

## Current scope

The declaration supports standard public-client authorization-code PKCE and
OpenAI-compatible Chat Completions, including streaming, and the existing
`/models` catalog. Token responses must specify Bearer token type and a positive
`expires_in`; tokens with an unknown lifetime are refused. HTTP failure bodies
from plugin endpoints are withheld so an echoed rotated opaque bearer cannot
leak into logs or frontends. It does not execute custom authorization/refresh callbacks,
load arbitrary inference protocol code, provide device authorization, use a
client secret or implement remote revocation. A server using another wire
protocol needs a host protocol adapter rather than a fictitious supported kind.
The existing extension host still owns executable tools; provider declarations
remain data and do not require that experimental feature.

Providers do not add session-context contributors or volatile prompt prefixes.
Inference continues through the existing logged turn loop and normal request
construction, so this extension has no new KV-cache-prefix effect.
