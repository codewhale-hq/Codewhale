# RFC: Codewhale Workrooms — Chat-native Threaded Agent Work

**Issue:** #3209
**Status:** Future RFC — the Workroom wire shape and link parser are defined
once, outside this repository: `cwc_core::workroom` and
`packages/contracts/src/workroom.js` in codewhale-contracts (authority:
codewhale-platform `crates/cwc-core`), which a Project's room serves. The
earlier copy in `crates/protocol/src/workroom.rs` had no consumers and was
removed. Phase 2 (store, app-server endpoints, `resolve_workroom_link` tool,
TUI inbox) not started
**Date:** 2026-06-17 (status refreshed 2026-07-15)
**Target:** post-v0.9.0; the release label is a maintainer decision, not part
of this design

This document is design scaffolding. This tree carries none of it: the
protocol types and link parser live in codewhale-contracts (see Status), and
runtime endpoints, mobile UI integration, persistent state, and model-visible
tools remain follow-up work.

## 1. Problem

Codewhale agent work currently lives in transient TUI sessions, local Runtime API
threads, Fleet runs, and chat-bridge message loops — each with its own lifecycle,
state representation, and context boundary. There is no first-class abstraction
that:

- lets a user start work on one surface (TUI) and resume it on another (mobile)
- gives a stable, shareable link to a thread of agent work
- attaches GitHub issues/PRs/commits as context without copying transcripts
- records which agent/model produced each event for multi-agent workflows
- provides a unified inbox of mentions, approvals, failures, and completions

## 2. Proposed Abstraction: `Workroom`

A `Workroom` is a durable, addressable container for a threaded conversation
involving one or more agents, models, and human participants. It maps onto
the existing Runtime API thread infrastructure and extends it with:

### 2.1 Core Types

```rust
/// Unique identifier for a workroom, stable across restarts.
pub struct WorkroomId(pub String);  // e.g. "wr_abc123def456"

/// A workroom aggregates threads, members, and metadata.
pub struct Workroom {
    pub id: WorkroomId,
    pub title: String,
    pub workspace: Option<String>,      // repo root or project path
    pub repo_identity: Option<RepoRef>, // GitHub repo identity (owner/name)
    pub owner: String,                  // local user or identity handle
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub visibility: WorkroomVisibility,
}

pub enum WorkroomVisibility {
    Private,
    Shared { allowed_tokens: Vec<String> },
}

/// A thread within a workroom — can be a channel, DM, or linked external ref.
pub struct WorkroomThread {
    pub id: String,
    pub workroom_id: WorkroomId,
    pub title: String,
    pub kind: WorkroomThreadKind,
    pub external_ref: Option<ExternalThreadRef>,
    pub created_at: DateTime<Utc>,
}

pub enum WorkroomThreadKind {
    Channel,
    DirectMessage,
    AgentTask,       // spawned by an agent for sub-work
    ApprovalQueue,   // pending human approvals
    ReceiptLog,      // completed agent receipts
}

/// An external reference that can be attached to a workroom thread.
pub enum ExternalThreadRef {
    GitHubIssue {
        owner: String,
        repo: String,
        number: u64,
    },
    GitHubPullRequest {
        owner: String,
        repo: String,
        number: u64,
    },
    GitHubCommit {
        owner: String,
        repo: String,
        sha: String,
    },
    GitHubCheck {
        owner: String,
        repo: String,
        check_run_id: u64,
    },
}

/// An event within a workroom thread, attributed to an agent/model.
pub struct WorkroomEvent {
    pub id: String,
    pub thread_id: String,
    pub workroom_id: WorkroomId,
    pub timestamp: DateTime<Utc>,
    pub kind: WorkroomEventKind,
    pub agent: Option<AgentAttribution>,
}

pub enum WorkroomEventKind {
    Message { content: String },
    Mention { mentioned_user: String },
    ToolCall { tool_name: String, summary: String },
    ToolResult { tool_name: String, success: bool },
    ApprovalRequest { tool_name: String },
    ArtifactLinked { path: String, kind: String },
    Receipt { summary: String },
    Failure { error: String },
    NeedsHuman { reason: String },
    Resumed,
}

pub struct AgentAttribution {
    pub provider: String,   // e.g. "deepseek"
    pub model: String,      // e.g. "deepseek-v4-pro"
    pub agent_id: String,   // sub-agent or fleet worker id
}

/// A link that can be pasted into any surface and resolved back to a workroom.
pub struct WorkroomLink {
    pub workroom_id: WorkroomId,
    pub thread_id: Option<String>,
    pub event_id: Option<String>,
}
```

### 2.2 Link Format

```
codewhale://workroom/wr_abc123def456
codewhale://workroom/wr_abc123def456/thread/thr_xyz
codewhale://workroom/wr_abc123def456/event/evt_789
```

### 2.3 Mapping to Existing Infrastructure

| Workroom concept | Existing mapping |
|---|---|
| `Workroom` | New abstraction; future persisted state alongside Runtime API threads |
| `WorkroomThread` | Maps to a `ThreadId` in the Runtime API |
| `WorkroomEvent` | Wraps existing thread/fleet events with agent attribution |
| `WorkroomLink` | New URL scheme resolvable by the Runtime API |
| `ExternalThreadRef` | New; metadata-only, no secret/token storage |
| `AgentAttribution` | Extracted from sub-agent metadata and fleet worker identity |

## 3. Planned Runtime API Endpoints

### 3.1 `GET /workrooms`

List all workrooms visible to the authenticated caller.

Response:
```json
{
  "workrooms": [
    {
      "id": "wr_abc123",
      "title": "PR #3231 — DeepInfra support",
      "updated_at": "2026-06-15T12:00:00Z",
      "active_threads": 3
    }
  ]
}
```

### 3.2 `GET /workroom/:id/threads`

List active threads within a workroom.

### 3.3 `GET /workroom/resolve?link=codewhale://workroom/wr_abc/thread/thr_x`

Resolve a workroom link to scoped context (thread metadata, recent events)
without replaying the full transcript.

### 3.4 Planned tool: `resolve_workroom_link`

A model-visible tool that takes a `codewhale://workroom/...` URL and returns
the scoped context (thread title, recent event summaries, external refs). This
should not be registered until the backing runtime resolution behavior exists.

## 4. Security Model

- **Local-first by default.** Persisted workroom state should live under the
  Codewhale home directory alongside existing state. No cloud service is
  assumed.
- **Runtime API auth required.** Planned workroom endpoints must use the same
  `Authorization: Bearer <token>` protection as other runtime surfaces.
- **No secrets in links.** Workroom links contain only opaque IDs, never API
  keys or tokens. Resolution requires local Runtime API access.
- **No secrets in events.** Event payloads must not contain API keys, auth
  tokens, or plaintext credentials. The `ArtifactLinked` event kind references
  paths, not contents.
- **Share semantics.** `WorkroomVisibility::Shared` lists allowed bearer tokens,
  not usernames. The operator controls which tokens can access a workroom.
- **No public links.** There is no unauthenticated read path for workrooms.

## 5. Integration Points

### 5.1 Mobile Control Page

The mobile page at `/mobile` already lists active threads. Replace its
ad-hoc thread listing with the `/workrooms` projection so it renders the
same inbox that the TUI and chat bridges see.

### 5.2 Chat Bridges (Telegram, Feishu)

Chat bridges currently maintain their own message loops. Each bridge should
publish bridge-originated messages as `WorkroomEvent::Message` into a
designated workroom thread, and consume `WorkroomEvent::Mention` events
as bridge notifications.

### 5.3 TUI

The TUI should surface workroom inbox events (mentions, approvals) in the
sidebar, and allow pasting `codewhale://` links into the composer for
context resolution.

## 6. Implementation Plan

### Phase 1: Foundation (this PR)
- [x] RFC design doc
- [x] `WorkroomId`, `Workroom`, `WorkroomThread`, `WorkroomEvent`, `WorkroomLink` types
- [x] `ExternalThreadRef` (GitHub refs as workroom context)
- [x] `AgentAttribution` (multi-agent/model event attribution)
- [x] Security model documentation
- [x] Architecture docs

### Phase 2: Integration (follow-up)
- [ ] Persistent workroom state store
- [ ] Runtime API endpoints: `GET /workrooms`, `GET /workroom/:id/threads`
- [ ] `resolve_workroom_link` tool for link resolution
- [ ] Mobile page consumes workroom projection
- [ ] Chat bridges publish/consume workroom events
- [ ] TUI inbox sidebar
- [ ] Workroom link paste resolution in composer

## 7. Non-goals for Phase 1

- No hosted public Codewhale cloud service
- No default-on Slack/Discord/Feishu/Telegram/GitHub App integration
- No arbitrary public share links without explicit auth story
- No model-specific workroom format
- No migration of existing threads (new workrooms only)
