# Codex App Server method inventory

Generated from `contract/codex-v2.json` (contract `codewhale-codex-app-server-v2` version 1) and the pinned Codex `rust-v0.166.0` export (commit `6ff1e1ca3b30fc8b914720681fcf5fe62e95f856`) by `cargo run -p codewhale-app-server-protocol --bin codewhale-codex-contract -- --write`. Edit the manifest, never this file; `tests/contract.rs` fails when they drift. The method sets are the unmodified export members under `tests/pinned/codex/`.

- **A**: required for 0.10.3 Codex parity.
- **B**: service methods, required for 0.10.3.
- **C**: out of scope, for the reason given.
- **undecided**: the 0.10.3 parity bar does not settle it yet. The last column carries a recommended tier for the founder to confirm; until then it is handled exactly like Tier C.

Status: `implemented` means a Codex-shaped handler is on main (response goldens are not yet checked); `partial` means the Codex method name dispatches to the legacy shape; `missing` means no handler; `method-not-found` means the dispatcher answers `-32601`; `never-sent` means the server never emits it. The stable export is the scope: methods that exist only in the experimental export are not listed. A complete inventory or typed shape is not a handler: status, not tier, says what main dispatches.

## Dispatch rules

| Tier | Rule |
| --- | --- |
| A | selected for 0.10.3; the typed params/result in src/v2.rs are the wire contract |
| B | selected for 0.10.3 (service methods); the typed params/result in src/v2.rs are the wire contract |
| C | not selected; a client request answers methodNotFound (-32601); the server never sends a Tier C request or notification |
| undecided | not selected until the founder decides; until then it is handled exactly like Tier C (methodNotFound, never sent), and the row keeps its recommendation |

## Client requests

| Method | Tier | Status | Basis | Recommended | Params | Result |
| --- | --- | --- | --- | --- | --- | --- |
| `initialize` | A | implemented | connection lifecycle (CW-7704; on main) |  | `InitializeParams` | `InitializeResponse` |
| `thread/start` | A | implemented | thread lifecycle |  | `ThreadStartParams` | `ThreadStartResponse` |
| `thread/resume` | A | partial | thread lifecycle |  | `ThreadResumeParams` | `ThreadResumeResponse` |
| `thread/fork` | A | partial | thread lifecycle |  | `ThreadForkParams` | `ThreadForkResponse` |
| `thread/archive` | A | partial | thread lifecycle |  | `ThreadArchiveParams` | `ThreadArchiveResponse` |
| `thread/delete` | B | missing | S9 services: thread delete, revert, shellCommand, compact |  | `ThreadDeleteParams` | `ThreadDeleteResponse` |
| `thread/unsubscribe` | A | missing | S5 subscriptions and paging |  | `ThreadUnsubscribeParams` | `ThreadUnsubscribeResponse` |
| `thread/name/set` | A | implemented | thread lifecycle |  | `ThreadSetNameParams` | `ThreadSetNameResponse` |
| `thread/goal/set` | A | partial | S8 goals |  | `ThreadGoalSetParams` | `ThreadGoalSetResponse` |
| `thread/goal/get` | A | partial | S8 goals |  | `ThreadGoalGetParams` | `ThreadGoalGetResponse` |
| `thread/goal/clear` | A | partial | S8 goals |  | `ThreadGoalClearParams` | `ThreadGoalClearResponse` |
| `thread/metadata/update` | undecided | missing | thread metadata beyond name/goal | B | ThreadMetadataUpdateParams (untyped) | ThreadMetadataUpdateResponse (untyped) |
| `thread/attachment/add` | undecided | missing | Codex desktop attachments | C | ThreadAttachmentAddParams (untyped) | ThreadAttachmentAddResponse (untyped) |
| `thread/attachment/list` | undecided | missing | Codex desktop attachments | C | ThreadAttachmentListParams (untyped) | ThreadAttachmentListResponse (untyped) |
| `thread/attachmentOwner/list` | undecided | missing | Codex desktop attachments | C | ThreadAttachmentOwnerListParams (untyped) | ThreadAttachmentOwnerListResponse (untyped) |
| `thread/attachment/remove` | undecided | missing | Codex desktop attachments | C | ThreadAttachmentRemoveParams (untyped) | ThreadAttachmentRemoveResponse (untyped) |
| `thread/section/move` | undecided | missing | Codex desktop sidebar sections; Codewhale projects are CW-7878 | C | ThreadSectionMoveParams (untyped) | ThreadSectionMoveResponse (untyped) |
| `thread/unarchive` | A | partial | thread lifecycle |  | `ThreadUnarchiveParams` | `ThreadUnarchiveResponse` |
| `thread/compact/start` | B | missing | S9 services: thread delete, revert, shellCommand, compact |  | `ThreadCompactStartParams` | `ThreadCompactStartResponse` |
| `thread/shellCommand` | B | missing | S9 services: thread delete, revert, shellCommand, compact |  | `ThreadShellCommandParams` | `ThreadShellCommandResponse` |
| `thread/approveGuardianDeniedAction` | undecided | missing | Codex Guardian auto-review override; Codewhale has its own Auto-Review posture | C | ThreadApproveGuardianDeniedActionParams (untyped) | ThreadApproveGuardianDeniedActionResponse (untyped) |
| `thread/revert` | B | missing | S9 services: thread delete, revert, shellCommand, compact |  | `ThreadRevertParams` | `ThreadRevertResponse` |
| `thread/list` | A | implemented | thread lifecycle |  | `ThreadListParams` | `ThreadListResponse` |
| `threadSection/list` | undecided | missing | Codex desktop sidebar sections; Codewhale projects are CW-7878 | C | ThreadSectionListParams (untyped) | ThreadSectionListResponse (untyped) |
| `threadSection/create` | undecided | missing | Codex desktop sidebar sections; Codewhale projects are CW-7878 | C | ThreadSectionCreateParams (untyped) | ThreadSectionCreateResponse (untyped) |
| `threadSection/update` | undecided | missing | Codex desktop sidebar sections; Codewhale projects are CW-7878 | C | ThreadSectionUpdateParams (untyped) | ThreadSectionUpdateResponse (untyped) |
| `threadSection/delete` | undecided | missing | Codex desktop sidebar sections; Codewhale projects are CW-7878 | C | ThreadSectionDeleteParams (untyped) | ThreadSectionDeleteResponse (untyped) |
| `thread/loaded/list` | A | implemented | thread lifecycle |  | `ThreadLoadedListParams` | `ThreadLoadedListResponse` |
| `thread/read` | A | implemented | thread lifecycle |  | `ThreadReadParams` | `ThreadReadResponse` |
| `thread/turns/list` | A | missing | S5 subscriptions and paging |  | `ThreadTurnsListParams` | `ThreadTurnsListResponse` |
| `thread/items/list` | A | missing | S5 subscriptions and paging |  | `ThreadItemsListParams` | `ThreadItemsListResponse` |
| `thread/inject_items` | undecided | missing | raw history injection | C | ThreadInjectItemsParams (untyped) | ThreadInjectItemsResponse (untyped) |
| `skills/list` | B | missing | S9 services: skills |  | `SkillsListParams` | `SkillsListResponse` |
| `skills/extraRoots/set` | undecided | missing | skills configuration writes | B | SkillsExtraRootsSetParams (untyped) | SkillsExtraRootsSetResponse (untyped) |
| `hooks/list` | B | missing | S9 services: hooks |  | `HooksListParams` | `HooksListResponse` |
| `marketplace/add` | undecided | missing | Codex plugin marketplace; Codewhale plugins are DSH (CW-7051) | C | MarketplaceAddParams (untyped) | MarketplaceAddResponse (untyped) |
| `marketplace/remove` | undecided | missing | Codex plugin marketplace; Codewhale plugins are DSH (CW-7051) | C | MarketplaceRemoveParams (untyped) | MarketplaceRemoveResponse (untyped) |
| `marketplace/upgrade` | undecided | missing | Codex plugin marketplace; Codewhale plugins are DSH (CW-7051) | C | MarketplaceUpgradeParams (untyped) | MarketplaceUpgradeResponse (untyped) |
| `plugin/list` | undecided | missing | Codex plugin marketplace; Codewhale plugins are DSH (CW-7051) | C | PluginListParams (untyped) | PluginListResponse (untyped) |
| `plugin/installed` | undecided | missing | Codex plugin marketplace; Codewhale plugins are DSH (CW-7051) | C | PluginInstalledParams (untyped) | PluginInstalledResponse (untyped) |
| `plugin/reconcile` | undecided | missing | Codex plugin marketplace; Codewhale plugins are DSH (CW-7051) | C | PluginReconcileParams (untyped) | PluginReconcileResponse (untyped) |
| `plugin/read` | undecided | missing | Codex plugin marketplace; Codewhale plugins are DSH (CW-7051) | C | PluginReadParams (untyped) | PluginReadResponse (untyped) |
| `plugin/skill/read` | undecided | missing | Codex plugin marketplace; Codewhale plugins are DSH (CW-7051) | C | PluginSkillReadParams (untyped) | PluginSkillReadResponse (untyped) |
| `plugin/share/save` | C | method-not-found | OpenAI-hosted plugin sharing; no Codewhale service |  | PluginShareSaveParams (untyped) | PluginShareSaveResponse (untyped) |
| `plugin/share/updateTargets` | C | method-not-found | OpenAI-hosted plugin sharing; no Codewhale service |  | PluginShareUpdateTargetsParams (untyped) | PluginShareUpdateTargetsResponse (untyped) |
| `plugin/share/list` | C | method-not-found | OpenAI-hosted plugin sharing; no Codewhale service |  | PluginShareListParams (untyped) | PluginShareListResponse (untyped) |
| `plugin/share/checkout` | C | method-not-found | OpenAI-hosted plugin sharing; no Codewhale service |  | PluginShareCheckoutParams (untyped) | PluginShareCheckoutResponse (untyped) |
| `plugin/share/delete` | C | method-not-found | OpenAI-hosted plugin sharing; no Codewhale service |  | PluginShareDeleteParams (untyped) | PluginShareDeleteResponse (untyped) |
| `app/read` | C | method-not-found | ChatGPT Apps connectors; no Codewhale service |  | AppsReadParams (untyped) | AppsReadResponse (untyped) |
| `app/list` | C | method-not-found | ChatGPT Apps connectors; no Codewhale service |  | AppsListParams (untyped) | AppsListResponse (untyped) |
| `app/installed` | C | method-not-found | ChatGPT Apps connectors; no Codewhale service |  | AppsInstalledParams (untyped) | AppsInstalledResponse (untyped) |
| `fs/readFile` | B | missing | S9 services: fs |  | `FsReadFileParams` | `FsReadFileResponse` |
| `fs/writeFile` | B | missing | S9 services: fs |  | `FsWriteFileParams` | `FsWriteFileResponse` |
| `fs/createDirectory` | B | missing | S9 services: fs |  | `FsCreateDirectoryParams` | `FsCreateDirectoryResponse` |
| `fs/getMetadata` | B | missing | S9 services: fs |  | `FsGetMetadataParams` | `FsGetMetadataResponse` |
| `fs/readDirectory` | B | missing | S9 services: fs |  | `FsReadDirectoryParams` | `FsReadDirectoryResponse` |
| `fs/remove` | B | missing | S9 services: fs |  | `FsRemoveParams` | `FsRemoveResponse` |
| `fs/copy` | B | missing | S9 services: fs |  | `FsCopyParams` | `FsCopyResponse` |
| `fs/watch` | B | missing | S9 services: fs |  | `FsWatchParams` | `FsWatchResponse` |
| `fs/unwatch` | B | missing | S9 services: fs |  | `FsUnwatchParams` | `FsUnwatchResponse` |
| `skills/config/write` | undecided | missing | skills configuration writes | B | SkillsConfigWriteParams (untyped) | SkillsConfigWriteResponse (untyped) |
| `plugin/install` | undecided | missing | Codex plugin marketplace; Codewhale plugins are DSH (CW-7051) | C | PluginInstallParams (untyped) | PluginInstallResponse (untyped) |
| `plugin/uninstall` | undecided | missing | Codex plugin marketplace; Codewhale plugins are DSH (CW-7051) | C | PluginUninstallParams (untyped) | PluginUninstallResponse (untyped) |
| `turn/start` | A | missing | S4a full-tool turns |  | `TurnStartParams` | `TurnStartResponse` |
| `turn/steer` | A | missing | S4a full-tool turns |  | `TurnSteerParams` | `TurnSteerResponse` |
| `turn/interrupt` | A | missing | S4a full-tool turns |  | `TurnInterruptParams` | `TurnInterruptResponse` |
| `review/start` | B | missing | S9 services: review |  | `ReviewStartParams` | `ReviewStartResponse` |
| `model/list` | B | missing | S9 services: models |  | `ModelListParams` | `ModelListResponse` |
| `account/gatewayOAuth/read` | C | method-not-found | OpenAI gateway OAuth; no Codewhale service |  |  |  |
| `account/gatewayOAuth/login` | C | method-not-found | OpenAI gateway OAuth; no Codewhale service |  |  |  |
| `account/gatewayOAuth/cancel` | C | method-not-found | OpenAI gateway OAuth; no Codewhale service |  |  |  |
| `modelProvider/capabilities/read` | undecided | missing | provider, feature and permission catalogs | B | ModelProviderCapabilitiesReadParams (untyped) | ModelProviderCapabilitiesReadResponse (untyped) |
| `experimentalFeature/list` | undecided | missing | Codex experimental feature flags | C | ExperimentalFeatureListParams (untyped) | ExperimentalFeatureListResponse (untyped) |
| `permissionProfile/list` | undecided | missing | provider, feature and permission catalogs | B | PermissionProfileListParams (untyped) | PermissionProfileListResponse (untyped) |
| `experimentalFeature/enablement/set` | undecided | missing | Codex experimental feature flags | C | ExperimentalFeatureEnablementSetParams (untyped) | ExperimentalFeatureEnablementSetResponse (untyped) |
| `mcpServer/oauth/login` | undecided | missing | MCP actions beyond status | B | McpServerOauthLoginParams (untyped) | McpServerOauthLoginResponse (untyped) |
| `config/mcpServer/reload` | undecided | missing | MCP actions beyond status | B |  |  |
| `mcpServerStatus/list` | B | missing | S9 services: MCP status |  | `ListMcpServerStatusParams` | `ListMcpServerStatusResponse` |
| `mcpServer/resource/read` | undecided | missing | MCP actions beyond status | B | McpResourceReadParams (untyped) | McpResourceReadResponse (untyped) |
| `mcpServer/tool/call` | undecided | missing | MCP actions beyond status | B | McpServerToolCallParams (untyped) | McpServerToolCallResponse (untyped) |
| `windowsSandbox/setupStart` | undecided | missing | Codex Windows sandbox setup; Codewhale has its own AppContainer sandbox | C | WindowsSandboxSetupStartParams (untyped) | WindowsSandboxSetupStartResponse (untyped) |
| `windowsSandbox/readiness` | undecided | missing | Codex Windows sandbox setup; Codewhale has its own AppContainer sandbox | C |  |  |
| `account/login/start` | undecided | missing | OpenAI account sign-in and usage; Codewhale signs in through /auth | C | LoginAccountParams (untyped) | LoginAccountResponse (untyped) |
| `account/login/cancel` | undecided | missing | OpenAI account sign-in and usage; Codewhale signs in through /auth | C | CancelLoginAccountParams (untyped) | CancelLoginAccountResponse (untyped) |
| `account/logout` | undecided | missing | OpenAI account sign-in and usage; Codewhale signs in through /auth | C |  |  |
| `account/rateLimits/read` | undecided | missing | OpenAI account sign-in and usage; Codewhale signs in through /auth | C |  |  |
| `account/rateLimitResetCredit/consume` | C | method-not-found | OpenAI billing and workspace messaging; no Codewhale service |  | ConsumeAccountRateLimitResetCreditParams (untyped) | ConsumeAccountRateLimitResetCreditResponse (untyped) |
| `account/usage/read` | undecided | missing | OpenAI account sign-in and usage; Codewhale signs in through /auth | C |  |  |
| `account/workspaceMessages/read` | C | method-not-found | OpenAI billing and workspace messaging; no Codewhale service |  |  |  |
| `account/sendAddCreditsNudgeEmail` | C | method-not-found | OpenAI billing and workspace messaging; no Codewhale service |  | SendAddCreditsNudgeEmailParams (untyped) | SendAddCreditsNudgeEmailResponse (untyped) |
| `feedback/upload` | C | method-not-found | uploads feedback to OpenAI; no Codewhale service |  | FeedbackUploadParams (untyped) | FeedbackUploadResponse (untyped) |
| `feedback/save` | C | method-not-found | uploads feedback to OpenAI; no Codewhale service |  | FeedbackSaveParams (untyped) | FeedbackSaveResponse (untyped) |
| `command/exec` | B | missing | S9 services: command/exec |  | `CommandExecParams` | `CommandExecResponse` |
| `command/exec/write` | B | missing | S9 services: command/exec |  | `CommandExecWriteParams` | `CommandExecWriteResponse` |
| `command/exec/terminate` | B | missing | S9 services: command/exec |  | `CommandExecTerminateParams` | `CommandExecTerminateResponse` |
| `command/exec/resize` | B | missing | S9 services: command/exec |  | `CommandExecResizeParams` | `CommandExecResizeResponse` |
| `config/read` | B | missing | S9 services: config |  | `ConfigReadParams` | `ConfigReadResponse` |
| `externalAgentConfig/detect` | undecided | missing | import config from other agents; Codewhale external agents are CW-7879 | C | ExternalAgentConfigDetectParams (untyped) | ExternalAgentConfigDetectResponse (untyped) |
| `externalAgentConfig/import` | undecided | missing | import config from other agents; Codewhale external agents are CW-7879 | C | ExternalAgentConfigImportParams (untyped) | ExternalAgentConfigImportResponse (untyped) |
| `externalAgentConfig/import/recordHistory` | undecided | missing | import config from other agents; Codewhale external agents are CW-7879 | C | ExternalAgentConfigImportHistoryRecordParams (untyped) | ExternalAgentConfigImportHistoryRecordResponse (untyped) |
| `externalAgentConfig/import/readHistories` | undecided | missing | import config from other agents; Codewhale external agents are CW-7879 | C |  |  |
| `config/value/write` | B | missing | S9 services: config |  | `ConfigValueWriteParams` | `ConfigWriteResponse` |
| `config/batchWrite` | B | missing | S9 services: config |  | `ConfigBatchWriteParams` | `ConfigWriteResponse` |
| `configRequirements/read` | undecided | missing | managed config requirements | B |  |  |
| `account/read` | undecided | missing | OpenAI account sign-in and usage; Codewhale signs in through /auth | C | GetAccountParams (untyped) | GetAccountResponse (untyped) |
| `getConversationSummary` | C | method-not-found | deprecated v1 API (upstream marks it DEPRECATED; TypeScript-only in the export) |  | GetConversationSummaryParams (untyped) | GetConversationSummaryResponse (untyped) |
| `gitDiffToRemote` | C | method-not-found | deprecated v1 API (upstream marks it DEPRECATED; TypeScript-only in the export) |  | GitDiffToRemoteParams (untyped) | GitDiffToRemoteResponse (untyped) |
| `getAuthStatus` | C | method-not-found | deprecated v1 API (upstream marks it DEPRECATED; TypeScript-only in the export) |  | GetAuthStatusParams (untyped) | GetAuthStatusResponse (untyped) |
| `fuzzyFileSearch` | undecided | missing | file search | B | FuzzyFileSearchParams (untyped) | FuzzyFileSearchResponse (untyped) |

## Client notifications

| Method | Tier | Status | Basis | Recommended | Params | Result |
| --- | --- | --- | --- | --- | --- | --- |
| `initialized` | A | implemented | connection lifecycle (CW-7704; on main) |  |  |  |

## Server requests

| Method | Tier | Status | Basis | Recommended | Params | Result |
| --- | --- | --- | --- | --- | --- | --- |
| `item/commandExecution/requestApproval` | A | missing | S6 interactions (CW-7704: bidirectional approvals, input, client tools) |  | `CommandExecutionRequestApprovalParams` | `CommandExecutionRequestApprovalResponse` |
| `item/fileChange/requestApproval` | A | missing | S6 interactions (CW-7704: bidirectional approvals, input, client tools) |  | `FileChangeRequestApprovalParams` | `FileChangeRequestApprovalResponse` |
| `item/tool/requestUserInput` | A | missing | S6 interactions (CW-7704: bidirectional approvals, input, client tools) |  | `ToolRequestUserInputParams` | `ToolRequestUserInputResponse` |
| `mcpServer/elicitation/request` | undecided | missing | MCP elicitation | A | McpServerElicitationRequestParams (untyped) | McpServerElicitationRequestResponse (untyped) |
| `item/permissions/requestApproval` | A | missing | S6 interactions (CW-7704: bidirectional approvals, input, client tools) |  | `PermissionsRequestApprovalParams` | `PermissionsRequestApprovalResponse` |
| `item/tool/call` | A | missing | S6 interactions (CW-7704: bidirectional approvals, input, client tools) |  | `DynamicToolCallParams` | `DynamicToolCallResponse` |
| `account/chatgptAuthTokens/refresh` | C | never-sent | ChatGPT externally managed auth tokens; no Codewhale service |  | ChatgptAuthTokensRefreshParams (untyped) | ChatgptAuthTokensRefreshResponse (untyped) |
| `attestation/generate` | C | never-sent | OpenAI request attestation; no Codewhale service |  | AttestationGenerateParams (untyped) | AttestationGenerateResponse (untyped) |
| `applyPatchApproval` | C | never-sent | deprecated v1 approvals for legacy turns; superseded by item/*/requestApproval |  | ApplyPatchApprovalParams (untyped) | ApplyPatchApprovalResponse (untyped) |
| `execCommandApproval` | C | never-sent | deprecated v1 approvals for legacy turns; superseded by item/*/requestApproval |  | ExecCommandApprovalParams (untyped) | ExecCommandApprovalResponse (untyped) |

## Server notifications

| Method | Tier | Status | Basis | Recommended | Params | Result |
| --- | --- | --- | --- | --- | --- | --- |
| `error` | A | missing | S3 projection: errors |  | `ErrorNotification` |  |
| `thread/started` | A | missing | S3 projection: thread lifecycle |  | `ThreadStartedNotification` |  |
| `thread/status/changed` | A | missing | S3 projection: thread lifecycle |  | `ThreadStatusChangedNotification` |  |
| `thread/readState/changed` | undecided | missing | thread state beyond the Tier A set | A | ThreadReadStateChangedNotification (untyped) |  |
| `thread/archived` | A | missing | S3 projection: thread lifecycle |  | `ThreadArchivedNotification` |  |
| `thread/deleted` | B | missing | pairs with a Tier B service |  | `ThreadDeletedNotification` |  |
| `thread/unarchived` | A | missing | S3 projection: thread lifecycle |  | `ThreadUnarchivedNotification` |  |
| `thread/closed` | A | missing | S3 projection: thread lifecycle |  | `ThreadClosedNotification` |  |
| `thread/reverted` | B | missing | pairs with a Tier B service |  | `ThreadRevertedNotification` |  |
| `skills/changed` | B | missing | pairs with a Tier B service |  | `SkillsChangedNotification` |  |
| `thread/name/updated` | A | missing | S3 projection: thread lifecycle |  | `ThreadNameUpdatedNotification` |  |
| `thread/attachment/updated` | undecided | missing | Codex desktop attachments, predictions, projects and environments | C | ThreadAttachmentUpdatedNotification (untyped) |  |
| `thread/goal/updated` | A | missing | S3 projection with S8 goals |  | `ThreadGoalUpdatedNotification` |  |
| `thread/prediction/updated` | undecided | missing | Codex desktop attachments, predictions, projects and environments | C | ThreadPredictionUpdatedNotification (untyped) |  |
| `thread/goal/cleared` | A | missing | S3 projection with S8 goals |  | `ThreadGoalClearedNotification` |  |
| `thread/queue/changed` | undecided | missing | thread state beyond the Tier A set | A | ThreadQueueChangedNotification (untyped) |  |
| `project/changed` | undecided | missing | Codex desktop attachments, predictions, projects and environments | C | ProjectChangedNotification (untyped) |  |
| `thread/project/updated` | undecided | missing | Codex desktop attachments, predictions, projects and environments | C | ThreadProjectUpdatedNotification (untyped) |  |
| `thread/environment/connected` | undecided | missing | Codex desktop attachments, predictions, projects and environments | C | EnvironmentConnectionNotification (untyped) |  |
| `thread/environment/disconnected` | undecided | missing | Codex desktop attachments, predictions, projects and environments | C | EnvironmentConnectionNotification (untyped) |  |
| `thread/settings/updated` | undecided | missing | thread state beyond the Tier A set | A | ThreadSettingsUpdatedNotification (untyped) |  |
| `thread/tokenUsage/updated` | A | missing | S3 projection: thread lifecycle |  | `ThreadTokenUsageUpdatedNotification` |  |
| `turn/started` | A | missing | S3 projection: turn/* |  | `TurnStartedNotification` |  |
| `hook/started` | undecided | missing | hook runs; hooks/list is Tier B | B | HookStartedNotification (untyped) |  |
| `turn/completed` | A | missing | S3 projection: turn/* |  | `TurnCompletedNotification` |  |
| `hook/completed` | undecided | missing | hook runs; hooks/list is Tier B | B | HookCompletedNotification (untyped) |  |
| `turn/diff/updated` | A | missing | S3 projection: turn/* |  | `TurnDiffUpdatedNotification` |  |
| `turn/plan/updated` | A | missing | S3 projection: turn/* |  | `TurnPlanUpdatedNotification` |  |
| `item/started` | A | missing | S3 projection: item/* |  | `ItemStartedNotification` |  |
| `item/autoApprovalReview/started` | undecided | missing | Codex Guardian; Codewhale has its own Auto-Review posture | C | ItemGuardianApprovalReviewStartedNotification (untyped) |  |
| `item/autoApprovalReview/completed` | undecided | missing | Codex Guardian; Codewhale has its own Auto-Review posture | C | ItemGuardianApprovalReviewCompletedNotification (untyped) |  |
| `autoApprovalReview/strictReviewRequired` | undecided | missing | Codex Guardian; Codewhale has its own Auto-Review posture | C | StrictReviewRequiredNotification (untyped) |  |
| `item/completed` | A | missing | S3 projection: item/* |  | `ItemCompletedNotification` |  |
| `rawResponseItem/completed` | C | never-sent | internal-only upstream (Codex Cloud, exact upstream usage); TypeScript-only in the export |  | RawResponseItemCompletedNotification (untyped) |  |
| `rawResponse/completed` | C | never-sent | internal-only upstream (Codex Cloud, exact upstream usage); TypeScript-only in the export |  | RawResponseCompletedNotification (untyped) |  |
| `item/agentMessage/delta` | A | missing | S3 projection: item/* |  | `AgentMessageDeltaNotification` |  |
| `item/plan/delta` | A | missing | S3 projection: item/* |  | `PlanDeltaNotification` |  |
| `command/exec/outputDelta` | B | missing | pairs with a Tier B service |  | `CommandExecOutputDeltaNotification` |  |
| `process/outputDelta` | undecided | missing | background processes | B | ProcessOutputDeltaNotification (untyped) |  |
| `process/exited` | undecided | missing | background processes | B | ProcessExitedNotification (untyped) |  |
| `item/commandExecution/outputDelta` | A | missing | S3 projection: item/* |  | `CommandExecutionOutputDeltaNotification` |  |
| `item/commandExecution/terminalInteraction` | A | missing | S3 projection: item/* |  | `TerminalInteractionNotification` |  |
| `item/fileChange/outputDelta` | A | missing | S3 projection: item/* |  | `FileChangeOutputDeltaNotification` |  |
| `item/fileChange/patchUpdated` | A | missing | S3 projection: item/* |  | `FileChangePatchUpdatedNotification` |  |
| `serverRequest/resolved` | A | missing | S6 interactions (CW-7704) |  | `ServerRequestResolvedNotification` |  |
| `item/mcpToolCall/progress` | A | missing | S3 projection: item/* |  | `McpToolCallProgressNotification` |  |
| `mcpServer/oauthLogin/completed` | undecided | missing | MCP server events | B | McpServerOauthLoginCompletedNotification (untyped) |  |
| `mcpServer/startupStatus/updated` | undecided | missing | MCP server events | B | McpServerStatusUpdatedNotification (untyped) |  |
| `mcpServer/event/stream/notification` | undecided | missing | MCP server events | B | McpServerEventStreamNotification (untyped) |  |
| `account/updated` | undecided | missing | OpenAI account state | C | AccountUpdatedNotification (untyped) |  |
| `account/gatewayOAuth/changed` | C | never-sent | pairs with a Tier C method |  | GatewayOAuthChangedNotification (untyped) |  |
| `account/rateLimits/updated` | undecided | missing | OpenAI account state | C | AccountRateLimitsUpdatedNotification (untyped) |  |
| `app/list/updated` | C | never-sent | pairs with a Tier C method |  | AppListUpdatedNotification (untyped) |  |
| `remoteControl/status/changed` | undecided | missing | remote control status | C | RemoteControlStatusChangedNotification (untyped) |  |
| `externalAgentConfig/import/progress` | undecided | missing | pairs with externalAgentConfig/import | C | ExternalAgentConfigImportProgressNotification (untyped) |  |
| `externalAgentConfig/import/completed` | undecided | missing | pairs with externalAgentConfig/import | C | ExternalAgentConfigImportCompletedNotification (untyped) |  |
| `fs/changed` | B | missing | pairs with a Tier B service |  | `FsChangedNotification` |  |
| `item/reasoning/summaryTextDelta` | A | missing | S3 projection: item/* |  | `ReasoningSummaryTextDeltaNotification` |  |
| `item/reasoning/summaryPartAdded` | A | missing | S3 projection: item/* |  | `ReasoningSummaryPartAddedNotification` |  |
| `item/reasoning/textDelta` | A | missing | S3 projection: item/* |  | `ReasoningTextDeltaNotification` |  |
| `thread/compacted` | B | missing | pairs with a Tier B service |  | `ContextCompactedNotification` |  |
| `model/rerouted` | undecided | missing | OpenAI model routing, verification, moderation and safety | C | ModelReroutedNotification (untyped) |  |
| `model/verification` | undecided | missing | OpenAI model routing, verification, moderation and safety | C | ModelVerificationNotification (untyped) |  |
| `modelProvider/authRecoveryStarted` | undecided | missing | provider auth recovery | B | AuthRecoveryNotification (untyped) |  |
| `modelProvider/authRecoveryCompleted` | undecided | missing | provider auth recovery | B | AuthRecoveryNotification (untyped) |  |
| `turn/moderationMetadata` | undecided | missing | OpenAI model routing, verification, moderation and safety | C | TurnModerationMetadataNotification (untyped) |  |
| `model/safetyBuffering/updated` | undecided | missing | OpenAI model routing, verification, moderation and safety | C | ModelSafetyBufferingUpdatedNotification (untyped) |  |
| `warning` | undecided | missing | generic warnings | A | WarningNotification (untyped) |  |
| `guardianWarning` | undecided | missing | Codex Guardian; Codewhale has its own Auto-Review posture | C | GuardianWarningNotification (untyped) |  |
| `deprecationNotice` | undecided | missing | generic warnings | A | DeprecationNoticeNotification (untyped) |  |
| `configWarning` | undecided | missing | generic warnings | A | ConfigWarningNotification (untyped) |  |
| `fuzzyFileSearch/sessionUpdated` | undecided | missing | pairs with fuzzyFileSearch | B | FuzzyFileSearchSessionUpdatedNotification (untyped) |  |
| `fuzzyFileSearch/sessionCompleted` | undecided | missing | pairs with fuzzyFileSearch | B | FuzzyFileSearchSessionCompletedNotification (untyped) |  |
| `thread/realtime/started` | undecided | missing | realtime audio sessions; no Codewhale equivalent | C | ThreadRealtimeStartedNotification (untyped) |  |
| `thread/realtime/itemAdded` | undecided | missing | realtime audio sessions; no Codewhale equivalent | C | ThreadRealtimeItemAddedNotification (untyped) |  |
| `thread/realtime/item/started` | undecided | missing | realtime audio sessions; no Codewhale equivalent | C | ThreadRealtimeItemStartedNotification (untyped) |  |
| `thread/realtime/item/transcript/delta` | undecided | missing | realtime audio sessions; no Codewhale equivalent | C | ThreadRealtimeItemTranscriptDeltaNotification (untyped) |  |
| `thread/realtime/item/completed` | undecided | missing | realtime audio sessions; no Codewhale equivalent | C | ThreadRealtimeItemCompletedNotification (untyped) |  |
| `thread/realtime/transcript/delta` | undecided | missing | realtime audio sessions; no Codewhale equivalent | C | ThreadRealtimeTranscriptDeltaNotification (untyped) |  |
| `thread/realtime/transcript/done` | undecided | missing | realtime audio sessions; no Codewhale equivalent | C | ThreadRealtimeTranscriptDoneNotification (untyped) |  |
| `thread/realtime/outputAudio/delta` | undecided | missing | realtime audio sessions; no Codewhale equivalent | C | ThreadRealtimeOutputAudioDeltaNotification (untyped) |  |
| `thread/realtime/sdp` | undecided | missing | realtime audio sessions; no Codewhale equivalent | C | ThreadRealtimeSdpNotification (untyped) |  |
| `thread/realtime/error` | undecided | missing | realtime audio sessions; no Codewhale equivalent | C | ThreadRealtimeErrorNotification (untyped) |  |
| `thread/realtime/closed` | undecided | missing | realtime audio sessions; no Codewhale equivalent | C | ThreadRealtimeClosedNotification (untyped) |  |
| `windows/worldWritableWarning` | undecided | missing | Codex Windows sandbox | C | WindowsWorldWritableWarningNotification (untyped) |  |
| `windowsSandbox/setupCompleted` | undecided | missing | Codex Windows sandbox | C | WindowsSandboxSetupCompletedNotification (untyped) |  |
| `account/login/completed` | undecided | missing | OpenAI account state | C | AccountLoginCompletedNotification (untyped) |  |

## Totals

| Kind | A | B | C | undecided |
| --- | --- | --- | --- | --- |
| Client requests | 19 | 25 | 19 | 46 |
| Client notifications | 1 | 0 | 0 | 0 |
| Server requests | 5 | 0 | 4 | 1 |
| Server notifications | 27 | 6 | 4 | 50 |

Typed definitions generated into `src/v2.rs`: 319.

## Errors

Codex envelopes carry no `jsonrpc` member. ACP keeps its own JSON-RPC 2.0 envelope and is not this contract.

| Name | Code | Meaning |
| --- | --- | --- |
| `PARSE_ERROR` | -32700 | the line or message is not JSON |
| `INVALID_REQUEST` | -32600 | not a valid request envelope, or a request before initialize |
| `METHOD_NOT_FOUND` | -32601 | Tier C or undecided method, or a name the pinned export does not have |
| `INVALID_PARAMS` | -32602 | params do not decode into the selected method's typed params |
| `INTERNAL_ERROR` | -32603 | the Engine failed while serving a valid request |
| `OVERLOADED` | -32001 | upstream Codex transport overload code; the outgoing queue is full |
| `THREAD_NOT_FOUND` | -32004 | the named thread does not exist (existing App Server code) |
| `RUNTIME_UNAVAILABLE` | -32005 | the Engine could not be reached (existing App Server code) |
| `CAPABILITY_UNSUPPORTED` | -32020 | the method exists and is selected, but this thread's executor or placement cannot serve it; `data` is `CapabilityRefusalData`; the request has no side effect and never falls back to local execution |

## Codewhale extensions

These ride on Codex shapes without changing any pinned field. The pinned `Thread` schema allows additional properties, so a Codex client ignores the fields. Codewhale notifications live under `codewhale/`, because Codex already uses `thread/environment/*` for its own environments. Extension shapes are declared under `extensions.definitions` in the manifest and generated with the Codex types; none carries a credential.

| Name | Kind | Tier | Status | Shape |
| --- | --- | --- | --- | --- |
| `Thread.executor` | field | A | defined, unpopulated | `ThreadExecutor` `{ kind, agentId, agentName? }`: a thread driven by an external agent over ACP; absent for a native thread. Never an environment value or a credential. |
| `Thread.environment` | field | B | defined, unpopulated | `ThreadPlacement` `{ id, providerKind }` (named apart from Codex `ThreadEnvironment`, which is a different type): where the thread runs; absent means this computer. A thread placed in an environment this build cannot serve refuses with `CAPABILITY_UNSUPPORTED` (`environment_not_supported`) and never runs locally. |
| `codewhale/environment/*` | server notifications | B | reserved, not implemented | environment state changes for placement |
| `ThreadItem.placement` | field | B | reserved, not implemented | where a child or job ran |
| `approval.decided.responder` | Runtime journal field | A | implemented | the client that answered (`desktop`, `phone`, `web`, `tui`, `acp`), as declared by that client |
