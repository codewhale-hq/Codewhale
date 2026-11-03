---
name: feishu
description: Work with Feishu or Lark bots, docs, sheets, bitables, approval flows, and OpenAPI/MCP setup without hardcoding credentials.
---

# Feishu / Lark

Use this skill when the user asks for Feishu, Lark, or "飞书" integration work.

## Ground Rules

- Feishu China APIs use `open.feishu.cn`; Lark international APIs use
  `open.larksuite.com`.
- Never hardcode app secrets, webhook secrets, tenant tokens, or user tokens.
  Use environment variables such as `FEISHU_APP_ID`,
  `FEISHU_APP_SECRET`, `FEISHU_WEBHOOK_URL`, and
  `FEISHU_WEBHOOK_SECRET`.
- If credentials are unavailable, produce setup instructions or a local stub
  instead of pretending the integration is live.

## Common Use Cases

- Bot webhook messages
- App access token and tenant access token flows
- Docs, Sheets, Wiki, and Bitable reads/writes
- Approval or workflow status updates
- Feishu/Lark MCP server configuration

## Workflow

1. Clarify whether the target is Feishu or Lark.
2. Identify the credential type: webhook, internal app, marketplace app, or
   OAuth user token.
3. Prefer official OpenAPI endpoints and signed webhooks when secrets are
   configured.
4. For MCP, build or configure a server that exposes narrow tools such as
   `send_message`, `read_doc`, `append_sheet_row`, or `query_bitable`.
5. Register the MCP server with `codewhale mcp add`, then run
   `codewhale mcp validate` and `codewhale mcp tools`.
6. Verify with a dry run, sandbox document, or read-back call before sending
   externally visible messages.

Ask for confirmation before sending messages, writing production documents, or
changing approval/workflow state.
