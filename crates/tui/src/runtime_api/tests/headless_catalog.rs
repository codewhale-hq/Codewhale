use super::*;
use codewhale_config::catalog::{
    CatalogOffering, CatalogSource, CatalogStatus, ProviderCatalogDelta, base_url_fingerprint,
    now_unix,
};
use codewhale_config::models_dev::ModelsDevModalities;
use codewhale_config::route::CapabilityState;

const MODEL: &str = "fixture/vision";
const FIRST_ENDPOINT: &str = "http://127.0.0.1:9/first/v1";
const SECOND_ENDPOINT: &str = "http://127.0.0.1:9/second/v1";

struct CatalogReset;

impl Drop for CatalogReset {
    fn drop(&mut self) {
        cold_process();
        codewhale_runtime::active_workshop::install_active_workshop(None);
    }
}

fn cold_process() {
    codewhale_engine::provider_catalog_live::reset_cache_for_test();
    codewhale_engine::provider_lake::clear_live_snapshot();
}

fn persist_catalog(kind: ProviderKind, identity: &str, endpoint: &str, images: bool) {
    let fingerprint = base_url_fingerprint(endpoint);
    let fetched_at = now_unix();
    let ticket = codewhale_engine::provider_catalog_live::begin_refresh_for_identity(
        kind, identity, endpoint,
    );
    assert_eq!(
        codewhale_engine::provider_catalog_live::record_success_if_current(
            &ticket,
            ProviderCatalogDelta {
                provider: identity.into(),
                base_url_fingerprint: fingerprint.clone(),
                fetched_at,
                offerings: vec![CatalogOffering {
                    provider: identity.into(),
                    wire_model_id: MODEL.into(),
                    endpoint_key: "chat".into(),
                    modalities: Some(ModelsDevModalities {
                        input: if images {
                            vec!["text".into(), "image".into()]
                        } else {
                            vec!["text".into()]
                        },
                        output: vec!["text".into()],
                    }),
                    source: CatalogSource::Live {
                        base_url_fingerprint: fingerprint,
                        fetched_at,
                    },
                    ..Default::default()
                }],
            },
        ),
        Some(CatalogStatus::Fresh)
    );
}

fn config_for(identity: &str, endpoint: &str) -> Config {
    let mut config = {
        let __cw_config_init_0_0 = Some(identity.into());
        let mut __cw_config_init_0_value: Config = Default::default();
        __cw_config_init_0_value.provider = __cw_config_init_0_0;
        __cw_config_init_0_value
    };
    let route = if identity == "openrouter" {
        &mut config
            .providers
            .get_or_insert_with(Default::default)
            .openrouter
    } else {
        let route = config
            .providers
            .get_or_insert_with(Default::default)
            .custom
            .entry(identity.into())
            .or_default();
        route.kind = Some("openai-compatible".into());
        route
    };
    route.base_url = Some(endpoint.into());
    route.api_key = Some("synthetic-headless-catalog-key".into());
    route.model = Some(MODEL.into());
    config
}

fn image_capability(config: &Config) -> CapabilityState {
    provider_model_image_input_for_api(config, &config.active_provider_identity().unwrap(), MODEL)
}

fn open_server_manager(config: &Config, root: &Path) -> Result<SharedRuntimeThreadManager> {
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace)?;
    let (manager, _) = open_runtime_threads_for_server(
        config,
        workspace.clone(),
        RuntimeThreadManagerConfig {
            data_dir: root.join("runtime"),
            task_data_dir: root.join("tasks"),
            sessions_dir: None,
            max_active_threads: 2,
        },
        Arc::new(codewhale_plugins::PluginRegistry::empty(&workspace)),
    )?;
    Ok(manager)
}

#[test]
fn headless_startup_publishes_cold_endpoint_catalog_capabilities() -> Result<()> {
    // `active_workshop_test_guard()` takes the env barrier before the workshop
    // gate and holds both (#6306), so it is the whole lock acquisition. Taking
    // `lock_test_env()` again here parks this thread on a non-reentrant mutex
    // it already owns; the ordering that #6049 hand-rolled now lives in the guard.
    let _workshop = codewhale_runtime::active_workshop::active_workshop_test_guard();
    let _offline = EnvVarGuard::set("CODEWHALE_DISABLE_CLOUD_FACTS", "1");
    let _live = codewhale_engine::provider_lake::lock_live_snapshot();
    let home = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
    let _reset = CatalogReset;
    cold_process();
    persist_catalog(ProviderKind::Openrouter, "openrouter", FIRST_ENDPOINT, true);
    cold_process();

    let config = config_for("openrouter", FIRST_ENDPOINT);
    assert!(
        provider_models_for_api(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Openrouter)
        )
        .contains(&MODEL.to_string())
    );
    assert_eq!(image_capability(&config), CapabilityState::Unknown);
    let _manager = open_server_manager(&config, home.path())?;
    assert_eq!(image_capability(&config), CapabilityState::Supported);
    assert_eq!(
        image_capability(&config_for("openrouter", SECOND_ENDPOINT)),
        CapabilityState::Unknown,
        "another endpoint must not inherit the saved capabilities"
    );
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn headless_reload_publishes_only_the_accepted_identity_and_endpoint() -> Result<()> {
    // The guard owns the env barrier and the workshop gate — see the
    // headless_startup twin.
    let _workshop = codewhale_runtime::active_workshop::active_workshop_test_guard();
    let _offline = EnvVarGuard::set("CODEWHALE_DISABLE_CLOUD_FACTS", "1");
    let _live = codewhale_engine::provider_lake::lock_live_snapshot();
    let home = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
    let _reset = CatalogReset;
    cold_process();
    persist_catalog(ProviderKind::Custom, "vision-one", FIRST_ENDPOINT, true);
    persist_catalog(ProviderKind::Custom, "vision-two", FIRST_ENDPOINT, false);
    persist_catalog(ProviderKind::Custom, "vision-one", SECOND_ENDPOINT, false);
    cold_process();

    let first = config_for("vision-one", FIRST_ENDPOINT);
    let second = config_for("vision-two", FIRST_ENDPOINT);
    let manager = open_server_manager(&first, home.path())?;
    assert_eq!(image_capability(&first), CapabilityState::Supported);
    assert_eq!(image_capability(&second), CapabilityState::Unknown);

    manager.reload_config(second).await?;
    assert_eq!(
        image_capability(&manager.read_config()),
        CapabilityState::Unsupported
    );
    manager
        .reload_config(config_for("vision-one", "http://127.0.0.1:9/uncached/v1"))
        .await?;
    assert_eq!(
        image_capability(&manager.read_config()),
        CapabilityState::Unknown
    );
    manager
        .reload_config(config_for("vision-one", SECOND_ENDPOINT))
        .await?;
    assert_eq!(
        image_capability(&manager.read_config()),
        CapabilityState::Unsupported
    );
    manager.reload_config(first).await?;
    assert_eq!(
        image_capability(&manager.read_config()),
        CapabilityState::Supported
    );
    Ok(())
}
