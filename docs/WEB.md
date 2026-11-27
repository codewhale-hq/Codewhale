# Local browser client

> 阅读简体中文版：[zh_hans/WEB.md](zh_hans/WEB.md)。

`codewhale web` opens Codewhale's embedded browser client over the canonical
Runtime API. It is a local surface: the server always binds to
`127.0.0.1`, cannot be rebound to a LAN address, and cannot run with Runtime
authentication disabled.

## Start it

From the workspace Codewhale should operate in, run:

```bash
codewhale web
```

The default address is `http://127.0.0.1:7878`. To avoid a local port
collision, choose another loopback port:

```bash
codewhale web --port 8788
```

Codewhale starts the Runtime API, serves the dependency-free client embedded
in the installed binary, prints a single-use launch URL, and asks the operating
system to open that URL in the default browser. If the browser does not open,
use the printed URL within ten minutes. Stop the process with `Ctrl+C`; the
browser session ends with it.

## What the browser can do

The embedded client provides a responsive thread and search rail, Runtime-owned
session facts, transcript and tool receipts, and a composer. It can create,
select, rename, and archive threads; start or steer turns; interrupt work;
resolve approvals; and answer Runtime user-input requests.

New threads use the configured provider and model by default. Named custom
routes retain their exact `[providers.<name>]` identity when loading models
and creating threads, including when several routes share the `custom` kind.
Selecting a route for a new thread does not change the Runtime defaults.
Provider endpoints and credentials are configured on the server; this dialog
only selects an existing route and model.

The browser is another view of the same local Runtime. It does not create a
second cloud account, copy provider credentials into browser storage, or
weaken the configured approval and sandbox policies.

## Serving the shared app build

`codewhale web` serves the embedded client by default. You can instead serve
the shared web app (the `apps/web` build from the `codewhale-app` repository)
from a directory on disk. This is opt-in; the embedded client remains the
default until the shared build reaches parity.

With Engine 0.10.5 or later, build the local Engine client from the root of
a `codewhale-app` checkout:

```bash
npm run build:engine -w apps/web
```

From the workspace where you want to use Codewhale, point the Engine at the
build's absolute path:

```bash
CODEWHALE_WEB_DIST="/path/to/codewhale-app/apps/web/dist-engine" codewhale web
```

If you have access to the App repository's Desktop workflow, you can also
download its `codewhale-web-dist-engine` artifact. Extract it and point
`CODEWHALE_WEB_DIST` at the directory containing `index.html`.

The directory must contain `index.html`, and neither the directory nor
`index.html` may be a symlink. When the variable is set to a valid directory,
startup prints a line naming the directory being served. When the variable is
unset or empty, the embedded client is served as before.

A set but invalid value is not ignored: `codewhale web` refuses to start and
prints the reason (the path is not a directory, is a symlink, or has no
`index.html`). Fix or unset the variable and run the command again.

When a Codewhale is already running (an open terminal session, for example),
`codewhale web` attaches to it, and that running Codewhale decides which
client is served from the environment it was started with. If the directory
you asked for is not the one it serves, the command stops and says so instead
of opening the other client: quit the running Codewhale and start
`codewhale web` again, or unset the variable. When the running Codewhale
serves a directory, the attach prints the same line naming it.

What is served from the directory:

- `index.html` at `/`, and for extensionless paths, so client-side routes such
  as `/settings/plugins` load the app.
- `/plugin-host.html`, the document the app frames for plugin UI.
- `/static/*`, the build's hashed assets.

`/v1`, `/api` and `/__codewhale` stay reserved for the Runtime API and the
bootstrap exchange; the directory never shadows them. Other `.html` files,
unknown file types and non-`GET`/`HEAD` requests return `404`.

The authentication boundary above is unchanged. `codewhale web` requires
Runtime authentication, and the Engine refuses `--insecure` for web.

## Authentication boundary

The browser-launch URL contains a random, short-lived, one-time bootstrap
capability. It never contains the Runtime bearer token. A loopback request
exchanges the capability for an `HttpOnly`, `SameSite=Strict`, process-local
session cookie and immediately invalidates the capability.

Reused, expired, malformed, and non-loopback bootstrap attempts fail closed.
The Runtime token is not placed in rendered HTML, browser storage, URL
queries or fragments, or browser-launch arguments. The one-time bootstrap
value is printed in the local terminal and briefly passes through the operating
system's browser launcher. It is single-use and expires after ten minutes, but
a hostile process already running as the same OS user remains inside the local
trust boundary.

Cookie-authenticated state-changing requests must also present the exact
local web origin. Cross-origin browser requests are rejected. Existing
explicit bearer and Runtime-token-header clients retain their normal Runtime
API behavior.

## Local means local

`codewhale web` accepts only `--port`; there is no `--host` or insecure-auth
option on this command. Do not treat it as a public website or expose its port
directly through router forwarding, a public reverse proxy, or a tunnel.

The separate `codewhale app-server --mobile` and `--http` modes have different
deployment and authentication contracts. Read [RUNTIME_API.md](RUNTIME_API.md)
before operating either one, especially before selecting a non-loopback bind.

## Troubleshooting

- If port `7878` is occupied, pass an unused `--port` value.
- If the browser does not open, copy the printed single-use bootstrap URL into
  a browser on the same machine within ten minutes. Start `codewhale web` again
  if that URL has already been used or expired.
- If the page loads but a provider is unavailable, inspect `codewhale doctor`
  and `/provider`; the web command does not configure or move provider
  credentials.
- If a session expired, stop and restart `codewhale web` to mint a new
  process-local session. Reusing an old bootstrap URL is expected to fail.
- A new tab opened by typing the address is not signed in: the session's
  request proof stays in the tab that opened it. Use that tab, duplicate it,
  or restart `codewhale web`.

For integration endpoints, headers, events, and the complete web-session
contract, see [RUNTIME_API.md](RUNTIME_API.md).
