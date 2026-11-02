use crate::base_url_resolution::{
    auth_mode_disables_api_key, auth_mode_uses_kimi_imported_token, env_api_key_for_provider,
    is_modelstudio_family, moonshot_base_url_uses_kimi_code,
    provider_preserves_custom_base_url_model, resolve_deepseek_base_url, resolve_minimax_base_url,
    resolve_modelstudio_base_url, resolve_xiaomi_mimo_base_url,
    should_skip_secret_store_for_provider, stored_api_key_for_provider,
    xiaomi_mimo_env_api_key_for_runtime,
};
use crate::config_toml::ConfigToml;
use crate::display_redaction::{
    config_value_at_path, is_sensitive_config_key, parse_bool, parse_http_headers, redact_secret,
    redact_toml_value_for_display, schema_toml_value, serialize_http_headers,
    serialize_http_headers_for_display, toml_value_as_u64,
};
use crate::env_overrides::EnvRuntimeOverrides;
use crate::extension_toml::HookSinksToml;
use crate::model_normalization::{
    default_model_for_provider, normalize_model_for_provider,
    root_default_model_is_foreign_to_provider,
};
use crate::project_config::check_config_toml_choice;
use crate::provider_config_keys::CUSTOM_PROVIDER_FIELD_HINT;
use crate::provider_config_keys::{
    ProviderConfigField, builtin_provider_kind_for_config_id, get_provider_config_display_value,
    get_provider_config_value, insert_provider_config_values, is_builtin_provider_config_id,
    parse_context_window, parse_custom_provider_config_key, parse_model_context_window_key,
    parse_provider_config_key, set_provider_config_value, unset_provider_config_value,
};
use crate::provider_defaults::*;
use crate::provider_kind::ProviderKind;
use crate::provider_toml::{
    ConfigApiKeyValueKind, LEGACY_ANTIGRAVITY_TOMBSTONE_MESSAGE, ProviderConfigToml,
    classify_config_api_key_value, is_upstream_auth_header,
};
use crate::runtime_options::{
    CliRuntimeOverrides, ModelSource, ProviderSource, ResolvedRuntimeOptions, RuntimeApiKeySource,
};
use crate::settings_schema::setting;
use crate::stream_timeouts::{
    DEFAULT_STREAM_CHUNK_TIMEOUT_SECS, MAX_STREAM_CHUNK_TIMEOUT_SECS, MIN_STREAM_CHUNK_TIMEOUT_SECS,
};
use crate::telemetry::{
    DEFAULT_TELEMETRY_ENDPOINT, TelemetrySource, resolved_telemetry_consent,
    telemetry_consent_from_env,
};
use crate::{cloud_facts, notifications, provider};
use anyhow::Context as _;
use anyhow::{Result, bail};
use codewhale_secrets::{SecretSource, Secrets};
use std::collections::BTreeMap;
use std::path::PathBuf;

impl ConfigToml {
    /// Exact configured provider id, including a dynamically named custom
    /// provider selected by the TUI.
    #[must_use]
    pub fn provider_id(&self) -> &str {
        self.selected_provider_id
            .as_deref()
            .filter(|id| {
                if self.provider == ProviderKind::Custom {
                    self.providers.extras.contains_key(*id)
                } else {
                    ProviderKind::parse_config_identity(id) == Some(self.provider)
                        && self.providers.extras.get(*id).is_none_or(|value| {
                            value
                                .as_table()
                                .is_some_and(|table| !table.contains_key("kind"))
                        })
                }
            })
            .unwrap_or_else(|| self.provider.as_str())
    }

    /// The real key behind a legacy top-level `api_key` / `base_url`.
    ///
    /// Those keys no longer exist at the top level (#6394); `config get|set|
    /// unset base_url` addresses the active provider's own table instead, so
    /// the value lands where the route reads it.
    #[must_use]
    pub fn root_alias_key(&self, key: &str) -> Option<String> {
        let field = match key {
            "api_key" | "apiKey" => "api_key",
            "base_url" | "baseUrl" => "base_url",
            _ => return None,
        };
        let table = self
            .named_custom_provider_id()
            .unwrap_or_else(|| self.provider.provider().provider_config_key());
        Some(format!("providers.{table}.{field}"))
    }

    /// Return the exact id only when the root selection names a dynamic custom
    /// provider rather than the legacy literal `custom` route.
    #[must_use]
    pub fn named_custom_provider_id(&self) -> Option<&str> {
        (self.provider == ProviderKind::Custom)
            .then_some(self.selected_provider_id.as_deref())
            .flatten()
            .filter(|id| self.providers.extras.contains_key(*id))
    }

    fn named_custom_provider_table(&self, provider_id: &str) -> Result<&toml::value::Table> {
        let table = self
            .providers
            .extras
            .get(provider_id)
            .and_then(toml::Value::as_table)
            .with_context(|| {
                format!(
                    "custom provider '{provider_id}' requires a matching [providers.{provider_id}] table"
                )
            })?;
        let compatible = table
            .get("kind")
            .and_then(toml::Value::as_str)
            .is_some_and(|kind| {
                kind.trim()
                    .to_ascii_lowercase()
                    .replace('_', "-")
                    .eq("openai-compatible")
            });
        if !compatible {
            bail!(
                "custom provider '{provider_id}' must set [providers.{provider_id}].kind = \"openai-compatible\""
            );
        }
        Ok(table)
    }

    /// The typed `[providers.<id>]` table of a named custom provider. A table
    /// that fails validation is an error, never a reason to read another one.
    fn named_custom_provider_config_for(&self, provider_id: &str) -> Result<ProviderConfigToml> {
        let table = self.named_custom_provider_table(provider_id)?;
        // The deserializer error can quote the offending value, which may be a
        // credential, so report only which table is wrong.
        toml::Value::Table(table.clone()).try_into().map_err(|_| {
            anyhow::anyhow!(
                "custom provider '{provider_id}' has an invalid [providers.{provider_id}] table: a field has the wrong type"
            )
        })
    }

    /// Mutable access to a custom provider's `[providers.<id>]` table,
    /// creating it on the first `config set providers.<id>.<field>`.
    fn custom_provider_table_mut(&mut self, provider_id: &str) -> Result<&mut toml::value::Table> {
        let entry = self
            .providers
            .extras
            .entry(provider_id.to_string())
            .or_insert_with(|| toml::Value::Table(toml::value::Table::new()));
        entry.as_table_mut().with_context(|| {
            format!("custom provider '{provider_id}' must be a [providers.{provider_id}] table")
        })
    }

    /// Write one leg of a custom provider table. Named custom providers are
    /// not in [`ProviderKind::ALL`], so without this path
    /// `config set providers.<custom>.<field>` fell through to a literal
    /// top-level extras key and silently never took effect (#5167).
    fn set_custom_provider_value(
        &mut self,
        provider_id: &str,
        field_key: &str,
        value: &str,
    ) -> Result<()> {
        if is_builtin_provider_config_id(provider_id) {
            bail!(
                "unknown field '{field_key}' for built-in provider '{provider_id}': \
                 expected one of api_key, base_url, model, context_window, mode, auth_mode, \
                 insecure_skip_tls_verify, http_headers, path_suffix"
            );
        }
        if field_key == "kind" {
            let compatible =
                value.trim().to_ascii_lowercase().replace('_', "-") == "openai-compatible";
            if !compatible {
                bail!(
                    "custom provider '{provider_id}' must set [providers.{provider_id}].kind = \"openai-compatible\""
                );
            }
            self.custom_provider_table_mut(provider_id)?.insert(
                "kind".to_string(),
                toml::Value::String(value.trim().to_string()),
            );
            return Ok(());
        }
        let Some(field) = ProviderConfigField::parse(field_key) else {
            bail!(
                "unknown field '{field_key}' for custom provider '{provider_id}': \
                 expected one of {CUSTOM_PROVIDER_FIELD_HINT}"
            );
        };
        let toml_value = match field {
            ProviderConfigField::Vendor => {
                bail!("vendor is only supported by providers.openrouter")
            }
            ProviderConfigField::ApiKey
            | ProviderConfigField::BaseUrl
            | ProviderConfigField::Model
            | ProviderConfigField::Mode
            | ProviderConfigField::Wire
            | ProviderConfigField::AuthMode
            | ProviderConfigField::PathSuffix => toml::Value::String(value.to_string()),
            ProviderConfigField::ContextWindow => {
                toml::Value::Integer(i64::from(parse_context_window(value)?))
            }
            ProviderConfigField::InsecureSkipTlsVerify | ProviderConfigField::AllowInsecureHttp => {
                toml::Value::Boolean(parse_bool(value)?)
            }
            ProviderConfigField::HttpHeaders => toml::Value::Table(
                parse_http_headers(value)?
                    .into_iter()
                    .map(|(name, header)| (name, toml::Value::String(header)))
                    .collect(),
            ),
        };
        self.custom_provider_table_mut(provider_id)?
            .insert(field.key().to_string(), toml_value);
        Ok(())
    }

    fn get_custom_provider_value_with(
        &self,
        provider_id: &str,
        field_key: &str,
        render: fn(&ProviderConfigToml, ProviderConfigField) -> Option<String>,
    ) -> Option<String> {
        let table = self.providers.extras.get(provider_id)?.as_table()?;
        if field_key == "kind" {
            return table.get("kind")?.as_str().map(str::to_string);
        }
        let field = ProviderConfigField::parse(field_key)?;
        let config: ProviderConfigToml = toml::Value::Table(table.clone()).try_into().ok()?;
        render(&config, field)
    }

    fn unset_custom_provider_value(&mut self, provider_id: &str, field_key: &str) {
        let Some(table) = self
            .providers
            .extras
            .get_mut(provider_id)
            .and_then(toml::Value::as_table_mut)
        else {
            return;
        };
        let leg = if field_key == "kind" {
            "kind"
        } else {
            ProviderConfigField::parse(field_key).map_or(field_key, |field| field.key())
        };
        table.remove(leg);
    }

    /// Write one `[providers.<id>.model_context_windows]` entry (#6108),
    /// whether `<id>` is a built-in provider key or a named custom table.
    fn set_model_context_window(
        &mut self,
        provider_id: &str,
        model: &str,
        value: &str,
    ) -> Result<()> {
        if model.eq_ignore_ascii_case("auto")
            || model.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            bail!("invalid model id for `model_context_windows`");
        }
        let window = parse_context_window(value)?;
        if let Some(kind) = builtin_provider_kind_for_config_id(provider_id) {
            self.providers
                .for_provider_mut(kind)
                .model_context_windows
                .insert(model.to_string(), window);
            return Ok(());
        }
        let table = self.custom_provider_table_mut(provider_id)?;
        let windows = table
            .entry("model_context_windows".to_string())
            .or_insert_with(|| toml::Value::Table(toml::value::Table::new()));
        windows
            .as_table_mut()
            .with_context(|| {
                format!("providers.{provider_id}.model_context_windows must be a table")
            })?
            .insert(model.to_string(), toml::Value::Integer(i64::from(window)));
        Ok(())
    }

    fn model_context_window_value(&self, provider_id: &str, model: &str) -> Option<String> {
        if let Some(kind) = builtin_provider_kind_for_config_id(provider_id) {
            return self
                .providers
                .for_provider(kind)
                .model_context_windows
                .get(model)
                .map(u32::to_string);
        }
        self.providers
            .extras
            .get(provider_id)?
            .as_table()?
            .get("model_context_windows")?
            .as_table()?
            .get(model)?
            .as_integer()
            .map(|value| value.to_string())
    }

    fn unset_model_context_window(&mut self, provider_id: &str, model: &str) {
        if let Some(kind) = builtin_provider_kind_for_config_id(provider_id) {
            self.providers
                .for_provider_mut(kind)
                .model_context_windows
                .remove(model);
            return;
        }
        if let Some(table) = self
            .providers
            .extras
            .get_mut(provider_id)
            .and_then(toml::Value::as_table_mut)
            && let Some(windows) = table
                .get_mut("model_context_windows")
                .and_then(toml::Value::as_table_mut)
        {
            windows.remove(model);
            if windows.is_empty() {
                table.remove("model_context_windows");
            }
        }
    }

    /// Bind the raw selector after deserializing a document. Exact custom
    /// tables take precedence over built-in aliases, and regional spellings
    /// survive later typed saves. This does not apply environment overrides.
    pub fn bind_persisted_provider_id(&mut self, provider_id: &str) -> Result<()> {
        let provider_id = provider_id.trim();
        // Earlier typed saves wrote the serde kebab-case spelling
        // `siliconflow-c-n` instead of the canonical `siliconflow-CN`. Read it
        // back as the built-in so those files load and the next save repairs
        // them, unless the user really declared a table by that name.
        let provider_id = if provider_id.eq_ignore_ascii_case("siliconflow-c-n")
            && !self.providers.extras.contains_key(provider_id)
        {
            ProviderKind::SiliconflowCN.as_str()
        } else {
            provider_id
        };
        let parsed = ProviderKind::parse_config_identity(provider_id);
        // Kindless tables mirroring a built-in alias remain inert. An explicit
        // custom declaration must validate; never fall back to a different
        // credential/endpoint authority when its kind is invalid.
        let custom_table = self.providers.extras.get(provider_id);
        let kindless_alias = parsed.is_some()
            && custom_table
                .and_then(toml::Value::as_table)
                .is_some_and(|table| !table.contains_key("kind"));
        let provider = if parsed != Some(ProviderKind::Antigravity)
            && custom_table.is_some()
            && !kindless_alias
        {
            self.named_custom_provider_config_for(provider_id)?;
            ProviderKind::Custom
        } else if let Some(provider) = parsed {
            provider
        } else {
            self.named_custom_provider_config_for(provider_id)?;
            ProviderKind::Custom
        };
        self.provider = provider;
        self.selected_provider_id = (provider_id != provider.as_str()
            && (provider != ProviderKind::Custom
                || self.providers.extras.contains_key(provider_id)))
        .then(|| provider_id.to_string());
        Ok(())
    }

    #[must_use]
    pub fn get_value(&self, key: &str) -> Option<String> {
        if let Some(alias) = self.root_alias_key(key) {
            return self.get_value(&alias);
        }
        if notifications::in_namespace(key) {
            let setting = notifications::NotificationSetting::parse(key)?;
            return Some(
                notifications::from_extras(&self.extras)
                    .ok()?
                    .display(setting),
            );
        }
        if let Some((provider_id, model)) = parse_model_context_window_key(key) {
            return self.model_context_window_value(provider_id, model);
        }
        if let Some((provider, field)) = parse_provider_config_key(key) {
            return get_provider_config_value(self.providers.for_provider(provider), field);
        }
        if let Some((provider_id, field_key)) = parse_custom_provider_config_key(key) {
            return self.get_custom_provider_value_with(
                provider_id,
                field_key,
                get_provider_config_value,
            );
        }

        match key {
            "provider" => Some(self.provider_id().to_string()),
            "stream_chunk_timeout_secs" | "tui.stream_chunk_timeout_secs" => {
                Some(self.stream_chunk_timeout_secs().to_string())
            }
            "http_headers" => serialize_http_headers(&self.http_headers),
            "default_text_model" => self.default_text_model.clone(),
            "model" => self.model.clone(),
            "auth.mode" => self.auth_mode.clone(),
            "verbosity" => self.verbosity.clone(),
            "log_level" => self.log_level.clone(),
            "telemetry" => self.telemetry.map(|v| v.to_string()),
            "telemetry_endpoint" => self.telemetry_endpoint.clone(),
            "approval_policy" => self.approval_policy.clone(),
            "sandbox_mode" => self.sandbox_mode.clone(),
            "tools.always_load" => self.tools.as_ref().map(|tools| tools.always_load.join(",")),
            "hook_sinks.unix_socket_path" => self
                .hook_sinks
                .as_ref()
                .and_then(|sinks| sinks.unix_socket_path.as_ref())
                .map(|path| path.display().to_string()),
            _ => self
                .extras
                .get(key)
                .map(toml::Value::to_string)
                .or_else(|| {
                    let document = toml::Value::try_from(self).ok()?;
                    config_value_at_path(&document, key).map(toml::Value::to_string)
                }),
        }
    }

    /// The unquoted contents of an extras key that holds a TOML string.
    ///
    /// [`ConfigToml::get_value`] renders extras through `toml::Value::to_string`,
    /// which re-applies TOML quoting — and switches to a single-quoted literal
    /// string whenever the payload contains a `"`. A JSON blob written with
    /// [`ConfigToml::set_value`] therefore comes back as `'[{"a":1}]'` and no
    /// longer parses as JSON (#4727). Callers that stored structured text want
    /// the payload, not its TOML rendering.
    #[must_use]
    pub fn get_raw_string(&self, key: &str) -> Option<&str> {
        self.extras.get(key).and_then(toml::Value::as_str)
    }

    #[must_use]
    pub fn get_display_value(&self, key: &str) -> Option<String> {
        if let Some(alias) = self.root_alias_key(key) {
            return self.get_display_value(&alias);
        }
        if notifications::in_namespace(key) {
            return self.get_value(key);
        }
        if let Some((provider_id, model)) = parse_model_context_window_key(key) {
            return self.model_context_window_value(provider_id, model);
        }
        if let Some((provider, field)) = parse_provider_config_key(key) {
            return get_provider_config_display_value(self.providers.for_provider(provider), field);
        }
        if let Some((provider_id, field_key)) = parse_custom_provider_config_key(key) {
            return self.get_custom_provider_value_with(
                provider_id,
                field_key,
                get_provider_config_display_value,
            );
        }

        if key == "telemetry" {
            // Report the resolved configuration preference even when the key is
            // absent. The telemetry owner also preserves recorded opt-outs
            // and fails closed when existing privacy state is unreadable.
            let (on, source) = resolved_telemetry_consent(self.telemetry);
            return Some(format!(
                "{} ({})",
                if on { "on" } else { "off" },
                source.as_str()
            ));
        }

        if key == "http_headers" {
            return serialize_http_headers_for_display(&self.http_headers);
        }

        if let Some(value) = self.extras.get(key) {
            return Some(redact_toml_value_for_display(key, value));
        }

        // Table and nested lookups use the same recursively redacted tree as
        // `config dump`; a parent such as `credentials` must keep its children
        // secret even when the requested leaf itself has an innocuous name.
        let document = self.redacted_toml_value();
        if let Some(value) = config_value_at_path(&document, key)
            && (value.is_table() || value.is_array() || key.contains('.'))
            && !matches!(key, "tui.stream_chunk_timeout_secs")
        {
            return Some(
                value
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| value.to_string()),
            );
        }

        self.get_value(key).map(|value| {
            if is_sensitive_config_key(key) {
                redact_secret(&value)
            } else {
                value
            }
        })
    }

    #[must_use]
    pub fn stream_chunk_timeout_secs(&self) -> u64 {
        let raw = self
            .extras
            .get("tui")
            .and_then(toml::Value::as_table)
            .and_then(|table| table.get("stream_chunk_timeout_secs"))
            .and_then(toml_value_as_u64)
            .or_else(|| {
                self.extras
                    .get("tui.stream_chunk_timeout_secs")
                    .and_then(toml_value_as_u64)
            })
            .or_else(|| {
                self.extras
                    .get("stream_chunk_timeout_secs")
                    .and_then(toml_value_as_u64)
            })
            .unwrap_or(DEFAULT_STREAM_CHUNK_TIMEOUT_SECS);
        if raw == 0 {
            DEFAULT_STREAM_CHUNK_TIMEOUT_SECS
        } else {
            raw.clamp(MIN_STREAM_CHUNK_TIMEOUT_SECS, MAX_STREAM_CHUNK_TIMEOUT_SECS)
        }
    }

    pub fn set_value(&mut self, key: &str, value: &str) -> Result<()> {
        if let Some(alias) = self.root_alias_key(key) {
            return self.set_value(&alias, value);
        }
        check_config_toml_choice(key, value)?;
        if let Some(field) = key.strip_prefix("stream.") {
            let def = setting(key).with_context(|| format!("unknown stream setting `{key}`"))?;
            let stored = schema_toml_value(key, def, value)?;
            if let Some(number) = stored.as_integer() {
                // Retry counts are u32 in the runtime reader; other values
                // are u64. Reject invalid types before touching the document.
                let max = if matches!(
                    field,
                    "max_resumes" | "max_transparent_retries" | "max_stream_errors"
                ) {
                    i64::from(u32::MAX)
                } else {
                    i64::MAX
                };
                anyhow::ensure!(
                    (0..=max).contains(&number),
                    "invalid unsigned value for `{key}`"
                );
            }
            self.extras
                .entry("stream".to_string())
                .or_insert_with(|| toml::Value::Table(toml::Table::new()))
                .as_table_mut()
                .context("stream must be a TOML table")?
                .insert(field.to_string(), stored);
            return Ok(());
        }
        if key == "update.mode" {
            let mode = value.trim().to_ascii_lowercase();
            anyhow::ensure!(
                matches!(mode.as_str(), "auto" | "notify" | "off"),
                "invalid value for `update.mode`: expected auto, notify or off. No value was changed."
            );
            self.extras
                .entry("update".to_string())
                .or_insert_with(|| toml::Value::Table(toml::Table::new()))
                .as_table_mut()
                .ok_or_else(|| anyhow::anyhow!("update must be a TOML table"))?
                .insert("mode".to_string(), toml::Value::String(mode));
            return Ok(());
        }
        if notifications::in_namespace(key) {
            let setting = notifications::NotificationSetting::required(key)?;
            let update = notifications::NotificationConfigUpdate::parse(setting, value)?;
            return notifications::edit_extras(&mut self.extras, setting, update.value()?);
        }
        if parse_custom_provider_config_key(key).is_some_and(|(provider_id, _)| {
            ProviderKind::parse_config_identity(provider_id) == Some(ProviderKind::Antigravity)
        }) {
            bail!(LEGACY_ANTIGRAVITY_TOMBSTONE_MESSAGE);
        }
        if let Some((provider_id, model)) = parse_model_context_window_key(key) {
            return self.set_model_context_window(provider_id, model, value);
        }
        if let Some((provider, field)) = parse_provider_config_key(key) {
            return set_provider_config_value(self, provider, field, value);
        }
        if let Some((provider_id, field_key)) = parse_custom_provider_config_key(key) {
            return self.set_custom_provider_value(provider_id, field_key, value);
        }

        match key {
            "provider" => {
                if ProviderKind::parse_config_identity(value) == Some(ProviderKind::Antigravity) {
                    bail!(LEGACY_ANTIGRAVITY_TOMBSTONE_MESSAGE);
                }
                self.bind_persisted_provider_id(value).with_context(|| {
                    format!(
                        "unknown provider '{value}': expected {} or a configured custom provider",
                        ProviderKind::names_hint()
                    )
                })?;
            }
            "http_headers" => self.http_headers = parse_http_headers(value)?,
            "default_text_model" => self.default_text_model = Some(value.to_string()),
            "model" => self.model = Some(value.to_string()),
            "auth.mode" => self.auth_mode = Some(value.to_string()),
            "verbosity" => self.verbosity = Some(value.to_string()),
            "log_level" => self.log_level = Some(value.to_string()),
            "telemetry" => {
                self.telemetry = Some(parse_bool(value)?);
            }
            // Scheme rules (HTTPS, or loopback HTTP) are enforced where a
            // batch would actually be sent, not here: a user must be able to
            // stage a value before the machinery that reads it exists.
            "telemetry_endpoint" => self.telemetry_endpoint = Some(value.to_string()),
            "approval_policy" => self.approval_policy = Some(value.to_string()),
            "sandbox_mode" => self.sandbox_mode = Some(value.to_string()),
            // The TUI reader (`ReasoningEffort::parse_strict`) owns this
            // vocabulary and accepts aliases the schema's option list does
            // not name (`none`, `mid`, `maximum`, ...), so the schema check
            // below would refuse values that take effect. `codewhale config
            // set` validates against that reader before calling here.
            "reasoning_effort" => {
                self.extras
                    .insert(key.to_string(), toml::Value::String(value.to_string()));
            }
            "hook_sinks.unix_socket_path" => {
                self.hook_sinks
                    .get_or_insert_with(HookSinksToml::default)
                    .unix_socket_path = Some(PathBuf::from(value));
            }
            // The MCP stdio dispatcher persists this established literal key
            // as JSON text; it is not a nested TOML setting.
            _ if key.contains('.') && key != "mcp.server_definitions" => {
                let (table, field) = key.rsplit_once('.').expect("dotted key");
                bail!(
                    "`config set` does not support nested key `{key}`; edit `{field}` in the [{table}] table of config.toml instead (use a TOML value of the documented type). No value was changed."
                );
            }
            _ => {
                // A declared setting keeps its declared type (#6563): refuse a
                // value the schema cannot hold, and store booleans and numbers
                // as TOML values so typed readers deserialize them. Whether an
                // undeclared key is read by anything is the caller's question;
                // `codewhale config set` refuses those before reaching here.
                let stored = match setting(key) {
                    Some(def) => schema_toml_value(key, def, value)?,
                    None => toml::Value::String(value.to_string()),
                };
                self.extras.insert(key.to_string(), stored);
            }
        }
        Ok(())
    }

    pub fn unset_value(&mut self, key: &str) -> Result<()> {
        if let Some(alias) = self.root_alias_key(key) {
            return self.unset_value(&alias);
        }
        if let Some(field) = key.strip_prefix("stream.") {
            anyhow::ensure!(setting(key).is_some(), "unknown stream setting `{key}`");
            if let Some(stream) = self.extras.get_mut("stream") {
                stream
                    .as_table_mut()
                    .context("stream must be a TOML table")?
                    .remove(field);
            }
            return Ok(());
        }
        if notifications::in_namespace(key) {
            let setting = notifications::NotificationSetting::required(key)?;
            return notifications::edit_extras(&mut self.extras, setting, None);
        }
        if let Some((provider_id, model)) = parse_model_context_window_key(key) {
            self.unset_model_context_window(provider_id, model);
            return Ok(());
        }
        if let Some((provider, field)) = parse_provider_config_key(key) {
            unset_provider_config_value(self, provider, field);
            return Ok(());
        }
        if let Some((provider_id, field_key)) = parse_custom_provider_config_key(key) {
            self.unset_custom_provider_value(provider_id, field_key);
            return Ok(());
        }

        match key {
            "provider" => {
                self.provider = ProviderKind::Deepseek;
                self.selected_provider_id = None;
            }
            "http_headers" => self.http_headers.clear(),
            "default_text_model" => self.default_text_model = None,
            "model" => self.model = None,
            "auth.mode" => self.auth_mode = None,
            "verbosity" => self.verbosity = None,
            "log_level" => self.log_level = None,
            "telemetry" => self.telemetry = None,
            "telemetry_endpoint" => self.telemetry_endpoint = None,
            "approval_policy" => self.approval_policy = None,
            "sandbox_mode" => self.sandbox_mode = None,
            "hook_sinks.unix_socket_path" => {
                if let Some(sinks) = self.hook_sinks.as_mut() {
                    sinks.unix_socket_path = None;
                }
            }
            _ => {
                self.extras.remove(key);
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn list_values(&self) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        out.insert("provider".to_string(), self.provider_id().to_string());

        if let Some(v) = serialize_http_headers_for_display(&self.http_headers) {
            out.insert("http_headers".to_string(), v);
        }
        if let Some(v) = self.default_text_model.as_ref() {
            out.insert("default_text_model".to_string(), v.clone());
        }
        if let Some(v) = self.model.as_ref() {
            out.insert("model".to_string(), v.clone());
        }
        if let Some(v) = self.auth_mode.as_ref() {
            out.insert("auth.mode".to_string(), v.clone());
        }
        if let Some(v) = self.verbosity.as_ref() {
            out.insert("verbosity".to_string(), v.clone());
        }
        if let Some(v) = self.log_level.as_ref() {
            out.insert("log_level".to_string(), v.clone());
        }
        if let Some(v) = self.telemetry {
            out.insert("telemetry".to_string(), v.to_string());
        }
        if let Some(v) = self.telemetry_endpoint.as_ref() {
            out.insert("telemetry_endpoint".to_string(), v.clone());
        }
        if let Some(v) = self.approval_policy.as_ref() {
            out.insert("approval_policy".to_string(), v.clone());
        }
        if let Some(v) = self.sandbox_mode.as_ref() {
            out.insert("sandbox_mode".to_string(), v.clone());
        }
        if let Some(v) = self
            .hook_sinks
            .as_ref()
            .and_then(|sinks| sinks.unix_socket_path.as_ref())
        {
            out.insert(
                "hook_sinks.unix_socket_path".to_string(),
                v.display().to_string(),
            );
        }

        for provider in provider::all_providers().iter().map(|p| p.kind()) {
            insert_provider_config_values(
                &mut out,
                provider,
                self.providers.for_provider(provider),
            );
        }

        for (k, v) in &self.extras {
            if k == "notifications" {
                if let Ok(config) = notifications::from_extras(&self.extras) {
                    for setting in notifications::NotificationSetting::ALL {
                        out.insert(
                            format!("notifications.{}", setting.key()),
                            config.display(setting),
                        );
                    }
                } else {
                    out.insert(k.clone(), "<invalid notification configuration>".into());
                }
            } else if notifications::in_namespace(k)
                && notifications::NotificationSetting::parse(k).is_some()
            {
                if let Ok(config) = notifications::from_extras(&self.extras) {
                    let setting = notifications::NotificationSetting::parse(k)
                        .expect("known notification key");
                    out.insert(
                        format!("notifications.{}", setting.key()),
                        config.display(setting),
                    );
                }
            } else {
                out.insert(k.clone(), redact_toml_value_for_display(k, v));
            }
        }
        out
    }

    /// Resolve runtime options without touching platform credential stores.
    ///
    /// This method keeps library callers prompt-free: CLI flag → config file
    /// → environment. Call `resolve_runtime_options_with_secrets` when a
    /// user-facing dispatcher should recover credentials from the configured
    /// secret store.
    #[must_use]
    pub fn resolve_runtime_options(&self, cli: &CliRuntimeOverrides) -> ResolvedRuntimeOptions {
        let no_keyring = Secrets::new(std::sync::Arc::new(
            codewhale_secrets::InMemoryKeyringStore::new(),
        ));
        self.resolve_runtime_options_with_secrets(cli, &no_keyring)
    }

    /// Resolve runtime options using an explicit secrets façade.
    ///
    /// API-key precedence is **CLI flag → config-file → secret store → environment**.
    #[must_use]
    pub fn resolve_runtime_options_with_secrets(
        &self,
        cli: &CliRuntimeOverrides,
        secrets: &Secrets,
    ) -> ResolvedRuntimeOptions {
        let env = EnvRuntimeOverrides::load();
        let (provider, provider_source) = if let Some(provider) = cli.provider {
            (provider, ProviderSource::Cli)
        } else if let Some(provider) = env.provider {
            (
                provider,
                ProviderSource::Env(env.provider_source.unwrap_or("CODEWHALE_PROVIDER")),
            )
        } else {
            (self.provider, ProviderSource::Config)
        };

        let named_custom_provider = (provider == ProviderKind::Custom
            && matches!(provider_source, ProviderSource::Config))
        .then(|| self.named_custom_provider_id())
        .flatten();
        let mut provider_cfg = if let Some(provider_id) = named_custom_provider {
            // A named route whose table became invalid after binding resolves
            // to the fail-closed loopback placeholder, never to the legacy
            // `[providers.custom]` endpoint, model and key.
            self.named_custom_provider_config_for(provider_id)
                .unwrap_or_else(|err| {
                    tracing::warn!("{err:#}");
                    ProviderConfigToml::default()
                })
        } else {
            self.providers.for_provider(provider).clone()
        };
        if provider == ProviderKind::SiliconflowCN {
            let fb = &self.providers.siliconflow;
            if provider_cfg.api_key.is_none() {
                provider_cfg.api_key = fb.api_key.clone();
            }
            if provider_cfg.base_url.is_none() {
                provider_cfg.base_url = fb.base_url.clone();
            }
            if provider_cfg.model.is_none() {
                provider_cfg.model = fb.model.clone();
            }
        }
        let auth_mode = cli
            .auth_mode
            .clone()
            .or_else(|| env.auth_mode.clone())
            .or_else(|| provider_cfg.auth_mode.clone())
            .or_else(|| self.auth_mode.clone());
        // The legacy top-level key and endpoint were moved into their owning
        // `[providers.<name>]` table when the file was parsed (#6394).
        let from_file = provider_cfg.api_key.clone();
        let cli_base_url = cli.base_url.clone();
        let env_base_url = env.base_url_for(provider);
        let file_base_url = provider_cfg.base_url.clone();
        let base_url_from_file =
            cli_base_url.is_none() && env_base_url.is_none() && file_base_url.is_some();
        let configured_base_url = cli_base_url.or(env_base_url).or(file_base_url);
        let xiaomi_mimo_mode = if provider == ProviderKind::XiaomiMimo {
            env.xiaomi_mimo_mode
                .clone()
                .or_else(|| provider_cfg.mode.clone())
        } else {
            None
        };
        let xiaomi_mimo_env_api_key = if provider == ProviderKind::XiaomiMimo {
            xiaomi_mimo_env_api_key_for_runtime(
                xiaomi_mimo_mode.as_deref(),
                configured_base_url.as_deref(),
            )
        } else {
            None
        };
        let explicit_api_key_for_endpoint = cli
            .api_key
            .as_deref()
            .or(from_file.as_deref().filter(|value| {
                classify_config_api_key_value(value) == ConfigApiKeyValueKind::Literal
            }))
            .or(xiaomi_mimo_env_api_key.as_deref());
        let provider_wire = provider_cfg.wire.as_deref();
        let base_url = if provider == ProviderKind::XiaomiMimo {
            resolve_xiaomi_mimo_base_url(
                configured_base_url,
                explicit_api_key_for_endpoint,
                xiaomi_mimo_mode.as_deref(),
            )
        } else if is_modelstudio_family(provider) {
            resolve_modelstudio_base_url(
                configured_base_url,
                provider,
                provider_cfg.mode.as_deref(),
                provider_wire,
            )
        } else if matches!(
            provider,
            ProviderKind::Minimax | ProviderKind::MinimaxAnthropic
        ) {
            resolve_minimax_base_url(configured_base_url, provider, provider_wire)
        } else if matches!(
            provider,
            ProviderKind::Deepseek | ProviderKind::DeepseekAnthropic
        ) {
            resolve_deepseek_base_url(configured_base_url, provider, provider_wire)
        } else {
            configured_base_url
                .unwrap_or_else(|| descriptor_fallback_base_url(provider, auth_mode.as_deref()))
        };
        // Released builds represented Ollama Cloud as the local `ollama`
        // identity plus one exact hosted base URL. Upgrade only that tuple in
        // memory: the parsed config and secret store are never rewritten, and
        // neighboring/custom routes retain the local/custom identity.
        let legacy_ollama_cloud = provider::migrates_legacy_ollama_cloud_route(provider, &base_url);
        let provider = if legacy_ollama_cloud {
            ProviderKind::OllamaCloud
        } else {
            provider
        };
        // `auth_mode = "none"` is an endpoint contract, so it suppresses every
        // credential source (including explicit CLI/config values). Otherwise
        // CLI and route-local config win outright. Ambient provider credentials
        // are allowed only on the provider's official endpoint family: a saved
        // OpenRouter key must never follow `provider = "openrouter"` to an
        // unrelated custom gateway merely because the provider id stayed the
        // same.
        let uses_kimi_imported_token = provider == ProviderKind::Moonshot
            && auth_mode
                .as_deref()
                .is_some_and(auth_mode_uses_kimi_imported_token);
        let auth_disabled = auth_mode_disables_api_key(auth_mode.as_deref());
        let custom_endpoint = provider_preserves_custom_base_url_model(provider, &base_url);
        let (api_key, api_key_source) = if auth_disabled {
            (None, None)
        } else if let Some(value) = cli.api_key.clone() {
            (Some(value), Some(RuntimeApiKeySource::Cli))
        } else if uses_kimi_imported_token && !custom_endpoint {
            (None, None)
        } else if (!custom_endpoint || base_url_from_file)
            && let Some(value) = from_file.clone().filter(|value| {
                classify_config_api_key_value(value) == ConfigApiKeyValueKind::Literal
            })
        {
            (Some(value), Some(RuntimeApiKeySource::ConfigFile))
        } else if !custom_endpoint
            && let Some(value) = xiaomi_mimo_env_api_key.filter(|v| !v.trim().is_empty())
        {
            (Some(value), Some(RuntimeApiKeySource::Env))
        } else if custom_endpoint {
            (None, None)
        } else if should_skip_secret_store_for_provider(provider, &base_url, auth_mode.as_deref()) {
            match env_api_key_for_provider(provider) {
                Some(value) => (Some(value), Some(RuntimeApiKeySource::Env)),
                None => (None, None),
            }
        } else {
            match stored_api_key_for_provider(secrets, provider, legacy_ollama_cloud) {
                Some((value, source)) => {
                    let source = match source {
                        SecretSource::Keyring => RuntimeApiKeySource::Keyring,
                        SecretSource::Env => RuntimeApiKeySource::Env,
                    };
                    (Some(value), Some(source))
                }
                None => match env_api_key_for_provider(provider) {
                    Some(value) => (Some(value), Some(RuntimeApiKeySource::Env)),
                    None => (None, None),
                },
            }
        };

        let env_provider_model = env.model_for(provider, &base_url);
        // Root `default_text_model` is the key `codewhale model set` writes and
        // the setup wizard writes, for every provider. It used to enter this
        // chain only when `provider == Deepseek`, which made this resolver
        // disagree with `Config::default_model()` in the TUI — the chain that
        // actually builds the request — for every non-DeepSeek provider
        // (#4832, #4838). The user's model still shipped; only this resolver,
        // and therefore `codewhale model resolve`, reported a provider default.
        //
        // It is honoured for any provider now, minus the one case the DeepSeek
        // gate was accidentally covering: a stale DeepSeek id left behind by a
        // provider switch must not be forwarded to an endpoint that cannot
        // serve it.
        let root_default_model = self
            .default_text_model
            .clone()
            .filter(|model| !root_default_model_is_foreign_to_provider(provider, model, &base_url));
        // Derived from the same chain as `model` below so the reported
        // provenance cannot drift from the id that is actually used.
        let model_source = if cli.model.is_some() {
            ModelSource::Cli
        } else if env.model.is_some() || env_provider_model.is_some() {
            ModelSource::Env
        } else if provider_cfg.model.is_some() {
            ModelSource::ProviderConfig
        } else if root_default_model.is_some() {
            ModelSource::RootDefaultTextModel
        } else if self.model.is_some() {
            ModelSource::RootModel
        } else {
            ModelSource::ProviderDefault
        };
        let explicit_model = model_source.is_explicit();
        let model = cli
            .model
            .clone()
            .or_else(|| env.model.clone())
            .or(env_provider_model)
            .or_else(|| provider_cfg.model.clone())
            .or(root_default_model)
            .or_else(|| self.model.clone())
            .unwrap_or_else(|| {
                if provider == ProviderKind::Moonshot
                    && (auth_mode
                        .as_deref()
                        .is_some_and(auth_mode_uses_kimi_imported_token)
                        || moonshot_base_url_uses_kimi_code(&base_url))
                {
                    DEFAULT_KIMI_CODE_MODEL.to_string()
                } else {
                    cloud_facts::cloud_default_model_for_route(provider, &base_url)
                        .map(|(model, _)| model)
                        .unwrap_or_else(|| default_model_for_provider(provider).to_string())
                }
            });
        let model = if provider == ProviderKind::OpencodeGo {
            // OpenCode Go's `/models` response also contains models that only
            // speak Anthropic Messages. This provider is deliberately bound to
            // Chat Completions, so even custom endpoint/env overrides cannot
            // promote an incompatible id onto `/chat/completions`.
            normalize_model_for_provider(provider, &model)
        } else if explicit_model && provider_preserves_custom_base_url_model(provider, &base_url) {
            model.trim().to_string()
        } else {
            normalize_model_for_provider(provider, &model)
        };

        // RouteResolver is the runtime path: the executable wire model,
        // protocol, and endpoint come from a ReadyRouteCandidate. Auth/key
        // resolution above is unchanged. A resolver error keeps the existing
        // model string so this method stays total, and keeps the error itself
        // so no caller can present the rejected model as a resolved route.
        let route = crate::route::RouteResolver::new().resolve(&crate::route::RouteRequest {
            explicit_provider: Some(provider),
            model_selector: Some(crate::route::LogicalModelRef::from(model.as_str())),
            saved_provider_model: None,
            base_url_override: Some(base_url.clone()),
            limit_overrides: Vec::new(),
            wire_override: None,
        });

        let mut http_headers = self.http_headers.clone();
        http_headers.extend(provider_cfg.http_headers.clone());
        if let Some(env_headers) = env.http_headers {
            http_headers.extend(env_headers);
        }
        http_headers.retain(|name, value| !name.trim().is_empty() && !value.trim().is_empty());
        if auth_disabled {
            http_headers.retain(|name, _| !is_upstream_auth_header(name));
        }

        let log_level = cli
            .log_level
            .clone()
            .or_else(|| env.log_level.clone())
            .or_else(|| self.log_level.clone());
        // The telemetry preference resolves once in the shared core behind
        // [`resolved_telemetry_consent`]. The telemetry owner also checks old
        // durable declines and unreadable privacy state. The
        // comments that matter live there: the
        // environment/file/default chain, and why every kill switch is a
        // floor (`telemetry = false` persisted in the file is the *persistent*
        // off switch; an explicit env "off", an unreadable env value, or a
        // dispatcher-declared floor forces off regardless of CLI flag or
        // config file).
        let (telemetry_env_file, telemetry_source_env_file) = telemetry_consent_from_env(
            env.telemetry,
            env.telemetry_env_invalid,
            env.telemetry_floor,
            self.telemetry,
        );
        // The CLI flag is a run-scoped term on top: `--telemetry false` stops
        // this run; `--telemetry true` can never climb over a kill switch.
        // The source names the CLI only when the CLI term actually decided
        // the outcome — a flag that lost to a kill switch is not the provenance.
        let telemetry = telemetry_env_file && cli.telemetry != Some(false);
        let telemetry_source = if cli.telemetry == Some(false)
            || (cli.telemetry == Some(true) && telemetry_env_file)
        {
            TelemetrySource::Cli
        } else {
            telemetry_source_env_file
        };
        let telemetry_persisted_off = self.telemetry == Some(false);
        // Only a *persisted* off is an answer. `--telemetry false` and
        // `CODEWHALE_TELEMETRY=0` are run-scoped kill switches: they must stop
        // this run without deleting the identity and buffered events of a user
        // who never revoked consent — the dispatcher forwards a resolved
        // `false` on every ordinary run, so treating an environment "off" as an
        // answer would also make the default state indistinguishable from a
        // revocation.
        let telemetry_explicit_off = telemetry_persisted_off;
        // The shipped default is [`DEFAULT_TELEMETRY_ENDPOINT`], and it is a
        // default rather than a floor: an explicit value in the environment or
        // the config file wins outright. An explicit *empty* value is not a
        // missing value — it is the local dry-run sink, and it stays reachable
        // by resolving to `None` instead of falling through to the default.
        //
        // None of this changes the user's opt-out. A session only reaches an
        // endpoint after `telemetry` above resolved true; every persistent and
        // run-scoped kill switch is upstream of this line.
        let telemetry_endpoint = match env
            .telemetry_endpoint
            .clone()
            .or_else(|| self.telemetry_endpoint.clone())
        {
            Some(configured) if configured.trim().is_empty() => None,
            Some(configured) => Some(configured),
            None => Some(DEFAULT_TELEMETRY_ENDPOINT.to_string()),
        };
        let approval_policy = cli
            .approval_policy
            .clone()
            .or_else(|| env.approval_policy.clone())
            .or_else(|| self.approval_policy.clone());
        let sandbox_mode = cli
            .sandbox_mode
            .clone()
            .or_else(|| env.sandbox_mode.clone())
            .or_else(|| self.sandbox_mode.clone());
        let yolo = cli.yolo.or(env.yolo);
        let verbosity = cli
            .verbosity
            .clone()
            .or_else(|| env.verbosity.clone())
            .or_else(|| self.verbosity.clone());

        ResolvedRuntimeOptions {
            provider,
            provider_source,
            model,
            model_source,
            api_key,
            api_key_source,
            base_url,
            auth_mode,
            insecure_skip_tls_verify: provider_cfg.insecure_skip_tls_verify.unwrap_or(false),
            log_level,
            telemetry,
            telemetry_source,
            telemetry_explicit_off,
            telemetry_endpoint,
            approval_policy,
            sandbox_mode,
            yolo,
            verbosity,
            http_headers,
            route,
        }
    }
}

/// Default base URL from the route descriptor, plus the one Moonshot/Kimi
/// imported-token exception that is an auth-mode fact rather than a kind.
pub(crate) fn descriptor_fallback_base_url(
    provider: ProviderKind,
    auth_mode: Option<&str>,
) -> String {
    if provider == ProviderKind::Moonshot
        && auth_mode.is_some_and(auth_mode_uses_kimi_imported_token)
    {
        return DEFAULT_KIMI_CODE_BASE_URL.to_string();
    }
    crate::route::ProviderDescriptor::for_kind(provider)
        .default_base_url()
        .to_string()
}
