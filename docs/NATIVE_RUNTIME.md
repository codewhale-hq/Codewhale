# Native Runtime attachment

This narrow `native_runtime_v1` capability extends authenticated local Control.
It does not enable the paused App Server v2 migration or change compatibility
Control's tool-free permissions. It requires an already running captured owner.

Launch the Desktop-selected executable with:

```
codewhale app-server --stdio --attach-only --native-runtime
```

The first JSONL frame is JSON-RPC 2.0 `runtime/attach`, with an ID and closed
params: `contract_version:1`, `target_id`, positive `selection_revision`, absolute
canonical `workspace`, and `expect_owner` (the existing RuntimeOwnerReceipt).
The acknowledgement echoes all binding fields and
`capabilities:["native_runtime_v1"]`. Next send the existing `initialize` request.
The helper validates its acknowledgement and sends `initialized` to the owner.
No operations are accepted before initialization. EOF detaches observations.
There is no startup fallback or automatic mutation replay.

Desktop may add `--describe-target` to the same invocation. Its single stdin
request is `runtime/describe-target`, with params `contract_version:1`,
`workspace`, `expected_pid`, `expected_data_dir`, and optional `config_path`.
The process inherits the selected supervisor's captured environment; in
particular CODEWHALE_HOME and CODEWHALE_RUNTIME_DIR select Engine's own endpoint
resolver. `expected_data_dir` is the actual canonical Runtime store directory,
not a hash. The probe authenticates without creating a store and returns
`contract_version`, `workspace`, `expect_owner`, and `capabilities`. Desktop must
recheck the selected supervisor and folder after this asynchronous operation.
No target-record path or credential-bearing descriptor is part of this API.

The closed request DTOs and method names are owned by
`crates/app-server-protocol/src/runtime.rs` (`NativeRuntimeRequest`). Existing
Runtime results are preserved. Unknown fields and methods are refused.
The connection permits at most 64 concurrent requests and 16 observations,
with bounded 8 MiB frames and output bytes. Partial JSONL input and admitted
request bodies share an 8 MiB byte budget; the existing WebSocket transport
and JSON decoder additionally have finite per-frame buffers. This is a pending
input bound, not a total-process heap limit. A subscription acknowledgement
precedes `runtime/event` notifications carrying the original journal frame.
`stream.end` frames retain their nullable sequence. Unsubscribe only detaches.
Backpressure terminates an observation or closes the channel. Its
`last_sent_sequence` is updated only after a successful writer flush, and is
not an acknowledgement of client processing. Reconnecting
requires a fresh selected receipt and does not replay a mutation.

Account status is read-only. Account lease/import/logout, configuration writes,
claim, shutdown, arbitrary HTTP/RPC, dynamic tool injection, permission escalation
and user-input submission are not exposed. Pending interactive input remains
visible in Runtime thread details and can be answered in Desktop. Native Chrome
message chunking and target-record file validation belong to the native-host
installer/adapter; this Engine surface does not authorize executing a binary
path from an unvalidated target record. Cross-platform source support does not
by itself demonstrate installed Windows ACL or named-pipe acceptance.
