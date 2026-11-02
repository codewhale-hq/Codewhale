use crate::config_toml::ConfigToml;
use crate::settings_schema::{SettingDef, SettingKind};
use anyhow::Context as _;
use anyhow::{Result, bail};
use std::collections::BTreeMap;

/// `value` as the TOML value a [`SettingDef`] declares, or an error naming
/// the key and what it accepts. No partial write happens on error.
pub(crate) fn schema_toml_value(key: &str, def: &SettingDef, value: &str) -> Result<toml::Value> {
    let trimmed = value.trim();
    Ok(match def.kind {
        SettingKind::Bool(_) => toml::Value::Boolean(
            parse_bool(value).with_context(|| format!("invalid value for '{key}'"))?,
        ),
        SettingKind::Int => toml::Value::Integer(trimmed.parse().map_err(|_| {
            anyhow::anyhow!("invalid value '{value}' for '{key}': expected an integer")
        })?),
        SettingKind::Float => toml::Value::Float(
            trimmed
                .parse::<f64>()
                .ok()
                .filter(|number| number.is_finite())
                .ok_or_else(|| {
                    anyhow::anyhow!("invalid value '{value}' for '{key}': expected a number")
                })?,
        ),
        SettingKind::Enum(options) => {
            let Some(option) = options
                .iter()
                .find(|option| option.value.eq_ignore_ascii_case(trimmed))
            else {
                let expected: Vec<&str> = options.iter().map(|option| option.value).collect();
                bail!(
                    "invalid value '{value}' for '{key}': expected one of {}",
                    expected.join(", ")
                );
            };
            toml::Value::String(option.value.to_string())
        }
        SettingKind::String => toml::Value::String(value.to_string()),
    })
}

pub(crate) fn parse_bool(raw: &str) -> Result<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" | "enabled" => Ok(true),
        "0" | "false" | "no" | "off" | "disabled" => Ok(false),
        _ => bail!("invalid boolean '{raw}'"),
    }
}

pub(crate) fn parse_http_headers(raw: &str) -> Result<BTreeMap<String, String>> {
    let mut headers = BTreeMap::new();
    for pair in raw.trim().split(',') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let Some((name, value)) = pair.split_once('=') else {
            bail!("invalid header pair '{pair}', expected name=value");
        };
        let name = name.trim();
        let value = value.trim();
        if name.is_empty() {
            bail!("header name cannot be empty");
        }
        if value.is_empty() {
            continue;
        }
        headers.insert(name.to_string(), value.to_string());
    }
    Ok(headers)
}

pub(crate) fn serialize_http_headers(headers: &BTreeMap<String, String>) -> Option<String> {
    if headers.is_empty() {
        return None;
    }
    Some(
        headers
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join(","),
    )
}

pub(crate) fn serialize_http_headers_for_display(
    headers: &BTreeMap<String, String>,
) -> Option<String> {
    if headers.is_empty() {
        return None;
    }
    Some(
        headers
            .iter()
            .map(|(name, value)| {
                let display_value = if is_sensitive_config_key(name) {
                    redact_secret(value)
                } else {
                    value.clone()
                };
                format!("{name}={display_value}")
            })
            .collect::<Vec<_>>()
            .join(","),
    )
}

pub(crate) fn redact_secret(secret: &str) -> String {
    let chars: Vec<char> = secret.chars().collect();
    if chars.len() <= 16 {
        return "********".to_string();
    }
    let prefix: String = chars.iter().take(4).collect();
    let suffix: String = chars
        .iter()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{prefix}***{suffix}")
}

#[must_use]
pub fn is_sensitive_config_key(key: &str) -> bool {
    key.rsplit('.')
        .next()
        .is_some_and(codewhale_secrets::sanitize::is_sensitive_key_name)
}

/// Resolve dotted paths without treating a dotted key as a top-level literal.
pub(crate) fn config_value_at_path<'a>(
    value: &'a toml::Value,
    key: &str,
) -> Option<&'a toml::Value> {
    key.split('.')
        .try_fold(value, |value, part| value.get(part))
}

pub(crate) fn redact_toml_value_for_display(key: &str, value: &toml::Value) -> String {
    redact_toml_value_for_display_inner(key, false, value).to_string()
}

impl ConfigToml {
    /// Redacted TOML rendering of the effective config for `config dump`.
    /// Structure is preserved; strings under sensitive key names (api_key,
    /// token, secret, … — see `is_sensitive_config_key`, nested tables
    /// inherit sensitivity from their ancestors) are redacted. Redaction is
    /// name-based: an unrecognized key holding a secret-looking value would
    /// pass through, so pasting dump output still deserves a glance.
    #[must_use]
    pub fn redacted_toml_value(&self) -> toml::Value {
        let value =
            toml::Value::try_from(self).expect("ConfigToml derives Serialize, so this holds");
        match value {
            toml::Value::Table(table) => toml::Value::Table(
                table
                    .into_iter()
                    .map(|(key, value)| {
                        let redacted = redact_toml_value_for_display_inner(&key, false, &value);
                        (key, redacted)
                    })
                    .collect(),
            ),
            other => other,
        }
    }
}

pub(crate) fn toml_value_as_u64(value: &toml::Value) -> Option<u64> {
    match value {
        toml::Value::Integer(value) => u64::try_from(*value).ok(),
        toml::Value::String(value) => value.trim().parse().ok(),
        _ => None,
    }
}

pub(crate) fn redact_toml_value_for_display_inner(
    key: &str,
    sensitive_ancestor: bool,
    value: &toml::Value,
) -> toml::Value {
    let sensitive = sensitive_ancestor || is_sensitive_config_key(key);
    match value {
        toml::Value::String(value) if sensitive => toml::Value::String(redact_secret(value)),
        toml::Value::Array(values) => toml::Value::Array(
            values
                .iter()
                .map(|value| redact_toml_value_for_display_inner(key, sensitive, value))
                .collect(),
        ),
        toml::Value::Table(table) => {
            let mut redacted = toml::map::Map::new();
            for (child_key, child_value) in table {
                let path = if key.is_empty() {
                    child_key.clone()
                } else {
                    format!("{key}.{child_key}")
                };
                redacted.insert(
                    child_key.clone(),
                    redact_toml_value_for_display_inner(&path, sensitive, child_value),
                );
            }
            toml::Value::Table(redacted)
        }
        _ if sensitive => toml::Value::String("********".to_string()),
        _ => value.clone(),
    }
}
