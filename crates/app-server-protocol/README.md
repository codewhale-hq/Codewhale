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

`src/methods.rs` holds the typed Tier A and B Codex method enums. `METHODS.md` is the
committed inventory of every pinned Codex method with its tier and status, and
`tests/methods.rs` checks the enums, the inventory and the pinned export against
each other.

The selected future Codex wire target is official `rust-v0.166.0`, commit
`6ff1e1ca3b30fc8b914720681fcf5fe62e95f856`. Its four-variant envelope and initialize/initialized lifecycle serve JSONL
stdio and WebSocket text messages over the private owner channel. The private
framing retains kernel peer and attach checks, bounded headers and messages, and
ACP's separate JSON-RPC 2.0 envelope. TCP WebSocket listener, exact thread/turn/item
dispatch, subscriptions, interactions and their complete generated SDK remain separate required adoption
slices; legacy compatibility methods do not establish full Codex wire parity.
The existing Runtime journal and native Engine receipt/store authority remain
where they are; no serialized observation creates publication authority.
