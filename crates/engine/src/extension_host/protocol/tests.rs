//! The TypeScript half of the protocol, generated from the Rust types.
//!
//! `schemars` reads each wire type's serde shape (field names, `Option` and
//! `default` fields, `deny_unknown_fields`, tags); [`render`] normalizes that
//! into the small vocabulary the host's validator checks and renders
//! `extension-host/src/protocol.generated.ts`. The committed file must equal
//! the rendering: re-record it with `CODEWHALE_CONFORMANCE_UPDATE=1` (refused
//! under CI, like every conformance golden), then rebuild `dist/`.
//!
//! Known limit: a wire shape outside that vocabulary (floats, inline
//! objects, multi-variant unions in params) panics here rather than being approximated.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use codewhale_app_server_protocol::schema::{
    Def, Field, SchemaMode, Ty, innermost, parse_def, quote, ref_name, typescript_defs,
};
use schemars::generate::SchemaSettings;
use schemars::{Schema, SchemaGenerator};

// `Value` and every wire type come from the parent module.
use super::*;

/// The params type of every method in [`METHODS`].
fn params_schema(method: &str, generator: &mut SchemaGenerator) -> Schema {
    match method {
        "host/initialize" => generator.subschema_for::<InitializeParams>(),
        "host/ping" | "host/shutdown" | "host/ready" => generator.subschema_for::<EmptyParams>(),
        "ext/activate" => generator.subschema_for::<ActivateParams>(),
        "ext/deactivate" => generator.subschema_for::<DeactivateParams>(),
        "tool/call" => generator.subschema_for::<ToolCallParams>(),
        "command/run" => generator.subschema_for::<CommandRunParams>(),
        "hook/evaluate" => generator.subschema_for::<HookEvaluateParams>(),
        "harness/run" => generator.subschema_for::<HarnessRunParams>(),
        "exec/redeem" => generator.subschema_for::<ExecutionRedeemParams>(),
        "mcp/open" => generator.subschema_for::<McpOpenParams>(),
        "mcp/request" => generator.subschema_for::<McpRequestParams>(),
        "mcp/close" => generator.subschema_for::<McpCloseParams>(),
        "proc/launch" => generator.subschema_for::<ProcLaunchParams>(),
        "proc/read" => generator.subschema_for::<ProcSessionParams>(),
        "proc/write" => generator.subschema_for::<ProcWriteParams>(),
        "proc/close" => generator.subschema_for::<ProcSessionParams>(),
        "net/start" => generator.subschema_for::<ProcLaunchParams>(),
        "net/fetch" => generator.subschema_for::<NetFetchParams>(),
        "net/read" | "net/release" => generator.subschema_for::<NetReadParams>(),
        "net/close" => generator.subschema_for::<ProcSessionParams>(),

        "$/cancel" => generator.subschema_for::<CancelParams>(),
        "host/hello" => generator.subschema_for::<HelloParams>(),
        "registry/register" => generator.subschema_for::<RegisterParams>(),
        "registry/unregister" => generator.subschema_for::<UnregisterParams>(),
        "core/call" => generator.subschema_for::<CoreCallParams>(),
        "session/checkpoint" => generator.subschema_for::<SessionCheckpointParams>(),
        "user-questions/ask" => generator.subschema_for::<UserQuestionParams>(),
        "subagent/start" => generator.subschema_for::<SubagentStartParams>(),
        "subagent/result" | "subagent/dispose" => generator.subschema_for::<SubagentHandleParams>(),
        "dsh/broker" => generator.subschema_for::<DshBrokerParams>(),
        "ext/faulted" => generator.subschema_for::<FaultedParams>(),
        "log" => generator.subschema_for::<LogParams>(),
        other => panic!("method `{other}` has no params type in the TypeScript generator"),
    }
}

/// The validator's kind for `ty`: a string enum is inlined, an object is a
/// reference into `SHAPES`.
fn kind(ty: &Ty, defs: &BTreeMap<String, Def>) -> String {
    match ty {
        Ty::Nullable(_) => panic!("host validator does not accept nullable fields"),
        Ty::String => "'string'".into(),
        Ty::Boolean => "'boolean'".into(),
        Ty::Uint => "'uint'".into(),
        Ty::Integer => "'integer'".into(),
        Ty::Object => "'object'".into(),
        Ty::Json => "'json'".into(),
        Ty::Ref(name) => match &defs[name] {
            Def::Object { .. } => format!("{{ ref: {} }}", quote(name)),
            Def::Enum(values) => format!(
                "{{ enum: [{}] }}",
                values
                    .iter()
                    .map(|v| quote(v))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Def::Union(_) => panic!("{name}: the host validator does not check unions"),
        },
        Ty::Const(value) => format!("{{ enum: [{}] }}", quote(value)),
        Ty::Array(item) => format!("{{ items: {} }}", kind(item, defs)),
    }
}

/// The kinds of the `fields` that are (or are not) `required`.
fn kinds(fields: &[Field], required: bool, defs: &BTreeMap<String, Def>) -> String {
    let rendered: Vec<String> = fields
        .iter()
        .filter(|field| field.required == required)
        .map(|field| format!("{}: {}", field.name, kind(&field.ty, defs)))
        .collect();
    if rendered.is_empty() {
        "{}".into()
    } else {
        format!("{{ {} }}", rendered.join(", "))
    }
}

const HEADER: &str = "\
// @generated from the Rust protocol types in crates/engine/src/extension_host/protocol.rs
// by `extension_host::protocol::tests`. Do not edit: change the Rust side, re-record with
//   CODEWHALE_CONFORMANCE_UPDATE=1 cargo test -p codewhale-tui --lib extension_host::protocol
// and rebuild dist/ with `npm run build`.

";

const VALIDATOR_TYPES: &str = "
/** A field's wire kind: the Rust field's serde type, normalized for validation. */
export type Kind =
  | 'string'
  | 'boolean'
  | 'uint'
  | 'integer'
  | 'object'
  | 'json'
  | { readonly ref: string }
  | { readonly enum: readonly string[] }
  | { readonly items: Kind }

/** An object's fields; `strict` is Rust's `deny_unknown_fields`. */
export interface Shape {
  readonly strict: boolean
  readonly required: { readonly [field: string]: Kind }
  readonly optional: { readonly [field: string]: Kind }
}
";

/// Render `protocol.generated.ts` from [`METHODS`] and the wire types.
fn render() -> String {
    let mut generator = SchemaSettings::draft2020_12().into_generator();
    let methods: Vec<(&MethodSpec, String)> = METHODS
        .iter()
        .map(|spec| {
            let schema = params_schema(spec.name, &mut generator);
            let name = ref_name(schema.as_value())
                .unwrap_or_else(|| panic!("{}: params must be a named type", spec.name));
            (spec, name)
        })
        .collect();
    let error = generator.subschema_for::<RpcErrorWire>();
    let error = ref_name(error.as_value()).expect("RpcErrorWire is a named type");
    // Results are typed for the host, not validated by it.
    let _ = generator.subschema_for::<RegisterResult>();
    let _ = generator.subschema_for::<ActivateResult>();
    let _ = generator.subschema_for::<DeactivateResult>();
    let _ = generator.subschema_for::<ToolResultWire>();
    let _ = generator.subschema_for::<CommandResultWire>();
    let _ = generator.subschema_for::<HookVerdictWire>();
    let _ = generator.subschema_for::<crate::tools::user_input::UserInputResponse>();
    let defs: BTreeMap<String, Def> = generator
        .definitions()
        .iter()
        .map(|(name, schema)| (name.clone(), parse_def(name, schema, SchemaMode::Host)))
        .collect();

    // What the validator checks: every params type and the error object,
    // with the objects they reference.
    let mut validated = BTreeSet::new();
    let mut pending: Vec<String> = methods
        .iter()
        .map(|(_, name)| name.clone())
        .chain([error])
        .collect();
    while let Some(name) = pending.pop() {
        if let Def::Object { fields, .. } = &defs[&name]
            && validated.insert(name.clone())
        {
            for field in fields {
                if let Ty::Ref(name) = innermost(&field.ty) {
                    pending.push(name.clone());
                }
            }
        }
    }

    let mut out = String::from(HEADER);
    let magic = std::str::from_utf8(&MAGIC).expect("ASCII magic");
    let _ = writeln!(out, "export const PROTOCOL_VERSION = {PROTOCOL_VERSION}");
    let _ = writeln!(out, "export const MAGIC_ASCII = {}", quote(magic));
    let _ = writeln!(out, "export const HEADER_LEN = {HEADER_LEN}");
    let _ = writeln!(out, "export const MAX_FRAME = {MAX_FRAME}");
    let _ = writeln!(out, "export const MAX_INFLIGHT = {MAX_INFLIGHT}");
    out.push_str(
        "\n/** JSON-RPC error codes used on this channel. */\nexport const ErrorCode = {\n",
    );
    for (name, code) in error_code::ALL {
        let _ = writeln!(out, "  {name}: {code},");
    }
    out.push_str("} as const\n\nexport type Direction = 'core_to_host' | 'host_to_core'\n");
    let services: Vec<String> = DshService::ALL
        .iter()
        .map(|service| quote(service.name()))
        .collect();
    let _ = writeln!(
        out,
        "\nexport const DSH_SERVICES = [{}] as const\nexport type DshService = typeof DSH_SERVICES[number]",
        services.join(", ")
    );
    out.push_str("\n/** Every method either side may send, and the trust tiers it is allowed on; nothing else is admitted. */\nexport const METHODS = [\n");
    for (spec, params) in &methods {
        let tiers: Vec<String> = spec.tiers.iter().map(|tier| quote(tier.name())).collect();
        let _ = writeln!(
            out,
            "  {{ name: {}, direction: {}, request: {}, params: {}, tiers: [{}] }},",
            quote(spec.name),
            quote(spec.direction.as_str()),
            spec.request,
            quote(params),
            tiers.join(", ")
        );
    }
    out.push_str("] as const\n");
    out.push_str(VALIDATOR_TYPES);
    out.push_str("\n/** Every method's params, and an error response's `error`. */\nexport const SHAPES: { readonly [name: string]: Shape } = {\n");
    for name in &validated {
        let Def::Object { strict, fields, .. } = &defs[name] else {
            unreachable!("only objects are validated");
        };
        let _ = writeln!(out, "  {name}: {{");
        let _ = writeln!(out, "    strict: {strict},");
        let _ = writeln!(out, "    required: {},", kinds(fields, true, &defs));
        let _ = writeln!(out, "    optional: {},", kinds(fields, false, &defs));
        out.push_str("  },\n");
    }
    out.push_str("}\n");
    out.push_str(&typescript_defs(&defs, SchemaMode::Host));
    out
}

#[test]
fn typescript_protocol_is_generated_from_the_rust_types() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("extension-host")
        .join("src")
        .join("protocol.generated.ts");
    if let Err(drift) = crate::test_support::check_golden(&path, &render()) {
        panic!(
            "{drift}\nThe TypeScript protocol is generated from protocol.rs; rebuild dist/ after re-recording."
        );
    }
}

/// Authority only the core may hold (CURRENT_DECISIONS §26): the event
/// authority, the store, approval, secrets and credentials, the turn loop,
/// sessions and the prompt. Names remain refused except the exact reviewed
/// invocation-bound checkpoint request, whose state and writer stay in Core.
const CORE_ONLY: &[&str] = &[
    "event",
    "store",
    "approv",
    "secret",
    "credential",
    "token",
    "auth",
    "turn",
    "loop",
    "session",
    "prompt",
];

/// Every method, with why it gives the host no core authority. Adding a
/// method means adding its row here, in review, with that reason.
const REVIEWED: &[(&str, &str, &str)] = &[
    (
        "host_to_core",
        "subagent/start",
        "opaque live root-invocation subagent ticket and bounded prompt, persona and tool narrowing; the existing nested-call gate plans and approves the typed agent start and SharedSubAgentManager owns the child, its loop, limits and receipts; no host-selected model route, widened tools or second executor",
    ),
    (
        "host_to_core",
        "subagent/result",
        "opaque live ticket and a child id this invocation started; reads the manager's settled output only; no session, transcript or other child",
    ),
    (
        "host_to_core",
        "subagent/dispose",
        "opaque live ticket and a child id this invocation started; asks the manager to cancel and dispose its own child; no other child, session or turn",
    ),
    (
        "host_to_core",
        "dsh/broker",
        "opaque live invocation ticket minted per plugin owner, DSH service and session; Rust redeems it under the same owner, tier, host-generation, lifetime and use checks as core/call, takes the session from the ticket and never from the host, and dispatches only to that service's typed Rust arm, which refuses an unknown method or an unimplemented service; no tool name, approval, credential or model route",
    ),
    (
        "host_to_core",
        "user-questions/ask",
        "opaque live root-invocation ticket and bounded question data; the existing Engine owns request identity, human wait, typed answer validation and cancellation; no host-selected session, reply, approval or new turn",
    ),
    (
        "host_to_core",
        "session/checkpoint",
        "opaque live root-invocation ticket only; Core captures its own prefix and the existing host persistence owner acknowledges the exact request; no host-supplied session selector, state, writer, restore or turn authority",
    ),
    (
        "core_to_host",
        "harness/run",
        "pinned Builtin orchestration of an opaque exact Rust-gated job; no launch, environment, approval or session writer",
    ),
    (
        "host_to_core",
        "exec/redeem",
        "Builtin-only host:harness; one single-use Execution grant for a Rust-held caller and prepared launch, current owner/generation/selection checks and bounded process cleanup",
    ),
    (
        "host_to_core",
        "net/start",
        "builtin only; opaque Rust HTTP session selectors and exact decoded operation tickets, shared OAuth/egress authority, bounded revocable response reads, no credential exposure",
    ),
    (
        "host_to_core",
        "net/fetch",
        "builtin only; opaque Rust HTTP session selectors and exact decoded operation tickets, shared OAuth/egress authority, bounded revocable response reads, no credential exposure",
    ),
    (
        "host_to_core",
        "net/read",
        "builtin only; opaque Rust HTTP session selectors and exact decoded operation tickets, shared OAuth/egress authority, bounded revocable response reads, no credential exposure",
    ),
    (
        "host_to_core",
        "net/release",
        "builtin only; opaque Rust HTTP session selectors and exact decoded operation tickets, shared OAuth/egress authority, bounded revocable response reads, no credential exposure",
    ),
    (
        "host_to_core",
        "net/close",
        "builtin only; opaque Rust HTTP session selectors and exact decoded operation tickets, shared OAuth/egress authority, bounded revocable response reads, no credential exposure",
    ),
    (
        "core_to_host",
        "mcp/open",
        "builtin only; Rust mints exact owner/host-generation operation tickets, owns spawn and validates the decoded frame before a pipe write; the SDK only executes the admitted protocol exchange",
    ),
    (
        "core_to_host",
        "mcp/request",
        "builtin only; Rust mints exact owner/host-generation operation tickets, owns spawn and validates the decoded frame before a pipe write; the SDK only executes the admitted protocol exchange",
    ),
    (
        "core_to_host",
        "mcp/close",
        "builtin only; Rust mints exact owner/host-generation operation tickets, owns spawn and validates the decoded frame before a pipe write; the SDK only executes the admitted protocol exchange",
    ),
    (
        "host_to_core",
        "proc/launch",
        "builtin only; Rust mints exact owner/host-generation operation tickets, owns spawn and validates the decoded frame before a pipe write; the SDK only executes the admitted protocol exchange",
    ),
    (
        "host_to_core",
        "proc/read",
        "builtin only; Rust mints exact owner/host-generation operation tickets, owns spawn and validates the decoded frame before a pipe write; the SDK only executes the admitted protocol exchange",
    ),
    (
        "host_to_core",
        "proc/write",
        "builtin only; Rust mints exact owner/host-generation operation tickets, owns spawn and validates the decoded frame before a pipe write; the SDK only executes the admitted protocol exchange",
    ),
    (
        "host_to_core",
        "proc/close",
        "builtin only; Rust mints exact owner/host-generation operation tickets, owns spawn and validates the decoded frame before a pipe write; the SDK only executes the admitted protocol exchange",
    ),
    (
        "core_to_host",
        "host/initialize",
        "the core states its limits; the host answers `{}`",
    ),
    (
        "core_to_host",
        "host/ping",
        "heartbeat; the host answers `{}`",
    ),
    (
        "core_to_host",
        "host/shutdown",
        "bounded teardown, sent by tests only",
    ),
    (
        "core_to_host",
        "ext/activate",
        "the core names the reviewed entry and its hash; the host reports tool names",
    ),
    (
        "core_to_host",
        "ext/deactivate",
        "sent after the core has already revoked the owner",
    ),
    (
        "core_to_host",
        "tool/call",
        "sent only after the core's approval gate has passed the call",
    ),
    (
        "core_to_host",
        "command/run",
        "sent only when the user runs the command themselves; the answer is text or a prompt that the core shows or submits through the ordinary turn",
    ),
    (
        "core_to_host",
        "$/cancel",
        "the core withdraws its own request",
    ),
    (
        "core_to_host",
        "hook/evaluate",
        "the core evaluates a reviewed owner's listener; monotonic proposals are folded and any input revision is re-gated in Rust, with no approval or tool handle exposed",
    ),
    (
        "host_to_core",
        "host/hello",
        "handshake facts the core checks against what it launched",
    ),
    (
        "host_to_core",
        "host/ready",
        "handshake completion; no payload",
    ),
    (
        "host_to_core",
        "registry/register",
        "a proposal the core admits or refuses; an admitted tool always needs approval, and an admitted command only runs when the user invokes it",
    ),
    (
        "host_to_core",
        "registry/unregister",
        "the host can only withdraw its own owner's registration",
    ),
    (
        "host_to_core",
        "core/call",
        "a request the core serves only for a ticket it minted for a call that already passed its gate, then plans and approves through the same gate as a model's call; the host names a tool and an input, never an approval, a card text, an argv, a URL or a ticket's contents",
    ),
    (
        "host_to_core",
        "ext/faulted",
        "a report; the core revokes the owner",
    ),
    (
        "host_to_core",
        "log",
        "diagnostic text the core bounds and escapes",
    ),
    (
        "host_to_core",
        "$/cancel",
        "the host withdraws its own in-flight request; the core cancels that request's task and drops whatever it produces",
    ),
];

/// The CI lint for the host protocol. It covers both directions, host→core
/// requests included: [`METHODS`] is every method either parser admits, and
/// the TypeScript validator admits only its generated copy, so the host can
/// neither send nor answer anything outside it.
#[test]
fn host_protocol_never_gains_core_authority() {
    for spec in METHODS {
        let name = spec.name.to_ascii_lowercase();
        for word in CORE_ONLY {
            assert!(
                !name.contains(word)
                    || (*word == "session"
                        && name == "session/checkpoint"
                        && spec.direction == Direction::HostToCore
                        && spec.request),
                "{} method `{}` reaches for core-only authority (`{word}`); \
                 the extension host must never hold it (CURRENT_DECISIONS §26)",
                spec.direction.as_str(),
                spec.name
            );
        }
    }
    let table: BTreeSet<(&str, &str)> = METHODS
        .iter()
        .map(|spec| (spec.direction.as_str(), spec.name))
        .collect();
    let reviewed: BTreeSet<(&str, &str)> = REVIEWED
        .iter()
        .map(|(direction, name, _)| (*direction, *name))
        .collect();
    assert_eq!(
        table, reviewed,
        "the method table changed: review each method in REVIEWED with why it gives the host no core authority"
    );
    // Nothing is decoded or sent outside the table: every method-shaped
    // literal in this module is a table row.
    let literal = regex::Regex::new(r#""([A-Za-z$][\w$-]*/[A-Za-z_]+)""#).expect("regex");
    let source = include_str!("../protocol.rs");
    let mut seen = 0;
    for capture in literal.captures_iter(source) {
        let name = &capture[1];
        assert!(
            METHODS.iter().any(|spec| spec.name == name),
            "protocol.rs uses method `{name}` outside METHODS"
        );
        seen += 1;
    }
    assert!(
        seen >= METHODS.len(),
        "the source scan matched only {seen} literals"
    );
}

/// The tier rule, against a table with methods reserved for the built-in tier
/// (the production table has none yet): refused to a plugin-tier host in both
/// directions, by the parser and by the sender's check, and open methods stay
/// open to both.
#[test]
fn a_method_reserved_for_the_builtin_tier_is_refused_in_both_directions() {
    const RESERVED: &[MethodSpec] = &[
        MethodSpec {
            tiers: &[HostTier::Builtin],
            ..row(Direction::HostToCore, "test/reserved", true)
        },
        MethodSpec {
            tiers: &[HostTier::Builtin],
            ..row(Direction::CoreToHost, "test/reserved-in", false)
        },
        row(Direction::HostToCore, "test/shared", true),
    ];
    use Direction::{CoreToHost, HostToCore};
    use HostTier::{Builtin, Plugin};

    assert_eq!(
        admit_in(RESERVED, HostToCore, "test/reserved", Some(1), Builtin),
        Ok(Some(1))
    );
    assert_eq!(
        admit_in(RESERVED, CoreToHost, "test/reserved-in", None, Builtin),
        Ok(None)
    );
    for (direction, method, id) in [
        (HostToCore, "test/reserved", Some(1)),
        (CoreToHost, "test/reserved-in", None),
    ] {
        let refused = admit_in(RESERVED, direction, method, id, Plugin).unwrap_err();
        assert!(
            refused.0.contains("not allowed on the plugin tier"),
            "{refused}"
        );
        assert!(allowed_in(RESERVED, direction, method, Builtin));
        assert!(!allowed_in(RESERVED, direction, method, Plugin));
    }
    // Open to both tiers, and a name or direction the table lacks is unknown
    // (not "reserved"), whatever the tier.
    for tier in HostTier::ALL {
        assert_eq!(
            admit_in(RESERVED, HostToCore, "test/shared", Some(2), tier),
            Ok(Some(2))
        );
        assert!(allowed_in(RESERVED, HostToCore, "test/shared", tier));
        for (direction, method) in [(HostToCore, "test/none"), (CoreToHost, "test/reserved")] {
            assert!(
                admit_in(RESERVED, direction, method, Some(3), tier)
                    .unwrap_err()
                    .0
                    .contains("unknown"),
                "{method}"
            );
            assert!(!allowed_in(RESERVED, direction, method, tier));
        }
    }
}

/// The production table: `allowed_on` says what each row says, and the
/// families the design reserves for the built-in tier (the process broker, the
/// fetch proxy and the MCP client; none exist yet) can never be added to the
/// plugin tier by accident.
#[test]
fn production_methods_follow_their_tier_rows_and_reserved_families_stay_builtin_only() {
    for count in [0, 1, 100] {
        let mut questions = json!([{
            "id": "q", "question": "Choose", "header": "",
            "options": (0..count).map(|index| json!({"label": format!("choice-{index}"), "description": ""})).collect::<Vec<_>>(),
            "allow_free_text": true, "multi_select": true,
        }]);
        if count > 0 {
            questions[0]["detail"] = json!("Review this exact plan");
            questions[0]["intent"] =
                json!({"kind":"plan-review", "approve":"choice-0", "call_id":"plan-call"});
        }
        let frame = json!({
            "jsonrpc": "2.0", "id": 87, "method": "user-questions/ask",
            "params": { "owner": {"plugin_id":"question-owner", "owner_token":"opaque", "generation":1}, "ticket":"question-ticket", "questions":questions },
        });
        for tier in HostTier::ALL {
            let parsed = parse_host_message(frame.clone(), tier).unwrap();
            assert_eq!(parsed.to_value(), frame);
            let HostMessage::Request { request, .. } = parsed else {
                panic!("wrong question variant")
            };
            let HostRequest::UserQuestion(params) = *request else {
                panic!("wrong question variant")
            };
            assert_eq!(params.questions[0].options.len(), count);
            assert!(parse_core_message(frame.clone(), tier).is_err());
            for field in ["session_id", "request_id", "require_typed_answers"] {
                let mut forged = frame.clone();
                forged["params"][field] = json!("foreign-selector");
                assert!(parse_host_message(forged, tier).is_err());
            }
        }
    }
    for spec in METHODS {
        for tier in HostTier::ALL {
            assert_eq!(
                allowed_on(spec.direction, spec.name, tier),
                spec.tiers.contains(&tier),
                "{}",
                spec.name
            );
        }
        assert!(!spec.tiers.is_empty(), "{} allows no tier", spec.name);
        if ["proc/", "net/", "mcp/"]
            .iter()
            .any(|family| spec.name.starts_with(family))
        {
            assert_eq!(
                spec.tiers,
                &[HostTier::Builtin],
                "{} is reserved for the built-in tier",
                spec.name
            );
        }
    }
    assert!(!allowed_on(
        Direction::HostToCore,
        "no/such-method",
        HostTier::Builtin
    ));
}

#[test]
fn dsh_broker_is_one_strict_envelope_and_names_only_the_closed_service_list() {
    let frame = json!({
        "jsonrpc": "2.0", "id": 91, "method": "dsh/broker",
        "params": {
            "owner": {"plugin_id": "dsh-owner", "owner_token": "opaque", "generation": 1},
            "ticket": "broker-ticket", "service": "workspace", "method": "read", "input": {"path": "a"},
        },
    });
    for tier in HostTier::ALL {
        let parsed = parse_host_message(frame.clone(), tier).unwrap();
        assert_eq!(parsed.to_value(), frame);
        let HostMessage::Request { request, .. } = parsed else {
            panic!("dsh/broker is a request")
        };
        let HostRequest::DshBroker(params) = *request else {
            panic!("wrong broker variant")
        };
        assert_eq!(params.service, "workspace");
        assert!(parse_core_message(frame.clone(), tier).is_err());
        for field in ["session_id", "call_id", "approval", "name"] {
            let mut forged = frame.clone();
            forged["params"][field] = json!("foreign-selector");
            assert!(parse_host_message(forged, tier).is_err(), "{field}");
        }
        for field in ["owner", "ticket", "service", "method", "input"] {
            let mut missing = frame.clone();
            missing["params"].as_object_mut().unwrap().remove(field);
            assert!(parse_host_message(missing, tier).is_err(), "{field}");
        }
        let mut notification = frame.clone();
        notification.as_object_mut().unwrap().remove("id");
        assert!(parse_host_message(notification, tier).is_err());
        let mut unknown = frame.clone();
        unknown["params"]["service"] = json!("vault");
        assert!(parse_host_message(unknown, tier).is_ok());
    }
    let names: Vec<&str> = DshService::ALL
        .iter()
        .map(|service| service.name())
        .collect();
    assert_eq!(
        names,
        [
            "workspace",
            "llm",
            "network",
            "settings",
            "catalog",
            "terminal",
            "remote"
        ]
    );
    for service in DshService::ALL {
        assert_eq!(DshService::parse(service.name()), Some(service));
    }
    assert_eq!(DshService::parse("Workspace"), None);
    assert_eq!(DshService::parse("vault"), None);
}
