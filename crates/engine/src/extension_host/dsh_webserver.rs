use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use codewhale_config::PluginAuthority;

use super::protocol::{
    CoreRequest, PluginRouteDispatchParams, PluginRouteDispatchResult, PluginRouteHeaderWire,
};
use super::supervisor::HostCallError;

pub const MAX_PLUGIN_ROUTE_REQUEST_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_PLUGIN_ROUTE_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
pub const PLUGIN_ROUTE_DISPATCH_DEADLINE: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginRouteRequest {
    pub method: String,
    pub path: String,
    pub query: Option<String>,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginRouteResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PluginRouteError {
    #[error("plugin extension host is unavailable: {0}")]
    HostUnavailable(String),
    #[error("plugin did not register a handler for this route")]
    NotFound,
    #[error("plugin route request body exceeds 4 MiB limit")]
    RequestTooLarge,
    #[error("plugin route response body exceeds 8 MiB limit")]
    ResponseTooLarge,
    #[error("plugin route timed out")]
    Timeout,
    #[error("plugin route execution failed: {0}")]
    ExecutionFailed(String),
}

pub async fn dispatch_plugin_route(
    authority: &PluginAuthority,
    request: PluginRouteRequest,
) -> Result<PluginRouteResponse, PluginRouteError> {
    if request.body.len() > MAX_PLUGIN_ROUTE_REQUEST_BYTES {
        return Err(PluginRouteError::RequestTooLarge);
    }
    let (host, owner) = super::manager()
        .shared
        .live_host_for_plugin_authority(authority)
        .await
        .map_err(PluginRouteError::HostUnavailable)?;
    let path = if request.path.starts_with('/') {
        request.path
    } else {
        format!("/{}", request.path)
    };
    let body_base64 = (!request.body.is_empty()).then(|| BASE64_STANDARD.encode(&request.body));
    let plugin_id = owner.plugin_id.clone();
    let params = PluginRouteDispatchParams {
        owner,
        method: request.method,
        path,
        query: request.query.filter(|q| !q.is_empty()),
        headers: request
            .headers
            .into_iter()
            .map(|(name, value)| PluginRouteHeaderWire { name, value })
            .collect(),
        body_base64,
        deadline_ms: PLUGIN_ROUTE_DISPATCH_DEADLINE.as_millis() as u64,
    };
    let value = host
        .call(CoreRequest::PluginRouteDispatch(params), Some(plugin_id))
        .await
        .map_err(|error| match error {
            HostCallError::Timeout { .. } => PluginRouteError::Timeout,
            HostCallError::Exited(reason) => PluginRouteError::HostUnavailable(reason),
            HostCallError::Rpc { message, .. } => PluginRouteError::ExecutionFailed(message),
            other => PluginRouteError::ExecutionFailed(other.to_string()),
        })?;
    let result: PluginRouteDispatchResult = serde_json::from_value(value)
        .map_err(|error| PluginRouteError::ExecutionFailed(format!("invalid response: {error}")))?;
    if !result.matched {
        return Err(PluginRouteError::NotFound);
    }
    if !(100..=599).contains(&result.status) {
        return Err(PluginRouteError::ExecutionFailed(format!(
            "invalid HTTP status code {}",
            result.status
        )));
    }
    let body = match result.body_base64 {
        Some(encoded) if !encoded.is_empty() => {
            if encoded.len() > (MAX_PLUGIN_ROUTE_RESPONSE_BYTES / 3 + 2) * 4 {
                return Err(PluginRouteError::ResponseTooLarge);
            }
            let decoded = BASE64_STANDARD.decode(&encoded).map_err(|error| {
                PluginRouteError::ExecutionFailed(format!("invalid base64 response body: {error}"))
            })?;
            if decoded.len() > MAX_PLUGIN_ROUTE_RESPONSE_BYTES {
                return Err(PluginRouteError::ResponseTooLarge);
            }
            decoded
        }
        _ => Vec::new(),
    };
    let headers = result
        .headers
        .into_iter()
        .map(|h| (h.name, h.value))
        .collect();
    Ok(PluginRouteResponse {
        status: result.status,
        headers,
        body,
    })
}
