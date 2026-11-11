use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use codewhale_config::extension_host::PluginSettings;
use codewhale_execpolicy::ApprovalMode;
use codewhale_runtime::config_persistence::{
    mutate_config_document, set_document_value, unset_document_value,
};
use codewhale_sandbox::SandboxPolicy;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use super::dsh_broker::{BrokerCall, BrokerFuture, BrokerMethods, BrokerRefusal, decode};
use super::plugin_config::{self, PluginConfig};
use super::protocol::DshService;
use crate::tools::spec::ToolContext;

const MAX_INPUT_BYTES: usize = 64 * 1024;
const MAX_OPS: usize = 64;
const MAX_PATH_DEPTH: usize = 16;
const MAX_KEY_CHARS: usize = 128;
const APPLIES: &str = "reload";
const LOG: &str = "codewhale::dsh_settings";

static TURN: LazyLock<Arc<tokio::sync::Mutex<()>>> =
    LazyLock::new(|| Arc::new(tokio::sync::Mutex::new(())));

#[derive(Deserialize)]
#[serde(tag = "method", content = "input", deny_unknown_fields)]
enum SettingsMethod {
    #[serde(rename = "describe")]
    Describe(NoInput),
    #[serde(rename = "update")]
    Update(PatchInput),
    #[serde(rename = "replace")]
    Replace(SectionInput),
    #[serde(rename = "mutate")]
    Mutate(OpsInput),
}

impl BrokerMethods for SettingsMethod {
    const NAMES: &'static [&'static str] = &["describe", "update", "replace", "mutate"];
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoInput {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchInput {
    patch: Map<String, Value>,
    #[serde(default)]
    expected_revision: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SectionInput {
    value: Map<String, Value>,
    #[serde(default)]
    expected_revision: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpsInput {
    ops: Vec<PathOp>,
    #[serde(default)]
    expected_revision: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "lowercase", deny_unknown_fields)]
enum PathOp {
    Set { path: Vec<String>, value: Value },
    Unset { path: Vec<String> },
}

enum Change {
    Update(Map<String, Value>),
    Replace(Map<String, Value>),
    Mutate(Vec<PathOp>),
}

#[derive(Clone)]
pub(super) struct SettingsAuthority {
    pub plugin_name: String,
    pub roots: BTreeSet<String>,
    pub source: Option<PathBuf>,
    pub active_revision: Option<String>,
}

enum Failure {
    Invalid(&'static str),
    Outside,
    Store,
}

struct Written {
    written: bool,
    conflict: bool,
    revision: String,
    roots: Vec<String>,
}

pub(super) fn serve(call: BrokerCall) -> BrokerFuture {
    serve_with(call, None)
}

pub(super) fn serve_with(call: BrokerCall, authority: Option<SettingsAuthority>) -> BrokerFuture {
    Box::pin(async move {
        let Some((authority, source)) = authority.and_then(|authority| {
            let source = authority.source.clone()?;
            (!authority.plugin_name.is_empty() && !authority.roots.is_empty())
                .then_some((authority, source))
        }) else {
            return Err(BrokerRefusal::NotAvailable(DshService::Settings));
        };
        if !fits(&call.input, &mut { MAX_INPUT_BYTES }) {
            return Err(invalid("the settings input is too large"));
        }
        let (change, expected) = match decode::<SettingsMethod>(&call)? {
            SettingsMethod::Describe(NoInput {}) => {
                let writable = write_refusal(call.context.tool()).is_none();
                let plugin = call.owner.plugin_id.clone();
                let cancel = call.context.cancel().clone();
                return describe(plugin, cancel, authority, source, writable).await;
            }
            SettingsMethod::Update(input) => (Change::Update(input.patch), input.expected_revision),
            SettingsMethod::Replace(input) => {
                (Change::Replace(input.value), input.expected_revision)
            }
            SettingsMethod::Mutate(input) => (Change::Mutate(input.ops), input.expected_revision),
        };
        if let Some(reason) = write_refusal(call.context.tool()) {
            tracing::info!(target: LOG, plugin = %call.owner.plugin_id, reason, "settings write refused by the session");
            return Err(refused("settings cannot be changed in this session"));
        }
        let turn = Arc::clone(&TURN).lock_owned().await;
        if call.context.cancel().is_cancelled() {
            return Err(refused("the settings call was withdrawn"));
        }
        let plugin = call.owner.plugin_id.clone();
        let name = authority.plugin_name.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            let _turn = turn;
            write(&source, &authority, change, expected.as_deref())
        })
        .await;
        match outcome {
            Ok(Ok(done)) => {
                tracing::info!(target: LOG, plugin = %plugin, name = %name, roots = ?done.roots, written = done.written, conflict = done.conflict, "plugin settings write");
                Ok(json!({
                    "written": done.written,
                    "conflict": done.conflict,
                    "revision": done.revision,
                    "applies": APPLIES,
                }))
            }
            Ok(Err(Failure::Invalid(message))) => Err(invalid(message)),
            Ok(Err(Failure::Outside)) => Err(refused(
                "that setting is outside this plugin's reviewed settings",
            )),
            Ok(Err(Failure::Store)) | Err(_) => Err(refused("the settings could not be saved")),
        }
    })
}

async fn describe(
    plugin: String,
    cancel: CancellationToken,
    authority: SettingsAuthority,
    source: PathBuf,
    writable: bool,
) -> Result<Value, BrokerRefusal> {
    let turn = Arc::clone(&TURN).lock_owned().await;
    if cancel.is_cancelled() {
        return Err(refused("the settings call was withdrawn"));
    }
    let name = authority.plugin_name.clone();
    let saved = tokio::task::spawn_blocking(move || {
        let _turn = turn;
        let settings = codewhale_runtime::config::read_plugin_settings(&source)?;
        plugin_config::convert(&name, &settings.get(&name).cloned().unwrap_or_default())
    })
    .await;
    let saved = match saved {
        Ok(Ok(saved)) => saved,
        Ok(Err(reason)) => {
            tracing::warn!(target: LOG, plugin = %plugin, reason = %reason, "plugin settings could not be read");
            return Err(refused("the settings could not be read"));
        }
        Err(_) => return Err(refused("the settings could not be read")),
    };
    let value: Map<String, Value> = saved
        .value
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(key, _)| authority.roots.contains(*key))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    Ok(json!({
        "ns": authority.plugin_name,
        "writable": writable,
        "roots": authority.roots,
        "value": value,
        "revision": saved.hash,
        "applied": authority.active_revision.as_ref().map(|active| *active == saved.hash),
        "applies": APPLIES,
    }))
}

fn invalid(message: &str) -> BrokerRefusal {
    BrokerRefusal::InvalidInput {
        service: DshService::Settings,
        message: message.into(),
    }
}

fn refused(message: &str) -> BrokerRefusal {
    BrokerRefusal::Refused {
        service: DshService::Settings,
        message: message.into(),
    }
}

fn write_refusal(context: &ToolContext) -> Option<&'static str> {
    let mut tool = context.clone();
    tool.refresh_live_posture();
    if tool.child_host.is_some() || tool.owner_agent_id.is_some() {
        return Some("the caller is a child agent");
    }
    if tool.tool_authority.is_some() {
        return Some("the caller is a worker under an authority envelope");
    }
    if matches!(tool.approval_mode, ApprovalMode::Never | ApprovalMode::Auto) {
        return Some("the approval posture refuses extension writes");
    }
    match tool.elevated_sandbox_policy {
        None => Some("the session has no posture"),
        Some(SandboxPolicy::ReadOnly { .. }) => Some("the session is read-only"),
        Some(_) => None,
    }
}

fn fits(value: &Value, budget: &mut usize) -> bool {
    let cost = match value {
        Value::String(text) => text.len() + 2,
        Value::Array(items) => {
            if !items.iter().all(|item| fits(item, budget)) {
                return false;
            }
            2
        }
        Value::Object(map) => {
            for (key, item) in map {
                if !fits(item, budget) {
                    return false;
                }
                let Some(left) = budget.checked_sub(key.len() + 3) else {
                    return false;
                };
                *budget = left;
            }
            2
        }
        _ => 8,
    };
    match budget.checked_sub(cost) {
        Some(left) => {
            *budget = left;
            true
        }
        None => false,
    }
}

fn key_ok(key: &str) -> bool {
    !key.is_empty()
        && key.chars().count() <= MAX_KEY_CHARS
        && !key.chars().any(char::is_control)
        && key != plugin_config::TOML_DATETIME_KEY
}

fn storable(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Number(number) => number.is_i64() || number.is_f64(),
        Value::Array(items) => items.iter().all(storable),
        Value::Object(map) => map.iter().all(|(key, item)| key_ok(key) && storable(item)),
        Value::Bool(_) | Value::String(_) => true,
    }
}

fn merge(under: Option<Value>, over: Value) -> Value {
    match (under, over) {
        (Some(Value::Object(mut under)), Value::Object(over)) => {
            for (key, value) in over {
                let merged = merge(under.remove(&key), value);
                under.insert(key, merged);
            }
            Value::Object(under)
        }
        (_, over) => over,
    }
}

fn reviewed<'a>(
    roots: &BTreeSet<String>,
    mut keys: impl Iterator<Item = &'a String>,
) -> Result<(), Failure> {
    if keys.all(|key| roots.contains(key)) {
        Ok(())
    } else {
        Err(Failure::Outside)
    }
}

fn set_at(section: &mut Map<String, Value>, path: &[String], value: Value) -> Result<(), Failure> {
    let Some((key, rest)) = path.split_first() else {
        return Err(Failure::Invalid("a settings path names at least one key"));
    };
    if rest.is_empty() {
        section.insert(key.clone(), value);
        return Ok(());
    }
    match section
        .entry(key.clone())
        .or_insert_with(|| Value::Object(Map::new()))
    {
        Value::Object(inner) => set_at(inner, rest, value),
        _ => Err(Failure::Invalid(
            "a settings path may only pass through tables",
        )),
    }
}

fn unset_at(section: &mut Map<String, Value>, path: &[String]) {
    match path {
        [] => {}
        [key] => {
            section.remove(key);
        }
        [key, rest @ ..] => {
            if let Some(Value::Object(inner)) = section.get_mut(key) {
                unset_at(inner, rest);
            }
        }
    }
}

fn next_config(
    current: &Value,
    roots: &BTreeSet<String>,
    change: Change,
) -> Result<Map<String, Value>, Failure> {
    let mut next = current.as_object().cloned().unwrap_or_default();
    match change {
        Change::Update(patch) => {
            reviewed(roots, patch.keys())?;
            for (key, value) in patch {
                let merged = merge(next.remove(&key), value);
                next.insert(key, merged);
            }
        }
        Change::Replace(section) => {
            reviewed(roots, section.keys())?;
            next.retain(|key, _| !roots.contains(key));
            next.extend(section);
        }
        Change::Mutate(ops) => {
            if ops.len() > MAX_OPS {
                return Err(Failure::Invalid("too many settings edits in one call"));
            }
            for op in ops {
                let (PathOp::Set { path, .. } | PathOp::Unset { path }) = &op;
                if path.is_empty() || path.len() > MAX_PATH_DEPTH {
                    return Err(Failure::Invalid("a settings path names 1 to 16 keys"));
                }
                reviewed(roots, path.iter().take(1))?;
                match op {
                    PathOp::Set { path, value } => set_at(&mut next, &path, value)?,
                    PathOp::Unset { path } => unset_at(&mut next, &path),
                }
            }
        }
    }
    let touched = next
        .iter()
        .filter(|(key, value)| roots.contains(*key) && current.get(key.as_str()) != Some(*value));
    for (key, value) in touched {
        if !key_ok(key) || !storable(value) {
            return Err(Failure::Invalid(
                "settings hold strings, finite numbers, booleans, arrays and tables under plain keys",
            ));
        }
    }
    Ok(next)
}

fn edit_value(value: &Value) -> Option<toml_edit::Value> {
    Some(match value {
        Value::Null => return None,
        Value::Bool(flag) => (*flag).into(),
        Value::Number(number) => match number.as_i64() {
            Some(integer) => integer.into(),
            None if number.is_f64() => number.as_f64()?.into(),
            None => return None,
        },
        Value::String(text) => text.as_str().into(),
        Value::Array(items) => toml_edit::Value::Array(
            items
                .iter()
                .map(edit_value)
                .collect::<Option<toml_edit::Array>>()?,
        ),
        Value::Object(map) => {
            let mut table = toml_edit::InlineTable::new();
            for (key, item) in map {
                table.insert(key.as_str(), edit_value(item)?);
            }
            toml_edit::Value::InlineTable(table)
        }
    })
}

fn store(
    doc: &mut toml_edit::DocumentMut,
    path: &mut Vec<String>,
    before: Option<&Value>,
    after: Option<&Value>,
) -> anyhow::Result<()> {
    match (before, after) {
        (Some(before), Some(after)) if before == after => Ok(()),
        (Some(Value::Object(before)), Some(Value::Object(after))) => {
            let added = after.keys().filter(|key| !before.contains_key(*key));
            for key in before.keys().chain(added) {
                path.push(key.clone());
                store(doc, path, before.get(key), after.get(key))?;
                path.pop();
            }
            Ok(())
        }
        (_, Some(after)) => {
            let value = edit_value(after)
                .ok_or_else(|| anyhow::anyhow!("a settings value is not representable"))?;
            let segments: Vec<&str> = path.iter().map(String::as_str).collect();
            set_document_value(doc, &segments, value)
        }
        (Some(_), None) => {
            let segments: Vec<&str> = path.iter().map(String::as_str).collect();
            unset_document_value(doc, &segments).map(|_| ())
        }
        (None, None) => Ok(()),
    }
}

fn saved(doc: &toml_edit::DocumentMut, name: &str) -> Result<PluginConfig, String> {
    let table: toml::Table =
        toml::from_str(&doc.to_string()).map_err(|_| "the edited config does not parse")?;
    let config = match table
        .get("plugins")
        .and_then(|plugins| plugins.get(name))
        .and_then(|plugin| plugin.get("config"))
    {
        None => None,
        Some(toml::Value::Table(config)) => Some(config.clone()),
        Some(_) => return Err(format!("plugin `{name}` config is not a table")),
    };
    plugin_config::convert(name, &PluginSettings { config })
}

fn write(
    source: &Path,
    authority: &SettingsAuthority,
    change: Change,
    expected: Option<&str>,
) -> Result<Written, Failure> {
    let name = authority.plugin_name.as_str();
    let mut outcome: Result<Written, Failure> = Err(Failure::Store);
    let stored = mutate_config_document(source, |doc| {
        let current = saved(doc, name).map_err(anyhow::Error::msg)?;
        if expected.is_some_and(|expected| expected != current.hash) {
            outcome = Ok(Written {
                written: false,
                conflict: true,
                revision: current.hash,
                roots: Vec::new(),
            });
            return Ok(());
        }
        let next = match next_config(&current.value, &authority.roots, change) {
            Ok(next) => Value::Object(next),
            Err(failure) => {
                outcome = Err(failure);
                anyhow::bail!("the settings change was refused");
            }
        };
        let roots: Vec<String> = authority
            .roots
            .iter()
            .filter(|root| current.value.get(root.as_str()) != next.get(root.as_str()))
            .cloned()
            .collect();
        if roots.is_empty() {
            outcome = Ok(Written {
                written: false,
                conflict: false,
                revision: current.hash,
                roots,
            });
            return Ok(());
        }
        let mut path = vec!["plugins".to_string(), name.to_string(), "config".into()];
        store(doc, &mut path, Some(&current.value), Some(&next))?;
        match saved(doc, name) {
            Ok(after) if after.value == next => {
                outcome = Ok(Written {
                    written: true,
                    conflict: false,
                    revision: after.hash,
                    roots,
                });
                Ok(())
            }
            Ok(_) => anyhow::bail!("the stored settings differ from the requested ones"),
            Err(reason) => {
                outcome = Err(Failure::Invalid(
                    "the settings would not be a valid plugin configuration",
                ));
                anyhow::bail!(reason)
            }
        }
    });
    match stored {
        Ok(()) => outcome,
        Err(error) => {
            tracing::warn!(target: LOG, name = %name, error = %error, "plugin settings were not saved");
            Err(match outcome {
                Err(failure) => failure,
                Ok(_) => Failure::Store,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use codewhale_config::OwnerRef;
    use codewhale_runtime::config::read_plugin_settings;

    use super::super::core_call::InvocationGuard;
    use super::super::dsh_broker::{Arm, TABLE, TestGrants};
    use super::super::plugin_config::{PluginConfigs, activation_hash};
    use super::super::protocol::{DshBrokerParams, RpcErrorWire, error_code};
    use super::super::supervisor::HostRequestContext;
    use super::super::test_support::fake_authority;
    use super::super::tier::HostTier;
    use super::super::{ExtensionHostManager, ExtensionHostOptions};
    use super::*;
    use crate::core::engine::LivePosture;
    use crate::tools::codemode::{ExtensionCaller, NestedCallGate};
    use crate::tools::spec::{
        ToolAuthorityEnvelope, ToolMutationAuthority, ToolShellAuthority, ToolVerificationAuthority,
    };

    const NAME: &str = "fixture.plugin";
    const BEFORE: &str = r#"# the person's own notes stay
allow_shell = true

[plugins."fixture.plugin".config]
model = "small" # chosen by hand
internal = "kept"

[plugins."fixture.plugin".config.limits]
# a comment inside a reviewed table
max = 3
burst = 9

[plugins.other.config]
model = "theirs"
"#;

    thread_local! {
        static AUTHORITY: RefCell<Option<SettingsAuthority>> = const { RefCell::new(None) };
    }

    fn granted_arm(call: BrokerCall) -> BrokerFuture {
        serve_with(call, AUTHORITY.with(|authority| authority.borrow().clone()))
    }

    const GRANTED: &[(DshService, Arm)] = &[(DshService::Settings, granted_arm as Arm)];

    struct Fixture {
        manager: ExtensionHostManager,
        owner: OwnerRef,
        other: OwnerRef,
        context: ToolContext,
        gate: NestedCallGate,
        posture: LivePosture,
        dir: tempfile::TempDir,
        _grants: TestGrants,
    }

    impl Fixture {
        fn new() -> Self {
            let grants = TestGrants::only(&[DshService::Settings, DshService::Workspace]);
            let manager = ExtensionHostManager::new(ExtensionHostOptions::default());
            let (owner, other) = {
                let mut registry = manager.shared.registry.lock().unwrap();
                let mut begin = |id: &str| {
                    let owner = registry
                        .begin_owner(HostTier::Plugin, id, id, Some(fake_authority(id)), "hash")
                        .unwrap();
                    registry.mark_active(&owner);
                    owner
                };
                (begin("a"), begin("b"))
            };
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("config.toml"), BEFORE).unwrap();
            let posture = LivePosture::for_tests(dir.path(), ApprovalMode::Suggest);
            let mut fixture = Self {
                manager,
                owner,
                other,
                context: ToolContext::new(dir.path()),
                gate: NestedCallGate::admitting_for_test().for_extension(
                    ExtensionCaller {
                        origin: "extension:a".to_string(),
                        tool: "a_tool".to_string(),
                        scope: "ext:a@hash".to_string(),
                    },
                    Vec::new(),
                ),
                posture,
                dir,
                _grants: grants,
            };
            fixture.context.live_posture = Some(fixture.posture.clone());
            fixture.in_session("settings-session-a");
            fixture.authorize(&["model", "limits"]);
            fixture
        }

        fn path(&self) -> PathBuf {
            self.dir.path().join("config.toml")
        }

        fn text(&self) -> String {
            std::fs::read_to_string(self.path()).unwrap()
        }

        fn active(&self) -> PluginConfigs {
            let mut configs = PluginConfigs::default();
            configs.replace(&read_plugin_settings(&self.path()).unwrap(), None);
            configs
        }

        fn authorize(&self, roots: &[&str]) {
            let active = activation_hash(&self.active().select(NAME));
            AUTHORITY.with(|authority| {
                *authority.borrow_mut() = Some(SettingsAuthority {
                    plugin_name: NAME.into(),
                    roots: roots.iter().map(|root| (*root).to_string()).collect(),
                    source: Some(self.path()),
                    active_revision: Some(active),
                });
            });
        }

        fn in_session(&mut self, session: &str) {
            self.context = self
                .context
                .clone()
                .with_session_objects(crate::rlm::session::SessionObjectSnapshot::new(
                    session.into(),
                    "fixture-model".into(),
                    self.context.workspace.clone(),
                    None,
                    Arc::new(Vec::new()),
                ))
                .with_origin_turn_id("settings-turn");
        }

        fn begin(&self) -> InvocationGuard {
            self.manager
                .shared
                .core_calls
                .begin(
                    HostTier::Plugin,
                    7,
                    &self.owner,
                    "call-1",
                    self.gate.extension().unwrap().0,
                    &self.context,
                    &self.gate,
                )
                .expect("an invocation")
        }

        async fn broker(
            &self,
            table: &[(DshService, Arm)],
            owner: &OwnerRef,
            ticket: &str,
            method: &str,
            input: Value,
        ) -> Result<Value, RpcErrorWire> {
            let (cx, _violations, _cancel) = HostRequestContext::for_test(40);
            self.broker_in(cx, table, owner, ticket, method, input)
                .await
        }

        async fn broker_in(
            &self,
            cx: HostRequestContext,
            table: &[(DshService, Arm)],
            owner: &OwnerRef,
            ticket: &str,
            method: &str,
            input: Value,
        ) -> Result<Value, RpcErrorWire> {
            self.manager
                .shared
                .core_calls
                .serve_broker_in(
                    table,
                    &self.manager.shared,
                    HostTier::Plugin,
                    7,
                    DshBrokerParams {
                        owner: owner.clone(),
                        ticket: ticket.to_string(),
                        service: "settings".to_string(),
                        method: method.to_string(),
                        input,
                    },
                    cx,
                )
                .await
        }

        async fn call(
            &self,
            ticket: &str,
            method: &str,
            input: Value,
        ) -> Result<Value, RpcErrorWire> {
            self.broker(GRANTED, &self.owner, ticket, method, input)
                .await
        }
    }

    fn ticket(guard: &InvocationGuard, service: &str) -> String {
        guard
            .broker_tickets()
            .unwrap()
            .into_iter()
            .find(|wire| wire.service == service)
            .unwrap()
            .ticket
    }

    fn refusal(result: &Result<Value, RpcErrorWire>) -> (i64, String) {
        let error = result.as_ref().expect_err("a refusal");
        let code = error
            .data
            .as_ref()
            .and_then(|data| data["dsh_refusal"].as_str())
            .unwrap_or_else(|| panic!("untyped refusal: {error:?}"))
            .to_string();
        (error.code, code)
    }

    fn ticket_refused() -> (i64, String) {
        (error_code::REFUSED, "ticket_refused".into())
    }

    fn policy_refused() -> (i64, String) {
        (error_code::REFUSED, "refused".into())
    }

    #[tokio::test]
    async fn settings_are_read_edited_applied_at_restart_and_refused_after_revocation() {
        let f = Fixture::new();
        let guard = f.begin();
        let settings = ticket(&guard, "settings");
        let boot = f.active().select(NAME).unwrap();

        let read = f.call(&settings, "describe", json!({})).await.unwrap();
        assert_eq!(read["ns"], NAME);
        assert_eq!(read["roots"], json!(["limits", "model"]));
        assert_eq!(
            read["value"],
            json!({"model": "small", "limits": {"max": 3, "burst": 9}}),
            "only the reviewed roots are shown"
        );
        assert_eq!(read["revision"], boot.hash.as_str());
        assert_eq!(read["applied"], true);
        assert_eq!(read["applies"], "reload");
        assert_eq!(read["writable"], true);

        let edit = f
            .call(
                &settings,
                "update",
                json!({
                    "patch": {"model": "large", "limits": {"max": 5}},
                    "expected_revision": boot.hash,
                }),
            )
            .await
            .unwrap();
        assert_eq!(edit["written"], true);
        assert_eq!(edit["conflict"], false);
        assert_eq!(edit["applies"], "reload");
        let text = f.text();
        for kept in [
            "# the person's own notes stay",
            "allow_shell = true",
            "# chosen by hand",
            "internal = \"kept\"",
            "# a comment inside a reviewed table",
            "burst = 9",
            "[plugins.other.config]\nmodel = \"theirs\"",
        ] {
            assert!(text.contains(kept), "{kept:?} survives in:\n{text}");
        }
        assert!(text.contains("model = \"large\""), "{text}");
        assert!(text.contains("max = 5"), "{text}");

        let pending = f.call(&settings, "describe", json!({})).await.unwrap();
        assert_eq!(pending["revision"], edit["revision"]);
        assert_eq!(
            pending["applied"], false,
            "the running plugin still has the old config"
        );
        assert_eq!(pending["value"]["model"], "large");
        assert_eq!(pending["value"]["limits"], json!({"max": 5, "burst": 9}));

        let restarted = f.active().select(NAME);
        assert_ne!(
            activation_hash(&restarted),
            boot.hash,
            "a reload re-activates the plugin"
        );
        let restarted = restarted.unwrap();
        assert_eq!(restarted.hash, edit["revision"].as_str().unwrap());
        assert_eq!(restarted.value["model"], "large");
        assert_eq!(restarted.value["internal"], "kept");
        f.authorize(&["model", "limits"]);
        let applied = f.call(&settings, "describe", json!({})).await.unwrap();
        assert_eq!(applied["applied"], true);

        let stale = f
            .call(
                &settings,
                "update",
                json!({"patch": {"model": "stale"}, "expected_revision": boot.hash}),
            )
            .await
            .unwrap();
        assert_eq!(stale["written"], false);
        assert_eq!(stale["conflict"], true);
        assert_eq!(stale["revision"], edit["revision"]);
        assert_eq!(f.text(), text, "a stale edit writes nothing");

        let ops = f
            .call(
                &settings,
                "mutate",
                json!({"ops": [
                    {"op": "set", "path": ["limits", "window", "seconds"], "value": 30},
                    {"op": "unset", "path": ["limits", "burst"]},
                ]}),
            )
            .await
            .unwrap();
        assert_eq!(ops["written"], true);
        let replaced = f
            .call(&settings, "replace", json!({"value": {"model": "tiny"}}))
            .await
            .unwrap();
        assert_eq!(replaced["written"], true);
        let last = f.active().select(NAME).unwrap();
        assert_eq!(
            last.value,
            json!({"model": "tiny", "internal": "kept"}),
            "replace resets the reviewed roots and keeps the rest"
        );
        let same = f
            .call(&settings, "replace", json!({"value": {"model": "tiny"}}))
            .await
            .unwrap();
        assert_eq!(same["written"], false);
        assert_eq!(same["revision"], last.hash.as_str());

        let text = f.text();
        f.manager.shared.core_calls.revoke_owner("a");
        let revoked = f
            .call(&settings, "update", json!({"patch": {"model": "after"}}))
            .await;
        assert_eq!(refusal(&revoked), ticket_refused());
        let unread = f.call(&settings, "describe", json!({})).await;
        assert_eq!(refusal(&unread), ticket_refused());
        assert_eq!(f.text(), text, "a revoked plugin writes nothing");
        drop(guard);
        assert_eq!(f.manager.shared.core_calls.live_tickets(), 0);
    }

    #[tokio::test]
    async fn settings_arm_refuses_foreign_tickets_unknown_methods_and_keys_outside_review() {
        let mut f = Fixture::new();
        let guard_a = f.begin();
        let settings = ticket(&guard_a, "settings");
        let before = f.text();
        let patch = json!({"patch": {"model": "x"}});

        for (owner, presented) in [
            (&f.other, settings.clone()),
            (&f.owner, ticket(&guard_a, "workspace")),
            (&f.owner, guard_a.ticket().to_string()),
        ] {
            let result = f
                .broker(GRANTED, owner, &presented, "update", patch.clone())
                .await;
            assert_eq!(refusal(&result), ticket_refused());
        }

        for method in ["edit", "configuration", "entries", "set", "core/call", ""] {
            let unknown = f.call(&settings, method, json!({})).await;
            assert_eq!(
                refusal(&unknown),
                (error_code::INVALID_PARAMS, "unknown_method".into()),
                "{method:?}"
            );
        }
        for (method, input) in [
            ("describe", json!({"redactSecrets": false})),
            ("update", json!({"ns": "other", "patch": {"model": "x"}})),
            ("update", json!({"patch": {"model": null}})),
            ("update", json!({"patch": {"model": {"": 1}}})),
            (
                "update",
                json!({"patch": {"model": 18446744073709551615u64}}),
            ),
            (
                "update",
                json!({"patch": {"model": "x".repeat(MAX_INPUT_BYTES)}}),
            ),
            ("update", json!({"patch": {"model": "x".repeat(17 * 1024)}})),
            (
                "mutate",
                json!({"ops": [{"op": "set", "path": [], "value": 1}]}),
            ),
            (
                "mutate",
                json!({"ops": [{"op": "set", "path": ["model", "deeper"], "value": 1}]}),
            ),
            (
                "mutate",
                json!({"ops": [{"op": "move", "path": ["model"]}]}),
            ),
            ("replace", json!({"value": "model"})),
        ] {
            let result = f.call(&settings, method, input.clone()).await;
            assert_eq!(
                refusal(&result),
                (error_code::INVALID_PARAMS, "invalid_input".into()),
                "{method} {}",
                input.to_string().chars().take(80).collect::<String>()
            );
        }
        for (method, input) in [
            ("update", json!({"patch": {"internal": "taken"}})),
            (
                "update",
                json!({"patch": {"model": "x", "allow_shell": false}}),
            ),
            (
                "replace",
                json!({"value": {"model": "x", "internal": "taken"}}),
            ),
            (
                "mutate",
                json!({"ops": [{"op": "unset", "path": ["internal"]}]}),
            ),
            (
                "mutate",
                json!({"ops": [
                    {"op": "set", "path": ["model"], "value": "x"},
                    {"op": "set", "path": ["plugins", "other", "config", "model"], "value": "mine"},
                ]}),
            ),
        ] {
            let result = f.call(&settings, method, input).await;
            assert_eq!(refusal(&result), policy_refused(), "{method}");
            assert_eq!(
                result.unwrap_err().message,
                "that setting is outside this plugin's reviewed settings"
            );
        }
        assert_eq!(f.text(), before, "no refused call wrote anything");

        for (method, input) in [
            ("describe", json!({})),
            ("anything", json!({})),
            ("update", patch.clone()),
        ] {
            let withheld = f.broker(TABLE, &f.owner, &settings, method, input).await;
            assert_eq!(
                refusal(&withheld),
                (error_code::NOT_AVAILABLE, "not_available".into()),
                "the landed arm has no authority yet ({method})"
            );
        }
        f.authorize(&[]);
        let unreviewed = f.call(&settings, "describe", json!({})).await;
        assert_eq!(
            refusal(&unreviewed),
            (error_code::NOT_AVAILABLE, "not_available".into()),
            "no reviewed roots is a refusal, not an open table"
        );
        f.authorize(&["model", "limits"]);

        f.in_session("settings-session-b");
        let guard_b = f.begin();
        let settings_b = ticket(&guard_b, "settings");
        drop(guard_a);
        let ended = f.call(&settings, "update", patch.clone()).await;
        assert_eq!(refusal(&ended), ticket_refused());
        assert_eq!(f.text(), before);

        let (cx, _violations, cancel) = HostRequestContext::for_test(41);
        cancel.cancel();
        let cancelled = f
            .broker_in(cx, GRANTED, &f.owner, &settings_b, "update", patch.clone())
            .await;
        assert_eq!(
            refusal(&cancelled),
            (error_code::CANCELLED, "cancelled".into())
        );
        assert_eq!(
            f.text(),
            before,
            "a call cancelled before it starts writes nothing"
        );

        let written = f.call(&settings_b, "update", patch).await.unwrap();
        assert_eq!(written["written"], true);
        drop(guard_b);
    }

    #[tokio::test]
    async fn settings_writes_follow_the_callers_limits_and_the_live_posture() {
        let mut f = Fixture::new();
        let before = f.text();
        let patch = json!({"patch": {"model": "x"}});
        let base = f.context.clone();

        let mut refused_contexts: Vec<(&str, ToolContext)> = Vec::new();
        refused_contexts.push((
            "a child agent",
            base.clone().with_owner_agent("child-1", "child"),
        ));
        let mut worker = base.clone();
        worker.tool_authority = Some(Arc::new(ToolAuthorityEnvelope {
            schema_version: 1,
            owner: "worker".into(),
            authority: ToolMutationAuthority::ScopedWrite,
            network_access: None,
            shell: ToolShellAuthority::default(),
            verification: ToolVerificationAuthority::default(),
            writable_roots: vec![".".into()],
            writable_files: Vec::new(),
            coordination_contracts: Vec::new(),
        }));
        refused_contexts.push(("a worker under an envelope", worker));
        let mut unknown = base.clone();
        unknown.live_posture = None;
        unknown.elevated_sandbox_policy = None;
        refused_contexts.push(("a session with no posture", unknown));
        let mut plan = base.clone();
        plan.live_posture = None;
        plan.elevated_sandbox_policy = Some(SandboxPolicy::read_only());
        refused_contexts.push(("a read-only session", plan));

        for (what, context) in refused_contexts {
            f.context = context;
            let guard = f.begin();
            let settings = ticket(&guard, "settings");
            let result = f.call(&settings, "update", patch.clone()).await;
            assert_eq!(refusal(&result), policy_refused(), "{what}");
            assert_eq!(
                result.unwrap_err().message,
                "settings cannot be changed in this session",
                "{what}: the refusal says no more than that"
            );
            let read = f.call(&settings, "describe", json!({})).await.unwrap();
            assert_eq!(read["writable"], false, "{what}");
            assert_eq!(f.text(), before, "{what}");
        }

        f.context = base;
        let guard = f.begin();
        let settings = ticket(&guard, "settings");
        for mode in [ApprovalMode::Never, ApprovalMode::Auto] {
            f.posture.switch_for_tests(mode);
            let result = f.call(&settings, "update", patch.clone()).await;
            assert_eq!(
                refusal(&result),
                policy_refused(),
                "{mode:?} set after the tool call began"
            );
            assert_eq!(f.text(), before);
        }
        f.posture.switch_for_tests(ApprovalMode::Suggest);
        let allowed = f.call(&settings, "update", patch).await.unwrap();
        assert_eq!(allowed["written"], true);
    }

    #[tokio::test]
    async fn a_settings_write_the_config_writer_cannot_persist_is_refused_and_changes_nothing() {
        let f = Fixture::new();
        let guard = f.begin();
        let settings = ticket(&guard, "settings");
        let before = f.text();
        let patch = json!({"patch": {"model": "x"}});

        let blocked = f.dir.path().join("not-a-directory");
        std::fs::write(&blocked, "a file where the config directory would be").unwrap();
        AUTHORITY.with(|authority| {
            authority.borrow_mut().as_mut().unwrap().source = Some(blocked.join("config.toml"));
        });
        let failed = f.call(&settings, "update", patch.clone()).await;
        assert_eq!(refusal(&failed), policy_refused());
        let message = failed.unwrap_err().message;
        assert_eq!(message, "the settings could not be saved");
        assert!(
            !message.contains("not-a-directory"),
            "the refusal carries no path"
        );
        assert!(!blocked.join("config.toml").exists());
        assert_eq!(
            std::fs::read_to_string(&blocked).unwrap(),
            "a file where the config directory would be"
        );

        f.authorize(&["model", "limits"]);
        std::fs::write(f.path(), "plugins = [unterminated").unwrap();
        let unparsable = f.call(&settings, "update", patch.clone()).await;
        assert_eq!(refusal(&unparsable), policy_refused());
        assert_eq!(
            f.text(),
            "plugins = [unterminated",
            "the file is left as it was"
        );
        let unreadable = f.call(&settings, "describe", json!({})).await;
        assert_eq!(refusal(&unreadable), policy_refused());
        assert_eq!(
            unreadable.unwrap_err().message,
            "the settings could not be read"
        );

        std::fs::write(f.path(), &before).unwrap();
        let recovered = f.call(&settings, "update", patch).await.unwrap();
        assert_eq!(recovered["written"], true);

        std::fs::remove_file(f.path()).unwrap();
        let created = f
            .call(
                &settings,
                "update",
                json!({"patch": {"limits": {"max": 1}}}),
            )
            .await
            .unwrap();
        assert_eq!(created["written"], true);
        assert_eq!(
            f.active().select(NAME).unwrap().value,
            json!({"limits": {"max": 1}}),
            "a first edit creates only the plugin's own table"
        );
    }
}
