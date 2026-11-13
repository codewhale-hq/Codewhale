use serde::Deserialize;
use serde_json::{Map, Value};

use super::dsh_broker::{
    BrokerCall, BrokerFuture, BrokerMethods, BrokerRefusal, builtin_services, decode,
};
use super::protocol::DshService;
use crate::tools::spec::ToolContext;

const LOG: &str = "codewhale::dsh_plugins";
const MAX_INPUT_FIELDS: usize = 32;

#[derive(Deserialize)]
#[serde(tag = "method", content = "input", deny_unknown_fields)]
enum PluginsMethod {
    #[serde(rename = "list")]
    List(Map<String, Value>),
    #[serde(rename = "status")]
    Status(Map<String, Value>),
    #[serde(rename = "posture")]
    Posture(Map<String, Value>),
    #[serde(rename = "install")]
    Install(Map<String, Value>),
    #[serde(rename = "remove")]
    Remove(Map<String, Value>),
    #[serde(rename = "enable")]
    Enable(Map<String, Value>),
    #[serde(rename = "disable")]
    Disable(Map<String, Value>),
}

impl BrokerMethods for PluginsMethod {
    const NAMES: &'static [&'static str] = &[
        "list", "status", "posture", "install", "remove", "enable", "disable",
    ];
}

pub(super) fn serve(call: BrokerCall) -> BrokerFuture {
    Box::pin(async move {
        let method = decode::<PluginsMethod>(&call)?;
        if !builtin_services(&call.owner.plugin_id).contains(&DshService::Plugins) {
            tracing::info!(target: LOG, plugin = %call.owner.plugin_id, "plugins refused: not the creator module");
            return Err(refused());
        }
        let tool = live(&call);
        if tool.execution.owner_agent_id.is_some() {
            tracing::info!(target: LOG, "plugins refused: a sub-agent caller");
            return Err(refused());
        }
        if method.input().len() > MAX_INPUT_FIELDS {
            return Err(BrokerRefusal::InvalidInput {
                service: DshService::Plugins,
                message: format!("a plugins input has at most {MAX_INPUT_FIELDS} fields"),
            });
        }
        Err(BrokerRefusal::NotAvailable(DshService::Plugins))
    })
}

impl PluginsMethod {
    fn input(&self) -> &Map<String, Value> {
        match self {
            Self::List(input)
            | Self::Status(input)
            | Self::Posture(input)
            | Self::Install(input)
            | Self::Remove(input)
            | Self::Enable(input)
            | Self::Disable(input) => input,
        }
    }
}

fn live(call: &BrokerCall) -> ToolContext {
    let mut tool = call.context.tool().clone();
    let _ = tool.refresh_live_posture();
    tool
}

fn refused() -> BrokerRefusal {
    BrokerRefusal::Refused {
        service: DshService::Plugins,
        message: "the plugins service is not available to this caller".to_string(),
    }
}
