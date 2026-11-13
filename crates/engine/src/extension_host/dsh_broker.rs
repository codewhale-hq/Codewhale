use std::future::Future;
use std::pin::Pin;

use codewhale_config::OwnerRef;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::protocol::{DshService, RpcErrorWire, error_code};
use super::registry::OwnerRegistry;
use super::tier::{HOST_OWNER_PREFIX, HostTier};
use super::{
    dsh_catalog, dsh_llm, dsh_network, dsh_plugins, dsh_remote, dsh_settings, dsh_terminal,
    dsh_workspace,
};
use crate::tools::codemode::{NestedDecision, NestedFailure};
use crate::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};

pub use super::core_call::BrokerContext;

pub struct BrokerCall {
    pub owner: OwnerRef,
    pub session_id: String,
    pub service: DshService,
    pub method: String,
    pub input: Value,
    pub context: BrokerContext,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrokerRefusal {
    UnknownService(String),
    UnknownMethod {
        service: DshService,
        method: String,
    },
    InvalidInput {
        service: DshService,
        message: String,
    },
    NotAvailable(DshService),
    NotGranted(DshService),
    Denied {
        service: DshService,
        message: String,
    },
    Refused {
        service: DshService,
        message: String,
    },
}

impl BrokerRefusal {
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownService(_) => "unknown_service",
            Self::UnknownMethod { .. } => "unknown_method",
            Self::InvalidInput { .. } => "invalid_input",
            Self::NotAvailable(_) => "not_available",
            Self::NotGranted(_) => "not_granted",
            Self::Denied { .. } => "denied",
            Self::Refused { .. } => "refused",
        }
    }

    #[must_use]
    pub fn wire(self) -> RpcErrorWire {
        let code = self.code();
        let (rpc, message, service) = match self {
            Self::UnknownService(name) => (
                error_code::INVALID_PARAMS,
                "dsh/broker names a service the core does not broker".to_string(),
                Value::String(name.chars().take(64).collect()),
            ),
            Self::UnknownMethod { service, method } => (
                error_code::INVALID_PARAMS,
                format!(
                    "the {} broker has no method `{}`",
                    service.name(),
                    method.chars().take(64).collect::<String>()
                ),
                Value::String(service.name().into()),
            ),
            Self::InvalidInput { service, message } => (
                error_code::INVALID_PARAMS,
                format!("invalid {} broker input: {message}", service.name()),
                Value::String(service.name().into()),
            ),
            Self::NotAvailable(service) => (
                error_code::NOT_AVAILABLE,
                format!("the {} broker is not available yet", service.name()),
                Value::String(service.name().into()),
            ),
            Self::NotGranted(service) => (
                error_code::REFUSED,
                format!(
                    "the {} service is not granted to this plugin",
                    service.name()
                ),
                Value::String(service.name().into()),
            ),
            Self::Denied { service, message } => (
                error_code::DENIED,
                message,
                Value::String(service.name().into()),
            ),
            Self::Refused { service, message } => (
                error_code::REFUSED,
                message,
                Value::String(service.name().into()),
            ),
        };
        RpcErrorWire {
            code: rpc,
            message,
            data: Some(json!({ "dsh_refusal": code, "service": service })),
        }
    }

    #[must_use]
    pub fn nested(service: DshService, failure: NestedFailure) -> Self {
        match failure {
            NestedFailure::Rejected {
                decision: NestedDecision::Denied,
                message,
            } => Self::Denied { service, message },
            NestedFailure::Rejected { message, .. } | NestedFailure::Unavailable(message) => {
                Self::Refused { service, message }
            }
        }
    }
}

pub const TICKET_REFUSED: &str = "ticket_refused";
pub const LIMIT: &str = "limit";
pub const CANCELLED: &str = "cancelled";

#[must_use]
pub fn coded(mut error: RpcErrorWire, code: &str) -> RpcErrorWire {
    error.data = Some(json!({ "dsh_refusal": code }));
    error
}

pub type BrokerFuture = Pin<Box<dyn Future<Output = Result<Value, BrokerRefusal>> + Send>>;
pub type Arm = fn(BrokerCall) -> BrokerFuture;

pub const TABLE: &[(DshService, Arm)] = &[
    (DshService::Workspace, dsh_workspace::serve as Arm),
    (DshService::Llm, dsh_llm::serve as Arm),
    (DshService::Network, dsh_network::serve as Arm),
    (DshService::Settings, dsh_settings::serve as Arm),
    (DshService::Catalog, dsh_catalog::serve as Arm),
    (DshService::Terminal, dsh_terminal::serve as Arm),
    (DshService::Remote, dsh_remote::serve as Arm),
    (DshService::Plugins, dsh_plugins::serve as Arm),
];

pub const BUILTIN_SERVICES: &[(&str, &[DshService])] = &[("creator", &[DshService::Plugins])];

#[must_use]
pub fn builtin_services(owner_id: &str) -> &'static [DshService] {
    owner_id
        .strip_prefix(HOST_OWNER_PREFIX)
        .and_then(|module| {
            BUILTIN_SERVICES
                .iter()
                .find(|(id, _)| *id == module)
                .map(|(_, services)| *services)
        })
        .unwrap_or(&[])
}

fn builtin_route(owner: &OwnerRef, service: DshService) -> bool {
    owner.plugin_id.starts_with(HOST_OWNER_PREFIX) || service.builtin_only()
}

#[must_use]
pub fn builtin_granted(
    registry: &OwnerRegistry,
    owner: &OwnerRef,
    service: DshService,
) -> Option<bool> {
    builtin_route(owner, service).then(|| {
        registry.tier_of(owner) == Some(HostTier::Builtin)
            && builtin_services(&owner.plugin_id).contains(&service)
    })
}

#[must_use]
pub fn mintable(tier: HostTier, owner: &OwnerRef, service: DshService, root_agent: bool) -> bool {
    if builtin_route(owner, service) || tier == HostTier::Builtin {
        tier == HostTier::Builtin
            && owner.plugin_id.starts_with(HOST_OWNER_PREFIX)
            && builtin_services(&owner.plugin_id).contains(&service)
            && (root_agent || !service.builtin_only())
    } else {
        tier == HostTier::Plugin
    }
}

#[cfg(test)]
thread_local! {
    static TEST_GRANTS: std::cell::RefCell<Option<Vec<DshService>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub struct TestGrants(Option<Vec<DshService>>);

#[cfg(test)]
impl TestGrants {
    pub fn only(services: &[DshService]) -> Self {
        Self(TEST_GRANTS.with(|grants| grants.replace(Some(services.to_vec()))))
    }
}

#[cfg(test)]
impl Drop for TestGrants {
    fn drop(&mut self) {
        let previous = self.0.take();
        TEST_GRANTS.with(|grants| *grants.borrow_mut() = previous);
    }
}

#[cfg(test)]
pub(super) fn test_grants() -> Option<Vec<DshService>> {
    TEST_GRANTS.with(|grants| grants.borrow().clone())
}

#[cfg(not(test))]
pub(super) fn test_grants() -> Option<Vec<DshService>> {
    None
}

#[must_use]
pub fn offered(service: DshService) -> bool {
    use codewhale_plugins::host_services::HostService;
    HostService::parse(service.name()).is_some_and(codewhale_plugins::host_services::offered)
}

#[must_use]
pub fn granted(registry: &OwnerRegistry, owner: &OwnerRef, service: DshService) -> bool {
    if let Some(granted) = builtin_granted(registry, owner, service) {
        return granted;
    }
    match test_grants() {
        Some(granted) => granted.contains(&service),
        None => offered(service) && registry.host_service_granted(owner, service),
    }
}

#[must_use]
pub fn granted_services(registry: &OwnerRegistry, owner: &OwnerRef) -> Vec<DshService> {
    DshService::EVERY
        .into_iter()
        .filter(|service| granted(registry, owner, *service))
        .collect()
}

#[must_use]
pub fn arm_in(table: &[(DshService, Arm)], service: DshService) -> Option<Arm> {
    table
        .iter()
        .find(|(candidate, _)| *candidate == service)
        .map(|(_, arm)| *arm)
}

#[must_use]
pub fn ticket_method(service: DshService) -> &'static str {
    match service {
        DshService::Workspace => "dsh/broker:workspace",
        DshService::Llm => "dsh/broker:llm",
        DshService::Network => "dsh/broker:network",
        DshService::Settings => "dsh/broker:settings",
        DshService::Catalog => "dsh/broker:catalog",
        DshService::Terminal => "dsh/broker:terminal",
        DshService::Remote => "dsh/broker:remote",
        DshService::Plugins => "dsh/broker:plugins",
    }
}

pub trait BrokerMethods: DeserializeOwned {
    const NAMES: &'static [&'static str];
}

pub fn decode<M: BrokerMethods>(call: &BrokerCall) -> Result<M, BrokerRefusal> {
    if !M::NAMES.contains(&call.method.as_str()) {
        return Err(BrokerRefusal::UnknownMethod {
            service: call.service,
            method: call.method.clone(),
        });
    }
    serde_json::from_value(json!({ "method": call.method, "input": call.input })).map_err(|error| {
        BrokerRefusal::InvalidInput {
            service: call.service,
            message: error.to_string(),
        }
    })
}

pub fn not_available(service: DshService) -> BrokerFuture {
    Box::pin(std::future::ready(Err(BrokerRefusal::NotAvailable(
        service,
    ))))
}

pub const ACT_TOOL: &str = "dsh_broker_act";
pub const REACH_TOOL: &str = "dsh_broker_reach";
pub const GATE_TOOLS: [&str; 2] = [ACT_TOOL, REACH_TOOL];
pub const MAX_ACT_TARGET: usize = 256;
const MAX_OPEN_ACTS: usize = 256;
pub const MAX_OPEN_ACTS_PER_INVOCATION: usize = 4;

#[must_use]
pub fn is_gate_tool(name: &str) -> bool {
    GATE_TOOLS
        .iter()
        .any(|tool| name.eq_ignore_ascii_case(tool))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokerEffect {
    Read,
    Write,
    Reach,
}

impl BrokerEffect {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Reach => "reach",
        }
    }

    #[must_use]
    pub fn gate_tool(self) -> &'static str {
        match self {
            Self::Reach => REACH_TOOL,
            Self::Read | Self::Write => ACT_TOOL,
        }
    }

    fn of(input: &Value) -> Option<Self> {
        serde_json::from_value(input.get("effect")?.clone()).ok()
    }
}

static OPEN_ACTS: std::sync::Mutex<Vec<(usize, Value)>> = std::sync::Mutex::new(Vec::new());

fn open_acts() -> std::sync::MutexGuard<'static, Vec<(usize, Value)>> {
    OPEN_ACTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub struct OpenAct(usize, Value);

impl OpenAct {
    #[must_use]
    pub fn open(input: &Value, holder: usize) -> Option<Self> {
        let mut open = open_acts();
        if open.len() >= MAX_OPEN_ACTS {
            return None;
        }
        open.push((holder, input.clone()));
        Some(Self(holder, input.clone()))
    }
}

impl Drop for OpenAct {
    fn drop(&mut self) {
        let mut open = open_acts();
        if let Some(at) = open
            .iter()
            .position(|(holder, act)| *holder == self.0 && *act == self.1)
        {
            open.swap_remove(at);
        }
    }
}

fn act_is_open(input: &Value) -> bool {
    open_acts().iter().any(|(_, open)| open == input)
}

#[must_use]
pub fn answered_for(input: &Value, result: &Value) -> bool {
    match result.get("content") {
        Some(Value::String(text)) => {
            serde_json::from_str::<Value>(text).is_ok_and(|answered| answered == *input)
        }
        Some(other) => other == input,
        None => false,
    }
}

#[must_use]
pub fn is_gate_spec(spec: &dyn ToolSpec) -> bool {
    is_gate_tool(spec.name())
        && spec.registration_origin() == std::any::type_name::<BrokerActTool>()
}

#[must_use]
pub fn act_description(input: &Value) -> String {
    let field = |key: &str| input.get(key).and_then(Value::as_str).unwrap_or("?");
    let verb = match BrokerEffect::of(input) {
        Some(BrokerEffect::Write) => "change something",
        Some(BrokerEffect::Reach) => "reach outside this session",
        _ => "read",
    };
    let target = input
        .get("target")
        .and_then(Value::as_str)
        .map(|target| format!(" ({target})"))
        .unwrap_or_default();
    format!(
        "plugin `{}` asks to {verb} through the {} service: `{}`{target}",
        field("plugin"),
        field("service"),
        field("method"),
    )
}

pub struct BrokerActTool {
    reach: bool,
}

impl BrokerActTool {
    pub const ACT: Self = Self { reach: false };
    pub const REACH: Self = Self { reach: true };

    #[must_use]
    pub fn all() -> [std::sync::Arc<dyn ToolSpec>; 2] {
        [
            std::sync::Arc::new(Self::ACT),
            std::sync::Arc::new(Self::REACH),
        ]
    }

    fn effects(&self) -> &'static [&'static str] {
        if self.reach {
            &["reach"]
        } else {
            &["read", "write"]
        }
    }

    fn admits(&self, input: &Value) -> Result<(), ToolError> {
        let fits = BrokerEffect::of(input)
            .is_some_and(|effect| (effect == BrokerEffect::Reach) == self.reach);
        if fits && act_is_open(input) {
            Ok(())
        } else {
            Err(ToolError::permission_denied(format!(
                "{} answers only for an act a plugin broker has open",
                self.name()
            )))
        }
    }
}

#[async_trait::async_trait]
impl ToolSpec for BrokerActTool {
    fn name(&self) -> &str {
        if self.reach { REACH_TOOL } else { ACT_TOOL }
    }

    fn description(&self) -> &str {
        "The session's permission gate for one act a plugin asked the host broker for."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "plugin": {"type": "string"},
                "service": {"type": "string"},
                "method": {"type": "string"},
                "effect": {"type": "string", "enum": self.effects()},
                "target": {"type": "string"},
            },
            "required": ["plugin", "service", "method", "effect"],
            "additionalProperties": false,
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        if self.reach {
            vec![ToolCapability::Network]
        } else {
            Vec::new()
        }
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Required
    }

    fn approval_requirement_for(&self, input: &Value) -> ApprovalRequirement {
        match BrokerEffect::of(input) {
            Some(BrokerEffect::Read) => ApprovalRequirement::Auto,
            _ => ApprovalRequirement::Required,
        }
    }

    fn is_read_only(&self) -> bool {
        false
    }

    fn is_read_only_for(&self, input: &Value) -> bool {
        BrokerEffect::of(input) == Some(BrokerEffect::Read)
    }

    fn model_visible(&self) -> bool {
        false
    }

    fn prepare(
        &self,
        input: Value,
        _context: &ToolContext,
    ) -> Result<crate::tools::spec::PreparedToolCall, ToolError> {
        self.admits(&input)?;
        Ok(crate::tools::spec::PreparedToolCall {
            name: self.name().to_string(),
            description: act_description(&input),
            read_only: self.is_read_only_for(&input),
            supports_parallel: false,
            starts_detached: false,
            approval: self.approval_requirement_for(&input),
            resources: vec![crate::tools::spec::ResourceClaim::GlobalExclusive],
            input,
        })
    }

    async fn execute(&self, input: Value, _context: &ToolContext) -> Result<ToolResult, ToolError> {
        self.admits(&input)?;
        Ok(ToolResult::success(input.to_string()))
    }
}
