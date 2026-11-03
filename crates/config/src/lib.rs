pub mod app_mode;
pub mod auth_source;
pub mod auto_model;
pub mod catalog;
pub mod cloud_facts;
mod config_document;
pub mod credentials;
pub mod descriptors;
pub mod device_code;
pub mod extension_host;
pub mod external_credentials;
pub mod hooks;
pub mod instruction_budget;
pub mod legacy_root;
pub mod local_reasoning;
pub mod model_reference;
pub mod models_dev;
pub mod notifications;
mod opencode_go;
pub mod persistence;
pub mod pricing;
pub mod private_directory;
pub mod provider;
mod provider_defaults;
mod provider_kind;
pub mod redaction;
pub mod resolve;
pub mod route;
pub mod settings_schema;
pub mod setup_state;
pub mod suggest;
pub mod user_constitution;
#[cfg(windows)]
pub mod windows_identity;
mod xai_credentials;
pub use app_mode::AppMode;
pub use auth_source::{AuthSourceKind, ProviderAuthSourceToml};
pub use codewhale_execpolicy::ToolAskRule;
pub use codewhale_paths::{CODEWHALE_APP_DIR, LEGACY_APP_DIR};
pub use codewhale_secrets::Secrets;
pub use config_document::{
    ConfigDocumentUndo, create_config_document, migrate_legacy_root_config, mutate_config_document,
    mutate_config_document_undoable, mutate_config_document_undoable_with_migration,
    mutate_config_document_with_migration, preview_legacy_root_config,
    replace_config_document_if_unchanged, set_config_document_value, unset_config_document_value,
    with_config_write_lock,
};
pub use external_credentials::{
    EXTERNAL_CREDENTIAL_CONSENT_VERSION, EXTERNAL_CREDENTIAL_READ_ONLY_SEMANTICS,
    ExternalCredentialAccess, ExternalCredentialConsentStatus, ExternalCredentialConsentToml,
    ExternalCredentialReadGrant, ExternalCredentialSource, default_dsh_credentials_path,
    external_credential_consent_status, quote_os_path, resolve_external_credential_path,
};
pub use model_reference::{Modality, ModelReferenceCard, ModelReferenceDatabase};
pub use opencode_go::{opencode_go_endpoint_key, opencode_go_model_id, opencode_go_models};
pub(crate) use provider_defaults::*;
pub use provider_kind::ProviderKind;
pub use route::ProviderId;
pub use settings_schema::{
    SETTINGS_SCHEMA, SettingDef, SettingKind, SettingOption, SettingUi, schema_groups, schema_rows,
    schema_tabs, setting, setting_index,
};
pub use setup_state::{
    ConstitutionAuthoring, ConstitutionChoice, ConstitutionSource, ConstitutionValidity,
    InheritedConfigFacts, RuntimePostureSource, SetupState, SetupStep, StepEntry, StepStatus,
    TELEMETRY_NOTICE_VERSION,
};
pub use user_constitution::{
    APPROX_BYTES_PER_TOKEN, AutonomyPreference, CacheProjection, ClauseOrigin, ClauseStatus,
    ConstitutionClause, ConstitutionRecommendation, MigrationOutcome, MigrationReceipt,
    MigrationRejection, Ratification, RatificationError, RecommendationParse,
    USER_CONSTITUTION_SCHEMA_VERSION, USER_CONSTITUTION_SCHEMA_VERSION_V1, UntrustedDraftParse,
    UserConstitution, UserConstitutionLoad,
};
pub use xai_credentials::{
    CHATGPT_HOST_FILE_NAME, CHATGPT_OAUTH_GENERATION_PREFIX, CHATGPT_OAUTH_GENERATION_SUFFIX,
    CLAUDE_OAUTH_GENERATION_PREFIX, CLAUDE_OAUTH_GENERATION_SUFFIX, LEGACY_CHATGPT_OAUTH_FILE_NAME,
    LEGACY_CLAUDE_OAUTH_FILE_NAME, LEGACY_XAI_OAUTH_FILE_NAME, XAI_OAUTH_GENERATION_PREFIX,
    XAI_OAUTH_GENERATION_SUFFIX, XaiOAuthCredentialStore, XaiOAuthRevocation,
    chatgpt_oauth_generation_path, claude_oauth_generation_path,
    clear_all_chatgpt_oauth_credentials, clear_all_chatgpt_oauth_credentials_locked,
    clear_all_claude_oauth_credentials_locked, clear_all_xai_oauth_credentials,
    is_valid_chatgpt_oauth_generation, is_valid_claude_oauth_generation,
    is_valid_xai_oauth_generation, legacy_chatgpt_oauth_path, legacy_xai_oauth_path,
    remove_chatgpt_oauth_generation, remove_xai_oauth_generation,
    validate_chatgpt_oauth_generation, validate_claude_oauth_generation,
    validate_xai_oauth_generation, with_xai_oauth_lifecycle_lock,
    with_xai_oauth_revocation_transaction, xai_oauth_credentials_dir, xai_oauth_generation_path,
};
mod base_url_resolution;
mod config_backup;
mod config_migration;
mod config_resolve;
mod config_store;
mod config_toml;
mod display_redaction;
mod env_overrides;
mod extension_toml;
mod fleet;
mod hotbar;
mod model_normalization;
mod path_io;
mod permissions;
mod project_config;
mod provider_chain;
mod provider_config_keys;
mod provider_toml;
mod runtime_options;
mod service_toml;
mod state_dirs;
mod stream_timeouts;
mod telemetry;

pub use base_url_resolution::*;
pub use config_backup::*;
pub use config_migration::*;
pub use config_store::*;
pub use config_toml::*;
pub use display_redaction::*;
pub use extension_toml::*;
pub use fleet::*;
pub use hotbar::*;
pub use model_normalization::*;
pub(crate) use path_io::*;
pub use permissions::*;
pub use project_config::*;
pub use provider_chain::*;
pub use provider_toml::*;
pub use runtime_options::*;
pub use service_toml::*;
pub use state_dirs::*;
pub use stream_timeouts::*;
pub use telemetry::*;

#[cfg(test)]
mod tests;
