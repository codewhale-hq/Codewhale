---
name: security-review
description: Review a change, module, or surface for exploitable defects — trust boundaries, authn/authz, injection, secret exposure, filesystem and network reach, dependency risk. Use when the user asks for a security review, audit, or vulnerability check of concrete code. Not for general code review, lint, or compliance paperwork.
invocation: model+user
---

# Security Review

Produce findings a reviewer can verify, not a vibes pass. Every finding
names the file, the reachable path that makes it real, and the fix.

## Scope the review first

- **What is under review:** a diff, a module, a plugin bundle, a network
  surface. Say the boundary out loud before reading.
- **Trust boundaries:** where untrusted input enters (HTTP handlers, MCP
  tool args, file parsers, CLI flags, env vars, rendered content) and where
  authority is exercised (fs writes, network egress, process spawn,
  credential reads, signing).
- **Prerequisites:** a checked-out tree and the project's own test runner.
  Ask for credentials only if a live path genuinely needs them; never read
  secrets from the environment or keychain yourself.

## Procedure

1. **Map entry points and sinks.** `rg` for the handlers, deserializers,
   and exec/fs/net calls in scope. Follow data from entry to sink before
   judging it.
2. **Authn/authz.** Every mutating or sensitive handler checks identity and
   object-level authorization. Look for checks that exist on one path but
   not its sibling, and for checks done on the client only.
3. **Injection.** Command lines, SQL, template eval, shell expansion,
   path joins under user influence, and markup that will render later —
   including generated HTML/markdown that carries repo content into a
   browser surface.
4. **Secrets.** `rg` for token/key/secret patterns and `git log -p` the
   diff for credentials. Also check what gets *logged* or embedded in
   receipts, exports, or error messages.
5. **Dependencies.** Run the project's audit gate if it exists
   (`cargo audit`, `npm audit`, `osv-scanner`) — report versions and CVEs,
   not "deps look old".
6. **Denial and abuse paths.** Unbounded reads/allocations, missing
   timeouts on network calls, resource leaks in error paths, retry storms.
7. **Verify a finding before reporting it.** Trace the real call path or
   write a minimal proof. A finding that "looks suspicious" but has no
   reachable path is a note, not a finding.

## Findings format

For each: severity (exploitability × impact), file:line, the reachable
path, a one-paragraph explanation, and the fix. Order by severity. Then a
short "checked and clean" list naming what was audited and cleared — the
scope statement means something only if the clear list is honest.

## Recovery and limits

- If you cannot prove reachability, downgrade the claim and say what
  evidence is missing.
- Do not claim a formal audit, certification, or absence of
  vulnerabilities. This review finds defects; it does not prove none exist.
- Never fix-and-stay-quiet on a security finding in someone else's in-flight
  code — report it first.

## Completion criteria

- Every entry point in scope was traced to its sinks.
- Findings carry file:line + reachability + fix; the clear list names what
  was actually checked.
- Severity ordering is defensible by exploitability, not by how loudly the
  code smells.
