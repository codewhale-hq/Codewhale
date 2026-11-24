//! Slash commands for the persistent network allow/deny list.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use toml::Value;

use codewhale_command_contract::handler::CommandHandler;
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};

use crate::commands::CommandResult;

pub(in crate::commands) const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "network",
    aliases: &[],
    usage: "/network [list|allow <host>|deny <host>|remove <host>|default <allow|deny|prompt>]",
    description_key: "cmd_network_description",
};

pub(in crate::commands) struct NetworkCmd;

impl RegisterCommand<CommandResult> for NetworkCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Pure(network)
    }
}

fn network(arg: Option<&str>) -> CommandResult {
    match network_inner(arg) {
        Ok(message) => CommandResult::message(message),
        Err(err) => CommandResult::error(err.to_string()),
    }
}

fn network_inner(arg: Option<&str>) -> anyhow::Result<String> {
    let raw = arg.map(str::trim).unwrap_or("");
    if raw.is_empty() || raw.eq_ignore_ascii_case("list") {
        return list_policy();
    }

    let mut parts = raw.split_whitespace();
    let Some(command) = parts.next() else {
        return list_policy();
    };
    let command = command.to_ascii_lowercase();

    match command.as_str() {
        "allow" | "deny" | "remove" | "forget" => {
            let Some(host_arg) = parts.next() else {
                bail!("Usage: /network {command} <host>");
            };
            if parts.next().is_some() {
                bail!("Usage: /network {command} <host>");
            }
            let host = normalize_host_arg(host_arg)?;
            let edit = match command.as_str() {
                "allow" => NetworkEdit::Allow,
                "deny" => NetworkEdit::Deny,
                _ => NetworkEdit::Remove,
            };
            update_host(edit, &host)
        }
        "default" => {
            let Some(value) = parts.next() else {
                bail!("Usage: /network default <allow|deny|prompt>");
            };
            if parts.next().is_some() {
                bail!("Usage: /network default <allow|deny|prompt>");
            }
            update_default(value)
        }
        _ => bail!(usage()),
    }
}

fn usage() -> &'static str {
    "Usage: /network [list|allow <host>|deny <host>|remove <host>|default <allow|deny|prompt>]"
}

#[derive(Clone, Copy)]
enum NetworkEdit {
    Allow,
    Deny,
    Remove,
}

/// Resolve the active config document path through the leaf configuration
/// crate (acyclic; no TUI persistence helper).
fn config_toml_path() -> anyhow::Result<PathBuf> {
    codewhale_config::resolve_config_path(None)
}

fn list_policy() -> anyhow::Result<String> {
    let path = config_toml_path()?;
    let doc = load_config_doc(&path)?;
    let network = doc.get("network").and_then(Value::as_table);
    let default = network
        .and_then(|table| table.get("default"))
        .and_then(Value::as_str)
        .unwrap_or("prompt");
    let allow = network
        .map(|table| string_array(table, "allow"))
        .unwrap_or_default();
    let deny = network
        .map(|table| string_array(table, "deny"))
        .unwrap_or_default();

    Ok(format!(
        "Network policy ({})\n\
         default = {default}\n\
         allow = {}\n\
         deny = {}\n\n\
         Use `/network allow <host>` to allow a host, `/network deny <host>` to block it, or `/network remove <host>` to clear an entry.",
        path.display(),
        display_list(&allow),
        display_list(&deny)
    ))
}

fn update_host(edit: NetworkEdit, host: &str) -> anyhow::Result<String> {
    let path = config_toml_path()?;
    codewhale_config::mutate_config_document(&path, |doc| {
        ensure_network_defaults(doc)?;
        let mut allow = document_string_array(doc, "allow")?;
        let mut deny = document_string_array(doc, "deny")?;
        match edit {
            NetworkEdit::Allow => {
                remove_host(&mut deny, host);
                add_host(&mut allow, host);
            }
            NetworkEdit::Deny => {
                remove_host(&mut allow, host);
                add_host(&mut deny, host);
            }
            NetworkEdit::Remove => {
                remove_host(&mut allow, host);
                remove_host(&mut deny, host);
            }
        }
        codewhale_config::set_config_document_value(
            doc,
            &["network", "allow"],
            string_array_value(&allow),
        )?;
        codewhale_config::set_config_document_value(
            doc,
            &["network", "deny"],
            string_array_value(&deny),
        )
    })?;
    let action = match edit {
        NetworkEdit::Allow => "allowed",
        NetworkEdit::Deny => "denied",
        NetworkEdit::Remove => "removed",
    };
    Ok(format!(
        "Network host {action}: {host}\nSaved to {}. Retry the command now.",
        path.display()
    ))
}

fn update_default(value: &str) -> anyhow::Result<String> {
    let normalized = match value.trim().to_ascii_lowercase().as_str() {
        "allow" => "allow",
        "deny" | "block" => "deny",
        "prompt" | "ask" => "prompt",
        _ => bail!("Usage: /network default <allow|deny|prompt>"),
    };

    let path = config_toml_path()?;
    codewhale_config::mutate_config_document(&path, |doc| {
        ensure_network_defaults(doc)?;
        codewhale_config::set_config_document_value(doc, &["network", "default"], normalized)
    })?;

    Ok(format!(
        "Network default set to {normalized}\nSaved to {}.",
        path.display()
    ))
}

fn load_config_doc(path: &Path) -> anyhow::Result<Value> {
    if !path.exists() {
        return Ok(Value::Table(toml::value::Table::new()));
    }
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read config at {}", path.display()))?;
    toml::from_str(&raw).map_err(|_| {
        anyhow::anyhow!(
            "failed to parse config at {}; file contents were omitted",
            codewhale_config::quote_os_path(path)
        )
    })
}

fn ensure_network_defaults(doc: &mut toml_edit::DocumentMut) -> anyhow::Result<()> {
    if doc
        .get("network")
        .and_then(toml_edit::Item::as_table_like)
        .and_then(|table| table.get("default"))
        .is_none()
    {
        codewhale_config::set_config_document_value(doc, &["network", "default"], "prompt")?;
    }
    if doc
        .get("network")
        .and_then(toml_edit::Item::as_table_like)
        .and_then(|table| table.get("audit"))
        .is_none()
    {
        codewhale_config::set_config_document_value(doc, &["network", "audit"], true)?;
    }
    Ok(())
}

fn document_string_array(doc: &toml_edit::DocumentMut, key: &str) -> anyhow::Result<Vec<String>> {
    let Some(item) = doc
        .get("network")
        .and_then(toml_edit::Item::as_table_like)
        .and_then(|table| table.get(key))
    else {
        return Ok(Vec::new());
    };
    let array = item
        .as_array()
        .with_context(|| format!("`network.{key}` must be an array of strings"))?;
    array
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(ToString::to_string)
                .with_context(|| format!("`network.{key}` must be an array of strings"))
        })
        .collect()
}

fn string_array_value(values: &[String]) -> toml_edit::Array {
    values.iter().map(String::as_str).collect()
}

fn string_array(table: &toml::value::Table, key: &str) -> Vec<String> {
    table
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(ToString::to_string)
        .collect()
}

fn add_host(list: &mut Vec<String>, host: &str) {
    if !list
        .iter()
        .any(|existing| normalize_host_for_compare(existing) == host)
    {
        list.push(host.to_string());
    }
}

fn remove_host(list: &mut Vec<String>, host: &str) {
    list.retain(|existing| normalize_host_for_compare(existing) != host);
}

fn normalize_host_arg(input: &str) -> anyhow::Result<String> {
    let trimmed = input.trim();
    let host = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        host_from_url(trimmed).context("URL must include a host")?
    } else {
        if trimmed.contains("://") || trimmed.contains('/') {
            bail!("Pass a host like `github.com`, not a URL path");
        }
        trimmed.to_string()
    };

    let normalized = normalize_host_for_compare(&host);
    if normalized.is_empty() {
        bail!("host cannot be empty");
    }
    Ok(normalized)
}

/// Extract the host portion of a URL, lowercased (leaf `reqwest::Url` parse;
/// no TUI helper dependency).
fn host_from_url(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url.trim()).ok()?;
    parsed.host_str().map(str::to_ascii_lowercase)
}

fn normalize_host_for_compare(host: &str) -> String {
    let trimmed = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if let Some(rest) = trimmed.strip_prefix("*.") {
        format!(".{rest}")
    } else {
        trimmed
    }
}

fn display_list(values: &[String]) -> String {
    if values.is_empty() {
        "[]".to_string()
    } else {
        format!("[{}]", values.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EnvGuard {
        _home: crate::test_support::EnvVarGuard,
        _userprofile: crate::test_support::EnvVarGuard,
        _codewhale_config_path: crate::test_support::EnvVarGuard,
        _deepseek_config_path: crate::test_support::EnvVarGuard,
        _lock: crate::test_support::TestEnvLock,
    }

    impl EnvGuard {
        fn new(home: &Path) -> Self {
            let lock = crate::test_support::lock_test_env();
            let config_path = home.join(".deepseek").join("config.toml");
            Self {
                _home: crate::test_support::EnvVarGuard::set("HOME", home),
                _userprofile: crate::test_support::EnvVarGuard::set("USERPROFILE", home),
                _codewhale_config_path: crate::test_support::EnvVarGuard::set(
                    "CODEWHALE_CONFIG_PATH",
                    &config_path,
                ),
                _deepseek_config_path: crate::test_support::EnvVarGuard::set(
                    "DEEPSEEK_CONFIG_PATH",
                    &config_path,
                ),
                _lock: lock,
            }
        }
    }

    fn temp_home(label: &str) -> PathBuf {
        let path = crate::test_support::test_scratch_dir(&format!("deepseek-network-{label}"));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn network_allow_persists_host_and_removes_exact_deny() {
        let home = temp_home("allow");
        let _guard = EnvGuard::new(&home);
        let config_path = home.join(".deepseek").join("config.toml");
        fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        fs::write(
            &config_path,
            "[network]\ndefault = \"prompt\"\ndeny = [\"github.com\"]\n",
        )
        .unwrap();

        let result = network(Some("allow GitHub.COM"));

        assert!(!result.is_error, "{:?}", result.message);
        let body = fs::read_to_string(config_path).unwrap();
        assert!(body.contains("allow = [\"github.com\"]"), "{body}");
        assert!(body.contains("deny = []"), "{body}");
    }

    #[test]
    fn network_allow_extracts_host_from_url() {
        let home = temp_home("url");
        let _guard = EnvGuard::new(&home);

        let result = network(Some("allow https://github.com/obra/superpowers"));

        assert!(!result.is_error, "{:?}", result.message);
        let body = fs::read_to_string(home.join(".deepseek").join("config.toml")).unwrap();
        assert!(body.contains("allow = [\"github.com\"]"), "{body}");
    }

    #[test]
    fn network_default_rejects_unknown_value() {
        let home = temp_home("default");
        let _guard = EnvGuard::new(&home);

        let result = network(Some("default maybe"));

        assert!(result.is_error);
        assert!(
            result
                .message
                .as_deref()
                .unwrap_or_default()
                .contains("/network default <allow|deny|prompt>")
        );
    }

    #[test]
    fn network_config_parse_error_omits_secret_contents_and_keys() {
        let home = temp_home("parse-redaction");
        let path = home.join("config.toml");
        let secret = "cw-secret-network-config-4507";
        fs::write(
            &path,
            format!("[providers.xai]\napi_key = \"{secret}\" trailing-junk\n"),
        )
        .unwrap();

        let error = load_config_doc(&path).expect_err("malformed config must fail");
        let diagnostic = format!("{error:#}");
        assert!(!diagnostic.contains(secret), "{diagnostic}");
        assert!(!diagnostic.contains("api_key"), "{diagnostic}");
        assert!(
            diagnostic.contains("file contents were omitted"),
            "{diagnostic}"
        );
    }

    #[test]
    fn handler_is_pure_and_argument_only() {
        assert!(matches!(NetworkCmd::handler(), CommandHandler::Pure(_)));
        assert_eq!(
            NetworkCmd::info().description_key,
            "cmd_network_description"
        );
        assert_eq!(NetworkCmd::info().aliases, &[] as &[&str]);
    }

    #[test]
    fn host_normalization_handles_wildcard_trailing_dot_and_url_paths() {
        // Wildcard prefix normalizes to a leading-dot suffix form.
        assert_eq!(normalize_host_for_compare("*.example.com"), ".example.com");
        // Trailing dot and case are normalized away.
        assert_eq!(normalize_host_for_compare("Example.COM."), "example.com");
        // A bare wildcard suffix compares equal to its explicit form.
        assert_eq!(normalize_host_for_compare("*.github.com"), ".github.com");

        // URL input extracts the host; a URL path is rejected.
        assert_eq!(
            host_from_url("https://API.Github.com/path").as_deref(),
            Some("api.github.com")
        );
        assert_eq!(host_from_url("not a url"), None);
        assert!(normalize_host_arg("https://github.com/path").is_ok());
        assert!(normalize_host_arg("a/b").is_err(), "URL path rejected");
        assert!(
            normalize_host_arg("https://").is_err(),
            "hostless URL rejected"
        );
    }

    #[test]
    fn exact_conflict_removal_is_case_and_wildcard_aware() {
        let mut allow = vec!["github.com".to_string()];
        let mut deny = vec!["GitHub.COM".to_string(), "*.example.com".to_string()];
        // Production normalizes the host argument before update_host; the
        // normalized form then removes the exact deny entry (case-insensitive).
        let host = normalize_host_arg("GitHub.COM").expect("normalize");
        assert_eq!(host, "github.com");
        remove_host(&mut deny, &host);
        assert_eq!(deny, vec!["*.example.com".to_string()]);
        // Denying removes the exact allow entry.
        remove_host(&mut allow, &host);
        assert!(allow.is_empty());
        // Adding an existing normalized host is a no-op (production passes
        // the already-normalized host into add_host).
        add_host(&mut allow, "github.com");
        add_host(&mut allow, "github.com");
        assert_eq!(allow, vec!["github.com".to_string()]);
    }
}
