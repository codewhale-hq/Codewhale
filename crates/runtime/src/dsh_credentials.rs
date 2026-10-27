//! Read-only DeepSeek Harness credential import.
//!
//! Official `dsh` stores API keys as a YAML mapping in
//! `$DSH_HOME/.credentials.yaml`. Codewhale may read `DEEPSEEK_API_KEY` from
//! that exact file only after `codewhale auth external-consent`. The file is
//! never written, refreshed, or loaded into the process environment.

use anyhow::{Result, bail};
use codewhale_config::ExternalCredentialReadGrant;

const DEEPSEEK_API_KEY_REF: &str = "DEEPSEEK_API_KEY";

/// Extract the DeepSeek API key from a granted dsh credentials document.
pub fn deepseek_api_key_from_grant(grant: &ExternalCredentialReadGrant) -> Result<Option<String>> {
    if grant.source() != codewhale_config::ExternalCredentialSource::DshCli {
        bail!(
            "DeepSeek Harness import requires a dsh_cli grant, not {}",
            grant.source().as_str()
        );
    }
    let Some(text) = crate::external_credentials::read_to_string(grant)? else {
        return Ok(None);
    };
    parse_dsh_deepseek_api_key(&text)
}

/// Strict subset of dsh-credentials-local: a mapping of POSIX identifiers to
/// non-empty strings. Nested values, empty strings, and duplicate keys fail
/// closed. Only `DEEPSEEK_API_KEY` is returned.
pub fn parse_dsh_deepseek_api_key(text: &str) -> Result<Option<String>> {
    let mut found = None;
    let mut seen = std::collections::BTreeSet::new();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            bail!(
                "DeepSeek Harness credentials line {} is not `KEY: value`",
                index + 1
            );
        };
        let key = key.trim();
        if !is_posix_identifier(key) {
            bail!(
                "DeepSeek Harness credentials line {} has a non-identifier key",
                index + 1
            );
        }
        if !seen.insert(key.to_string()) {
            bail!("DeepSeek Harness credentials declare `{key}` more than once");
        }
        let value = unquote_yaml_string(value.trim()).map_err(|reason| {
            anyhow::anyhow!(
                "DeepSeek Harness credentials line {} is invalid: {reason}",
                index + 1
            )
        })?;
        if value.is_empty() {
            bail!(
                "DeepSeek Harness credentials line {} has an empty value",
                index + 1
            );
        }
        if key == DEEPSEEK_API_KEY_REF {
            found = Some(value);
        }
    }
    Ok(found)
}

fn is_posix_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some('A'..='Z' | 'a'..='z' | '_'))
        && chars.all(|ch| matches!(ch, 'A'..='Z' | 'a'..='z' | '0'..='9' | '_'))
}

fn unquote_yaml_string(value: &str) -> Result<String, &'static str> {
    if value.starts_with('{')
        || value.starts_with('[')
        || value.starts_with('|')
        || value.starts_with('>')
    {
        return Err("nested or block YAML is not supported");
    }
    if let Some(inner) = value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    {
        if inner.contains('\\') {
            return Err("escaped quoted strings are not supported");
        }
        return Ok(inner.to_string());
    }
    if let Some(inner) = value
        .strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
    {
        if inner.contains('\'') {
            return Err("escaped single-quoted strings are not supported");
        }
        return Ok(inner.to_string());
    }
    if value.contains(':') && value.contains(' ') {
        return Err("unquoted mapping values are not supported");
    }
    Ok(value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_quoted_deepseek_key() {
        assert_eq!(
            parse_dsh_deepseek_api_key("DEEPSEEK_API_KEY: sk-live\n").unwrap(),
            Some("sk-live".to_string())
        );
        assert_eq!(
            parse_dsh_deepseek_api_key(
                "DEEPSEEK_API_KEY: \"sk-quoted\"\nOPENAI_API_KEY: sk-other\n"
            )
            .unwrap(),
            Some("sk-quoted".to_string())
        );
    }

    #[test]
    fn missing_deepseek_key_is_absent_not_an_error() {
        assert_eq!(
            parse_dsh_deepseek_api_key("OPENAI_API_KEY: sk-other\n").unwrap(),
            None
        );
    }

    #[test]
    fn rejects_empty_values_duplicates_and_nested_yaml() {
        assert!(parse_dsh_deepseek_api_key("DEEPSEEK_API_KEY:\n").is_err());
        assert!(
            parse_dsh_deepseek_api_key("DEEPSEEK_API_KEY: sk-a\nDEEPSEEK_API_KEY: sk-b\n").is_err()
        );
        assert!(parse_dsh_deepseek_api_key("DEEPSEEK_API_KEY: {nested: true}\n").is_err());
    }
}
