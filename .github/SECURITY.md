# Security Policy

codewhale is a coding agent with direct access to file operations, shell execution, and the network. Security disclosures are taken seriously.

## Supported Versions

Only the latest stable release receives security patches. No backports to older versions.

| Version | Supported |
|---|---|
| latest stable | :white_check_mark: |
| < latest | :x: |

Check the [releases page](https://github.com/codewhale-hq/CodeWhale/releases) for the current version.

## Reporting a Vulnerability

**Do not open a public GitHub issue for security vulnerabilities.**

Report privately via one of:

- **GitHub private advisory**: [github.com/codewhale-hq/CodeWhale/security/advisories/new](https://github.com/codewhale-hq/CodeWhale/security/advisories/new)
- **Email**: [hmbown@gmail.com](mailto:hmbown@gmail.com) — include `[SECURITY]` in the subject line

Include in your report:

- A description of the vulnerability and the impact if exploited
- Steps to reproduce or a proof of concept
- Affected versions and configuration details
- Any suggested mitigation (optional)

## Response Timeline

| Phase | Target |
|---|---|
| Acknowledgment | Within 48 hours of receipt |
| Assessment | Within 5 days — triage severity, scope, and fix approach |
| Patch (critical) | Within 14 days from assessment |
| Patch (moderate/low) | Next feature release or per-maintainer timeline |
| Disclosure | After patch is shipped and users have had time to update |

You will receive status updates at each phase. If the timeline slips, we will communicate the reason and the revised estimate.

## Scope

### In scope (what counts)

- Remote code execution through crafted prompts or model responses
- Sandbox escape — breaking out of an active Seatbelt/bubblewrap wrapper or a declared workspace boundary
- Credential leak — exfiltration of API keys, tokens, or environment secrets
- Arbitrary file read/write outside the intended workspace (`PathEscape` bypass)
- SSRF via `fetch_url` or `web_search` against internal network endpoints
- Unauthorised MCP server access or tool invocation

### Out of scope

- Social engineering of the maintainer or contributors
- Denial of service / rate-limit exhaustion against the DeepSeek API
- Vulnerabilities in third-party dependencies (report to the upstream project)
- Attacks requiring physical access to the victim's machine
- Theoretical ML-model injection attacks not demonstrated in the codewhale context

If you are unsure whether a bug is in scope, report it anyway. We will triage and respond.


## WeCom Bridge Security

The WeCom Bridge (`integrations/wecom-bridge/`) extends Codewhale to WeCom
(企业微信) Smart Bot WebSocket sessions. It inherits all standard Codewhale
security boundaries and adds bridge-specific controls.

### Bridge-specific protections

- **No public port**: The bridge communicates with `codewhale serve --http` on `127.0.0.1` only
- **Token gate**: All runtime API calls carry `CODEWHALE_RUNTIME_TOKEN`
- **Chat allowlist**: Only chats/users listed in `WECOM_CHAT_ALLOWLIST` can interact. First-pairing mode (`WECOM_ALLOW_UNLISTED=true`) is meant for onboarding only
- **Approval required**: Tool calls from WeCom sessions must be approved — either via explicit `/allow <id>` commands or natural-language keywords (`允许`, `yes`, `ok`, etc.)
- **Thread ownership**: `/threads` and `/resume` cover only the threads the chat itself created; any other thread ID is refused with one fixed reply
- **No workspace exposure**: Only prompts, status summaries, and approval requests are sent to WeCom. Workspace contents, shell output, and runtime internals stay on the local machine

### Reporting WeCom Bridge vulnerabilities

Report bridge-specific security issues through the same channels listed above.
Include the bridge version (check `package.json`) and your WeCom deployment configuration
(sensitive values redacted). Bridge logs may be requested for reproduction.

### Bridge environment safety

- `WECOM_BOT_SECRET` and `CODEWHALE_RUNTIME_TOKEN` must never be committed to git
- The `.env` file is gitignored; use `.env.example` as the template
- Rotate secrets periodically, especially after sharing screen captures
- Use `CODEWHALE_APPROVAL_TIMEOUT_MS` (default 5 min) to limit the approval window

## Hall of Fame

We maintain a hall of fame for reporters who submit verified security vulnerabilities. To be credited, include your preferred name / handle in the report.

*No entries yet — be the first.*
