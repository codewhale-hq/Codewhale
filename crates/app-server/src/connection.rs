use crate::{DispatchError, params_or_object, parse_params};
use codewhale_app_server_protocol::lifecycle::{InitializeParams, InitializeResponse};
use serde_json::Value;

#[derive(Default)]
pub(crate) struct ConnectionSession {
    initialization: Option<InitializeParams>,
}

impl ConnectionSession {
    pub(crate) fn initialized(&self) -> bool {
        self.initialization.is_some()
    }

    pub(crate) fn initialize(&mut self, params: Value) -> Result<Value, DispatchError> {
        if self.initialized() {
            return Err(DispatchError::invalid_request("Already initialized"));
        }
        let params: InitializeParams = parse_params(params_or_object(params))?;
        axum::http::HeaderValue::from_str(&params.client_info.name)
            .map_err(|_| DispatchError::invalid_request("Invalid clientInfo.name"))?;
        if let Some(capabilities) = params.capabilities.as_ref()
            && (capabilities.explicit_gateway_oauth
                || capabilities.request_attestation
                || capabilities.mcp_server_openai_form_elicitation
                || capabilities
                    .opt_out_notification_methods
                    .as_ref()
                    .is_some_and(|methods| !methods.is_empty())
                || capabilities
                    .extensions
                    .as_ref()
                    .is_some_and(|extensions| !extensions.is_empty()))
        {
            return Err(DispatchError::invalid_request(
                "Requested initialization capability is not implemented",
            ));
        }
        let home = codewhale_paths::codewhale_home()
            .map_err(|error| DispatchError::internal(error.to_string()))?
            .filter(|path| path.is_absolute())
            .ok_or_else(|| DispatchError::internal("absolute Codewhale home unavailable"))?;
        let response = InitializeResponse {
            user_agent: format!(
                "Codewhale/{} ({}; {})",
                env!("CARGO_PKG_VERSION"),
                params.client_info.name,
                params.client_info.version
            ),
            codex_home: home,
            platform_family: std::env::consts::FAMILY.to_string(),
            platform_os: std::env::consts::OS.to_string(),
        };
        let value = serde_json::to_value(response)
            .map_err(|error| DispatchError::internal(error.to_string()))?;
        self.initialization = Some(params);
        Ok(value)
    }
}
