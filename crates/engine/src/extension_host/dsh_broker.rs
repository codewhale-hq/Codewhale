use std::future::Future;
use std::pin::Pin;

use codewhale_config::OwnerRef;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::protocol::{DshService, RpcErrorWire, error_code};
use super::{
    dsh_catalog, dsh_llm, dsh_network, dsh_remote, dsh_settings, dsh_terminal, dsh_workspace,
};

pub struct BrokerCall {
    pub owner: OwnerRef,
    pub session_id: String,
    pub service: DshService,
    pub method: String,
    pub input: Value,
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
}

impl BrokerRefusal {
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownService(_) => "unknown_service",
            Self::UnknownMethod { .. } => "unknown_method",
            Self::InvalidInput { .. } => "invalid_input",
            Self::NotAvailable(_) => "not_available",
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
        };
        RpcErrorWire {
            code: rpc,
            message,
            data: Some(json!({ "dsh_refusal": code, "service": service })),
        }
    }
}

pub const TICKET_REFUSED: &str = "ticket_refused";
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
];

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
