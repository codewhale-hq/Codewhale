use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::llm_client::LlmClient;
use crate::llm_client::StreamEventBox;
use codewhale_models::{MessageRequest, MessageResponse};

/// Object-safe model boundary for Engine dependency injection.
///
/// The existing `LlmClient` uses return-position `impl Future`, which is
/// efficient for concrete providers but cannot be placed behind `dyn`. This
/// adapter preserves that provider trait while giving deterministic Engine
/// tests and alternate adapters one injectable boundary.
#[async_trait]
pub trait ModelClient: Send + Sync {
    fn provider_name(&self) -> &str;
    fn model(&self) -> &str;
    fn wire_format(&self) -> Option<codewhale_config::provider::WireFormat> {
        None
    }
    fn cache_route_receipt(&self) -> Option<crate::route_receipt::TurnRouteReceipt> {
        None
    }
    /// Concrete route base for billing classification, when this client can
    /// prove one. Provider-neutral injected clients leave it unknown.
    fn billing_base_url(&self) -> Option<&str> {
        None
    }
    fn route_limits(&self) -> Option<codewhale_config::route::RouteLimits> {
        None
    }
    fn effective_max_output_tokens(&self, requested_model: &str) -> u32 {
        let route = self.effective_route_envelope(requested_model, chrono::Utc::now());
        crate::route_budget::effective_max_output_tokens_for_route(
            route.provider,
            &route.model,
            self.route_limits(),
        )
    }
    fn effective_route_envelope(
        &self,
        requested_model: &str,
        dispatched_at: chrono::DateTime<chrono::Utc>,
    ) -> crate::cost_status::EffectiveRouteEnvelope {
        let provider = crate::config::ProviderKind::parse(self.provider_name())
            .unwrap_or(crate::config::ProviderKind::Custom);
        crate::cost_status::EffectiveRouteEnvelope::capture_observed(
            provider,
            self.provider_name(),
            requested_model,
            self.billing_base_url(),
            dispatched_at,
        )
    }
    async fn create_message(&self, request: MessageRequest) -> Result<MessageResponse>;
    /// Fresh authorization evidence; cache-owning adapters must bypass it.
    async fn create_message_uncached(&self, request: MessageRequest) -> Result<MessageResponse> {
        self.create_message(request).await
    }
    async fn create_message_stream(&self, request: MessageRequest) -> Result<StreamEventBox>;
    #[expect(dead_code)]
    async fn health_check(&self) -> Result<bool>;
}

pub type SharedModelClient = Arc<dyn ModelClient>;

/// Every existing provider client automatically satisfies the injectable
/// boundary. This keeps provider-specific HTTP/routing code behind
/// `LlmClient` while the Engine owns only the object-safe contract.
#[async_trait]
impl<T> ModelClient for T
where
    T: LlmClient + Send + Sync,
{
    fn provider_name(&self) -> &str {
        LlmClient::provider_name(self)
    }

    fn model(&self) -> &str {
        LlmClient::model(self)
    }

    fn wire_format(&self) -> Option<codewhale_config::provider::WireFormat> {
        LlmClient::wire_format(self)
    }
    fn cache_route_receipt(&self) -> Option<crate::route_receipt::TurnRouteReceipt> {
        LlmClient::cache_route_receipt(self)
    }

    fn billing_base_url(&self) -> Option<&str> {
        LlmClient::billing_base_url(self)
    }

    fn route_limits(&self) -> Option<codewhale_config::route::RouteLimits> {
        LlmClient::route_limits(self)
    }

    fn effective_max_output_tokens(&self, requested_model: &str) -> u32 {
        LlmClient::effective_max_output_tokens(self, requested_model)
    }

    fn effective_route_envelope(
        &self,
        requested_model: &str,
        dispatched_at: chrono::DateTime<chrono::Utc>,
    ) -> crate::cost_status::EffectiveRouteEnvelope {
        LlmClient::effective_route_envelope(self, requested_model, dispatched_at)
    }

    async fn create_message(&self, request: MessageRequest) -> Result<MessageResponse> {
        LlmClient::create_message(self, request).await
    }

    async fn create_message_uncached(&self, request: MessageRequest) -> Result<MessageResponse> {
        LlmClient::create_message_uncached(self, request).await
    }

    async fn create_message_stream(&self, request: MessageRequest) -> Result<StreamEventBox> {
        LlmClient::create_message_stream(self, request).await
    }

    async fn health_check(&self) -> Result<bool> {
        LlmClient::health_check(self).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_client_is_object_safe() {
        fn accepts_dyn(_: Option<SharedModelClient>) {}
        accepts_dyn(None);
    }
}
