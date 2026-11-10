# Codewhale App Server protocol

This crate owns the existing Runtime image, event-envelope, capability and
dynamic-tool contracts, plus the shared RPC envelope and initialization shapes.
Engine, App Server, hooks and the SDK consume that one
family; the former `codewhale_protocol::runtime` module is removed.

The runtime contracts reuse serde and the existing workspace Chrono dependency. The optional `schema` feature uses
the existing locked schemars dependency and shared JSON Schema to TypeScript
renderer. It selects input/output schema contracts from the actual owner. Runtime output
fields reflect serialization while accepted input defaults stay optional.
DynamicToolSpecOutput is the serialized view of the same DynamicToolSpec Rust
type used in thread-created output. Runtime TypeScript uses JSON-serializable
object members (which may omit undefined values); root/array undefined remains
excluded. The existing Native host generator keeps its prior text and
present-value normalization.

Generate outputs with:

```sh
cargo run -p codewhale-app-server-protocol --features schema --bin codewhale-app-server-schema -- --write
```

The command writes `schema/json/runtime.schema.json`,
`schema/typescript/runtime.d.ts`, and the matching packaged SDK declaration.
`--check` compares all three without writing. Normal runtime startup does not
generate files or enable schema machinery.

`contract/codex-v2.json` is the frozen, versioned Codex v2 contract manifest:
every pinned stable Codex method with its tier (A, B, C or undecided), status,
basis and, for selected methods, the upstream variant, params and result types;
the dispatch rule for each tier; the error codes; and the Codewhale extensions.
`src/codegen` reads it with the unmodified pinned export and writes
`src/methods.rs`, `src/v2.rs`, `METHODS.md`,
`schema/json/codex-v2.schema.json` and `schema/typescript/codex-v2.d.ts`:

```sh
cargo run -p codewhale-app-server-protocol --bin codewhale-codex-contract -- --write
```

`--check` compares without writing, and `tests/contract.rs` fails on any drift,
on any disagreement between the manifest and the pinned method sets, and on
any selected shape whose typed Rust value does not round-trip an instance the
pinned schema accepts. `v2::ClientRequestParams`, `v2::ServerNotification`,
`v2::ServerRequestParams`, `v2::ClientResponse` and `v2::ServerRequestResponse`
are the typed envelopes; Codex envelopes carry no `jsonrpc` member. A typed
shape is not a handler: `METHODS.md` status says what main dispatches.

The selected future Codex wire target is official `rust-v0.166.0`, commit
`6ff1e1ca3b30fc8b914720681fcf5fe62e95f856`. Its four-variant envelope and initialize/initialized lifecycle serve JSONL
stdio and WebSocket text messages over the private owner channel. The private
framing retains kernel peer and attach checks, bounded headers and messages, and
ACP's separate JSON-RPC 2.0 envelope. TCP WebSocket listener, exact thread/turn/item
dispatch, subscriptions, interactions and their complete generated SDK remain separate required adoption
slices; legacy compatibility methods do not establish full Codex wire parity.
The existing Runtime journal and native Engine receipt/store authority remain
where they are; no serialized observation creates publication authority.
