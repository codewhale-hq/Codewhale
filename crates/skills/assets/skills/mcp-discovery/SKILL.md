---
name: mcp-discovery
description: "Find and start a zero-environment local MCP server when the session lacks a capability that no available tool, project script, or ordinary local code can cover."
---

# mcp-discovery

The public MCP Registry ships hundreds of ready-made stdio servers
(filesystems, databases, browsers, media processing, developer utilities, cloud
APIs, SaaS integrations, …). This skill is how you reach one when the session
is genuinely missing a capability.

It is a fallback, not a first step. Discovery costs a `tool_search` to load the
deferred tools, a query, an approval, and an install — spend that only when it
buys a capability you do not have.

## When to use

- An available tool, a project script or test, or a few lines of local code
  cannot do the job.
- The task needs an external service, database, or protocol client that this
  session has no access to.
- You would otherwise install a package or reimplement a well-known converter
  or client by hand.

## When not to use

- Ordinary work: writing and editing files, reading fixtures, running repo
  commands, and checking your own output. Do those directly.
- A capability an already-loaded tool covers, even partially — try it first.
- A near-miss row in a result list. A scored match is a suggestion; the missing
  capability is what decides.

## Workflow

1. **Name the gap.** State the capability you lack before searching. If you
   cannot name one, you do not need the Registry.
2. **Load and query.** `registry_sync` and `start_registry_mcp_server` are
   deferred: load one with `tool_search` and use the returned schema. If a call
   only loads the schema without executing, retry once with that schema. Then call
   `registry_sync {query: "<the missing capability>"}`. It scores a host-side
   snapshot and returns at most eight matches; the full index never enters the
   conversation. Packages declaring any environment variable (including API
   keys/tokens) are excluded and never written to the cache.
3. **Judge the matches.** Take a server when it covers the gap you named. If
   nothing does, refine the query once, then continue with local tools.
4. **Install + run transactionally.** Call
   `start_registry_mcp_server {registry_name: "<exact name>", arguments: {...}}`.
   Supply only values listed in `required_args`; omit `arguments` when none are
   required. Never install or launch the package through the shell. Starting a
   server requires approval.
5. **Solve the task with the new tools.** Their complete schemas are added to
   the current turn immediately after a successful connection; call the exact
   names returned by the start result.

## If a server fails to start

`start_registry_mcp_server` reports when a package exits before the handshake
(often CLI help output = incomplete launch args). Verify the exact required
arguments, retry once with the corrected structured values, and if it still
fails move on to the next candidate or to a local approach. Failed starts are
rolled back, so retrying the same Registry name is safe.

## Don't

- Don't attempt to pass env vars or secrets; this flow has no env channel.
- Don't reconstruct or edit the Registry-provided package command.
