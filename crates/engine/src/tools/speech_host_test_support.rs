use crate::extension_host::{ExtensionHostManager, ExtensionHostOptions};
use codewhale_config::ProviderKind;
use codewhale_runtime::config::Config;
use codewhale_runtime::features::FeaturesToml;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub fn manager(node: PathBuf, home: &Path) -> Arc<ExtensionHostManager> {
    Arc::new(ExtensionHostManager::new(ExtensionHostOptions {
        runtime: codewhale_config::extension_host::ExtensionHostRuntime::Node,
        node_override: Some(node),
        root: Some(home.join("host")),
        ..ExtensionHostOptions::default()
    }))
}

pub fn config(url: &str, host: bool) -> Config {
    let mut config = {
        let __cw_config_init_0_0 = Some("xiaomi-mimo".into());
        let mut __cw_config_init_0_value: Config = Config::default();
        __cw_config_init_0_value.provider = __cw_config_init_0_0;
        __cw_config_init_0_value
    };
    config
        .set_provider_base_url_override(
            &config.test_identity_for_kind(ProviderKind::XiaomiMimo),
            Some(url.into()),
        )
        .unwrap();
    config
        .set_provider_api_key_override(
            &config.test_identity_for_kind(ProviderKind::XiaomiMimo),
            Some("local-fixture-only".into()),
        )
        .unwrap();
    config.features = Some(FeaturesToml {
        entries: [("speech_host".into(), host)].into_iter().collect(),
    });
    config
}
pub async fn provider(server: &MockServer, count: u64) {
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices":[{"message":{"audio":{"data":"aGk=","transcript":"hi"}}}]
        })))
        .expect(count)
        .mount(server)
        .await;
}
