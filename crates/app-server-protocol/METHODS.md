# Codex App Server method inventory

Every method in the pinned Codex `rust-v0.166.0` stable wire enums (commit `6ff1e1ca3b30fc8b914720681fcf5fe62e95f856`), with the tier Codewhale gives it and what is on main. The method sets are the unmodified export members under `tests/pinned/codex/`; `tests/methods.rs` fails when this file, those members and the typed enums in `src/methods.rs` disagree.

- **A**: required for 0.10.3 Codex parity.
- **B**: service methods, required for 0.10.3.
- **C**: out of scope, for the reason given. A Tier C client request answers `-32601` method not found; the server never sends a Tier C request or notification.
- **undecided**: the 0.10.3 parity bar does not settle it yet. The last column carries a recommended tier for the founder to confirm.

Status: `implemented` means a Codex-shaped handler is on main (response goldens are not yet checked); `partial` means the Codex method name dispatches to the legacy shape; `missing` means no handler; `method-not-found` means the dispatcher answers `-32601`; `never-sent` means the server never emits it. The stable export is the scope: methods that exist only in the experimental export are not listed.

## Client requests

| Method | Tier | Status | Basis | Recommended |
| --- | --- | --- | --- | --- |
| `initialize` | A | implemented | connection lifecycle (CW-7704; on main) |  |
| `thread/start` | A | implemented | thread lifecycle |  |
| `thread/resume` | A | partial | thread lifecycle |  |
| `thread/fork` | A | partial | thread lifecycle |  |
| `thread/archive` | A | partial | thread lifecycle |  |
| `thread/delete` | B | missing | S9 services: thread delete, revert, shellCommand, compact |  |
| `thread/unsubscribe` | A | missing | S5 subscriptions and paging |  |
| `thread/name/set` | A | implemented | thread lifecycle |  |
| `thread/goal/set` | A | partial | S8 goals |  |
| `thread/goal/get` | A | partial | S8 goals |  |
| `thread/goal/clear` | A | partial | S8 goals |  |
| `thread/metadata/update` | undecided | missing | thread metadata beyond name/goal | B |
| `thread/attachment/add` | undecided | missing | Codex desktop attachments | C |
| `thread/attachment/list` | undecided | missing | Codex desktop attachments | C |
| `thread/attachmentOwner/list` | undecided | missing | Codex desktop attachments | C |
| `thread/attachment/remove` | undecided | missing | Codex desktop attachments | C |
| `thread/section/move` | undecided | missing | Codex desktop sidebar sections; Codewhale projects are CW-7878 | C |
| `thread/unarchive` | A | partial | thread lifecycle |  |
| `thread/compact/start` | B | missing | S9 services: thread delete, revert, shellCommand, compact |  |
| `thread/shellCommand` | B | missing | S9 services: thread delete, revert, shellCommand, compact |  |
| `thread/approveGuardianDeniedAction` | undecided | missing | Codex Guardian auto-review override; Codewhale has its own Auto-Review posture | C |
| `thread/revert` | B | missing | S9 services: thread delete, revert, shellCommand, compact |  |
| `thread/list` | A | implemented | thread lifecycle |  |
| `threadSection/list` | undecided | missing | Codex desktop sidebar sections; Codewhale projects are CW-7878 | C |
| `threadSection/create` | undecided | missing | Codex desktop sidebar sections; Codewhale projects are CW-7878 | C |
| `threadSection/update` | undecided | missing | Codex desktop sidebar sections; Codewhale projects are CW-7878 | C |
| `threadSection/delete` | undecided | missing | Codex desktop sidebar sections; Codewhale projects are CW-7878 | C |
| `thread/loaded/list` | A | implemented | thread lifecycle |  |
| `thread/read` | A | implemented | thread lifecycle |  |
| `thread/turns/list` | A | missing | S5 subscriptions and paging |  |
| `thread/items/list` | A | missing | S5 subscriptions and paging |  |
| `thread/inject_items` | undecided | missing | raw history injection | C |
| `skills/list` | B | missing | S9 services: skills |  |
| `skills/extraRoots/set` | undecided | missing | skills configuration writes | B |
| `hooks/list` | B | missing | S9 services: hooks |  |
| `marketplace/add` | undecided | missing | Codex plugin marketplace; Codewhale plugins are DSH (CW-7051) | C |
| `marketplace/remove` | undecided | missing | Codex plugin marketplace; Codewhale plugins are DSH (CW-7051) | C |
| `marketplace/upgrade` | undecided | missing | Codex plugin marketplace; Codewhale plugins are DSH (CW-7051) | C |
| `plugin/list` | undecided | missing | Codex plugin marketplace; Codewhale plugins are DSH (CW-7051) | C |
| `plugin/installed` | undecided | missing | Codex plugin marketplace; Codewhale plugins are DSH (CW-7051) | C |
| `plugin/reconcile` | undecided | missing | Codex plugin marketplace; Codewhale plugins are DSH (CW-7051) | C |
| `plugin/read` | undecided | missing | Codex plugin marketplace; Codewhale plugins are DSH (CW-7051) | C |
| `plugin/skill/read` | undecided | missing | Codex plugin marketplace; Codewhale plugins are DSH (CW-7051) | C |
| `plugin/share/save` | C | method-not-found | OpenAI-hosted plugin sharing; no Codewhale service |  |
| `plugin/share/updateTargets` | C | method-not-found | OpenAI-hosted plugin sharing; no Codewhale service |  |
| `plugin/share/list` | C | method-not-found | OpenAI-hosted plugin sharing; no Codewhale service |  |
| `plugin/share/checkout` | C | method-not-found | OpenAI-hosted plugin sharing; no Codewhale service |  |
| `plugin/share/delete` | C | method-not-found | OpenAI-hosted plugin sharing; no Codewhale service |  |
| `app/read` | C | method-not-found | ChatGPT Apps connectors; no Codewhale service |  |
| `app/list` | C | method-not-found | ChatGPT Apps connectors; no Codewhale service |  |
| `app/installed` | C | method-not-found | ChatGPT Apps connectors; no Codewhale service |  |
| `fs/readFile` | B | missing | S9 services: fs |  |
| `fs/writeFile` | B | missing | S9 services: fs |  |
| `fs/createDirectory` | B | missing | S9 services: fs |  |
| `fs/getMetadata` | B | missing | S9 services: fs |  |
| `fs/readDirectory` | B | missing | S9 services: fs |  |
| `fs/remove` | B | missing | S9 services: fs |  |
| `fs/copy` | B | missing | S9 services: fs |  |
| `fs/watch` | B | missing | S9 services: fs |  |
| `fs/unwatch` | B | missing | S9 services: fs |  |
| `skills/config/write` | undecided | missing | skills configuration writes | B |
| `plugin/install` | undecided | missing | Codex plugin marketplace; Codewhale plugins are DSH (CW-7051) | C |
| `plugin/uninstall` | undecided | missing | Codex plugin marketplace; Codewhale plugins are DSH (CW-7051) | C |
| `turn/start` | A | missing | S4a full-tool turns |  |
| `turn/steer` | A | missing | S4a full-tool turns |  |
| `turn/interrupt` | A | missing | S4a full-tool turns |  |
| `review/start` | B | missing | S9 services: review |  |
| `model/list` | B | missing | S9 services: models |  |
| `account/gatewayOAuth/read` | C | method-not-found | OpenAI gateway OAuth; no Codewhale service |  |
| `account/gatewayOAuth/login` | C | method-not-found | OpenAI gateway OAuth; no Codewhale service |  |
| `account/gatewayOAuth/cancel` | C | method-not-found | OpenAI gateway OAuth; no Codewhale service |  |
| `modelProvider/capabilities/read` | undecided | missing | provider, feature and permission catalogs | B |
| `experimentalFeature/list` | undecided | missing | Codex experimental feature flags | C |
| `permissionProfile/list` | undecided | missing | provider, feature and permission catalogs | B |
| `experimentalFeature/enablement/set` | undecided | missing | Codex experimental feature flags | C |
| `mcpServer/oauth/login` | undecided | missing | MCP actions beyond status | B |
| `config/mcpServer/reload` | undecided | missing | MCP actions beyond status | B |
| `mcpServerStatus/list` | B | missing | S9 services: MCP status |  |
| `mcpServer/resource/read` | undecided | missing | MCP actions beyond status | B |
| `mcpServer/tool/call` | undecided | missing | MCP actions beyond status | B |
| `windowsSandbox/setupStart` | undecided | missing | Codex Windows sandbox setup; Codewhale has its own AppContainer sandbox | C |
| `windowsSandbox/readiness` | undecided | missing | Codex Windows sandbox setup; Codewhale has its own AppContainer sandbox | C |
| `account/login/start` | undecided | missing | OpenAI account sign-in and usage; Codewhale signs in through /auth | C |
| `account/login/cancel` | undecided | missing | OpenAI account sign-in and usage; Codewhale signs in through /auth | C |
| `account/logout` | undecided | missing | OpenAI account sign-in and usage; Codewhale signs in through /auth | C |
| `account/rateLimits/read` | undecided | missing | OpenAI account sign-in and usage; Codewhale signs in through /auth | C |
| `account/rateLimitResetCredit/consume` | C | method-not-found | OpenAI billing and workspace messaging; no Codewhale service |  |
| `account/usage/read` | undecided | missing | OpenAI account sign-in and usage; Codewhale signs in through /auth | C |
| `account/workspaceMessages/read` | C | method-not-found | OpenAI billing and workspace messaging; no Codewhale service |  |
| `account/sendAddCreditsNudgeEmail` | C | method-not-found | OpenAI billing and workspace messaging; no Codewhale service |  |
| `feedback/upload` | C | method-not-found | uploads feedback to OpenAI; no Codewhale service |  |
| `feedback/save` | C | method-not-found | uploads feedback to OpenAI; no Codewhale service |  |
| `command/exec` | B | missing | S9 services: command/exec |  |
| `command/exec/write` | B | missing | S9 services: command/exec |  |
| `command/exec/terminate` | B | missing | S9 services: command/exec |  |
| `command/exec/resize` | B | missing | S9 services: command/exec |  |
| `config/read` | B | missing | S9 services: config |  |
| `externalAgentConfig/detect` | undecided | missing | import config from other agents; Codewhale external agents are CW-7879 | C |
| `externalAgentConfig/import` | undecided | missing | import config from other agents; Codewhale external agents are CW-7879 | C |
| `externalAgentConfig/import/recordHistory` | undecided | missing | import config from other agents; Codewhale external agents are CW-7879 | C |
| `externalAgentConfig/import/readHistories` | undecided | missing | import config from other agents; Codewhale external agents are CW-7879 | C |
| `config/value/write` | B | missing | S9 services: config |  |
| `config/batchWrite` | B | missing | S9 services: config |  |
| `configRequirements/read` | undecided | missing | managed config requirements | B |
| `account/read` | undecided | missing | OpenAI account sign-in and usage; Codewhale signs in through /auth | C |
| `getConversationSummary` | C | method-not-found | deprecated v1 API (upstream marks it DEPRECATED; TypeScript-only in the export) |  |
| `gitDiffToRemote` | C | method-not-found | deprecated v1 API (upstream marks it DEPRECATED; TypeScript-only in the export) |  |
| `getAuthStatus` | C | method-not-found | deprecated v1 API (upstream marks it DEPRECATED; TypeScript-only in the export) |  |
| `fuzzyFileSearch` | undecided | missing | file search | B |

## Client notifications

| Method | Tier | Status | Basis | Recommended |
| --- | --- | --- | --- | --- |
| `initialized` | A | implemented | connection lifecycle (CW-7704; on main) |  |

## Server requests

| Method | Tier | Status | Basis | Recommended |
| --- | --- | --- | --- | --- |
| `item/commandExecution/requestApproval` | A | missing | S6 interactions (CW-7704: bidirectional approvals, input, client tools) |  |
| `item/fileChange/requestApproval` | A | missing | S6 interactions (CW-7704: bidirectional approvals, input, client tools) |  |
| `item/tool/requestUserInput` | A | missing | S6 interactions (CW-7704: bidirectional approvals, input, client tools) |  |
| `mcpServer/elicitation/request` | undecided | missing | MCP elicitation | A |
| `item/permissions/requestApproval` | A | missing | S6 interactions (CW-7704: bidirectional approvals, input, client tools) |  |
| `item/tool/call` | A | missing | S6 interactions (CW-7704: bidirectional approvals, input, client tools) |  |
| `account/chatgptAuthTokens/refresh` | C | never-sent | ChatGPT externally managed auth tokens; no Codewhale service |  |
| `attestation/generate` | C | never-sent | OpenAI request attestation; no Codewhale service |  |
| `applyPatchApproval` | C | never-sent | deprecated v1 approvals for legacy turns; superseded by item/*/requestApproval |  |
| `execCommandApproval` | C | never-sent | deprecated v1 approvals for legacy turns; superseded by item/*/requestApproval |  |

## Server notifications

| Method | Tier | Status | Basis | Recommended |
| --- | --- | --- | --- | --- |
| `error` | A | missing | S3 projection: errors |  |
| `thread/started` | A | missing | S3 projection: thread lifecycle |  |
| `thread/status/changed` | A | missing | S3 projection: thread lifecycle |  |
| `thread/readState/changed` | undecided | missing | thread state beyond the Tier A set | A |
| `thread/archived` | A | missing | S3 projection: thread lifecycle |  |
| `thread/deleted` | B | missing | pairs with a Tier B service |  |
| `thread/unarchived` | A | missing | S3 projection: thread lifecycle |  |
| `thread/closed` | A | missing | S3 projection: thread lifecycle |  |
| `thread/reverted` | B | missing | pairs with a Tier B service |  |
| `skills/changed` | B | missing | pairs with a Tier B service |  |
| `thread/name/updated` | A | missing | S3 projection: thread lifecycle |  |
| `thread/attachment/updated` | undecided | missing | Codex desktop attachments, predictions, projects and environments | C |
| `thread/goal/updated` | A | missing | S3 projection with S8 goals |  |
| `thread/prediction/updated` | undecided | missing | Codex desktop attachments, predictions, projects and environments | C |
| `thread/goal/cleared` | A | missing | S3 projection with S8 goals |  |
| `thread/queue/changed` | undecided | missing | thread state beyond the Tier A set | A |
| `project/changed` | undecided | missing | Codex desktop attachments, predictions, projects and environments | C |
| `thread/project/updated` | undecided | missing | Codex desktop attachments, predictions, projects and environments | C |
| `thread/environment/connected` | undecided | missing | Codex desktop attachments, predictions, projects and environments | C |
| `thread/environment/disconnected` | undecided | missing | Codex desktop attachments, predictions, projects and environments | C |
| `thread/settings/updated` | undecided | missing | thread state beyond the Tier A set | A |
| `thread/tokenUsage/updated` | A | missing | S3 projection: thread lifecycle |  |
| `turn/started` | A | missing | S3 projection: turn/* |  |
| `hook/started` | undecided | missing | hook runs; hooks/list is Tier B | B |
| `turn/completed` | A | missing | S3 projection: turn/* |  |
| `hook/completed` | undecided | missing | hook runs; hooks/list is Tier B | B |
| `turn/diff/updated` | A | missing | S3 projection: turn/* |  |
| `turn/plan/updated` | A | missing | S3 projection: turn/* |  |
| `item/started` | A | missing | S3 projection: item/* |  |
| `item/autoApprovalReview/started` | undecided | missing | Codex Guardian; Codewhale has its own Auto-Review posture | C |
| `item/autoApprovalReview/completed` | undecided | missing | Codex Guardian; Codewhale has its own Auto-Review posture | C |
| `autoApprovalReview/strictReviewRequired` | undecided | missing | Codex Guardian; Codewhale has its own Auto-Review posture | C |
| `item/completed` | A | missing | S3 projection: item/* |  |
| `rawResponseItem/completed` | C | never-sent | internal-only upstream (Codex Cloud, exact upstream usage); TypeScript-only in the export |  |
| `rawResponse/completed` | C | never-sent | internal-only upstream (Codex Cloud, exact upstream usage); TypeScript-only in the export |  |
| `item/agentMessage/delta` | A | missing | S3 projection: item/* |  |
| `item/plan/delta` | A | missing | S3 projection: item/* |  |
| `command/exec/outputDelta` | B | missing | pairs with a Tier B service |  |
| `process/outputDelta` | undecided | missing | background processes | B |
| `process/exited` | undecided | missing | background processes | B |
| `item/commandExecution/outputDelta` | A | missing | S3 projection: item/* |  |
| `item/commandExecution/terminalInteraction` | A | missing | S3 projection: item/* |  |
| `item/fileChange/outputDelta` | A | missing | S3 projection: item/* |  |
| `item/fileChange/patchUpdated` | A | missing | S3 projection: item/* |  |
| `serverRequest/resolved` | A | missing | S6 interactions (CW-7704) |  |
| `item/mcpToolCall/progress` | A | missing | S3 projection: item/* |  |
| `mcpServer/oauthLogin/completed` | undecided | missing | MCP server events | B |
| `mcpServer/startupStatus/updated` | undecided | missing | MCP server events | B |
| `mcpServer/event/stream/notification` | undecided | missing | MCP server events | B |
| `account/updated` | undecided | missing | OpenAI account state | C |
| `account/gatewayOAuth/changed` | C | never-sent | pairs with a Tier C method |  |
| `account/rateLimits/updated` | undecided | missing | OpenAI account state | C |
| `app/list/updated` | C | never-sent | pairs with a Tier C method |  |
| `remoteControl/status/changed` | undecided | missing | remote control status | C |
| `externalAgentConfig/import/progress` | undecided | missing | pairs with externalAgentConfig/import | C |
| `externalAgentConfig/import/completed` | undecided | missing | pairs with externalAgentConfig/import | C |
| `fs/changed` | B | missing | pairs with a Tier B service |  |
| `item/reasoning/summaryTextDelta` | A | missing | S3 projection: item/* |  |
| `item/reasoning/summaryPartAdded` | A | missing | S3 projection: item/* |  |
| `item/reasoning/textDelta` | A | missing | S3 projection: item/* |  |
| `thread/compacted` | B | missing | pairs with a Tier B service |  |
| `model/rerouted` | undecided | missing | OpenAI model routing, verification, moderation and safety | C |
| `model/verification` | undecided | missing | OpenAI model routing, verification, moderation and safety | C |
| `modelProvider/authRecoveryStarted` | undecided | missing | provider auth recovery | B |
| `modelProvider/authRecoveryCompleted` | undecided | missing | provider auth recovery | B |
| `turn/moderationMetadata` | undecided | missing | OpenAI model routing, verification, moderation and safety | C |
| `model/safetyBuffering/updated` | undecided | missing | OpenAI model routing, verification, moderation and safety | C |
| `warning` | undecided | missing | generic warnings | A |
| `guardianWarning` | undecided | missing | Codex Guardian; Codewhale has its own Auto-Review posture | C |
| `deprecationNotice` | undecided | missing | generic warnings | A |
| `configWarning` | undecided | missing | generic warnings | A |
| `fuzzyFileSearch/sessionUpdated` | undecided | missing | pairs with fuzzyFileSearch | B |
| `fuzzyFileSearch/sessionCompleted` | undecided | missing | pairs with fuzzyFileSearch | B |
| `thread/realtime/started` | undecided | missing | realtime audio sessions; no Codewhale equivalent | C |
| `thread/realtime/itemAdded` | undecided | missing | realtime audio sessions; no Codewhale equivalent | C |
| `thread/realtime/item/started` | undecided | missing | realtime audio sessions; no Codewhale equivalent | C |
| `thread/realtime/item/transcript/delta` | undecided | missing | realtime audio sessions; no Codewhale equivalent | C |
| `thread/realtime/item/completed` | undecided | missing | realtime audio sessions; no Codewhale equivalent | C |
| `thread/realtime/transcript/delta` | undecided | missing | realtime audio sessions; no Codewhale equivalent | C |
| `thread/realtime/transcript/done` | undecided | missing | realtime audio sessions; no Codewhale equivalent | C |
| `thread/realtime/outputAudio/delta` | undecided | missing | realtime audio sessions; no Codewhale equivalent | C |
| `thread/realtime/sdp` | undecided | missing | realtime audio sessions; no Codewhale equivalent | C |
| `thread/realtime/error` | undecided | missing | realtime audio sessions; no Codewhale equivalent | C |
| `thread/realtime/closed` | undecided | missing | realtime audio sessions; no Codewhale equivalent | C |
| `windows/worldWritableWarning` | undecided | missing | Codex Windows sandbox | C |
| `windowsSandbox/setupCompleted` | undecided | missing | Codex Windows sandbox | C |
| `account/login/completed` | undecided | missing | OpenAI account state | C |
