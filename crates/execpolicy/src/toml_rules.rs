//! Execpolicy rules loaded from TOML configuration.
//!
//! The legacy Starlark policy engine (`PolicyParser`, `Policy`, `Rule`, the
//! `execpolicy check` CLI verb) was deleted for v0.9.4: the runtime never
//! enforced it, so a green `check` meant nothing. The live policy surface is
//! the TOML `execpolicy.toml` rules here plus this crate's permission engine.
//!
//! Where the file lives is the caller's business — the TUI resolves
//! `~/.deepseek/execpolicy.toml` and hands the parsed [`ExecPolicyConfig`]
//! to its shell tool.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::command_safety::{prefix_allow_matches, prefix_grant_is_eligible};
use crate::matcher::pattern_matches;

/// Verdict of evaluating a command against the TOML rule sets.
///
/// Distinct from [`crate::ExecPolicyDecision`], which is the permission
/// engine's structured output; this is the file-rules verdict consumed by
/// the shell tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleDecision {
    Allow,
    Deny(String),
    AskUser(String),
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ExecPolicyConfig {
    #[serde(default)]
    pub rules: BTreeMap<String, RuleSet>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct RuleSet {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
}

impl ExecPolicyConfig {
    pub fn parse(contents: &str) -> Result<Self> {
        toml::from_str(contents).context("failed to parse execpolicy.toml")
    }

    pub fn from_path(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read execpolicy file {}", path.display()))?;
        Self::parse(&contents)
    }

    pub fn evaluate(&self, command: &str) -> RuleDecision {
        // #security: a deny pattern has to be matched against the commands the
        // shell would actually run, not against the text as written. Quoting,
        // command substitution (`` `cmd` ``, `$(cmd)`), grouping, chaining and
        // wrapper payloads (`bash -c …`, `eval …`, `sudo …`) all produce an
        // invocation whose text differs from the rule while its effect does
        // not. `expanded_commands` word-splits the way a shell does and returns
        // every command line involved, so one deny pattern covers all of the
        // spellings instead of one string pattern per metacharacter.
        //
        // Only the deny loop is widened. The allow loop below still matches the
        // command as written, so a broader expansion can never turn into a
        // broader auto-approval.
        let expansion = crate::shell_expand::expand_command(command);
        let prefix_eligible = prefix_grant_is_eligible(command, &expansion);
        let deny_targets: Vec<String> = expansion.deny_commands().map(ToOwned::to_owned).collect();
        // A command word only known at run time cannot be checked against a
        // deny pattern: fail closed while any deny pattern is configured, and
        // never let an allow pattern written for the outer command approve
        // nested or unresolved code.
        if expansion.dynamic && self.rules.values().any(|rules| !rules.deny.is_empty()) {
            return RuleDecision::Deny(
                "execpolicy: command word cannot be resolved statically while deny rules are in force"
                    .to_string(),
            );
        }
        // A deny pattern names a command prefix, as the permission engine's
        // deny rules do: `git push --force` also denies
        // `git push --force origin main`. The whole-command glob still
        // applies for patterns that spell out `*` wildcards.
        for (group, rules) in &self.rules {
            for pattern in &rules.deny {
                if deny_targets.iter().any(|target| {
                    crate::denied_prefix_matches(pattern, target)
                        || pattern_matches(pattern, target)
                }) {
                    return RuleDecision::Deny(format!("execpolicy denied by {group}: {pattern}"));
                }
            }
        }

        if !prefix_eligible {
            return RuleDecision::AskUser(
                "execpolicy: command syntax or arguments require an exact approval".to_string(),
            );
        }
        for (group, rules) in &self.rules {
            for pattern in &rules.allow {
                // Allow rules use arity-aware prefix matching first so that
                // `allow = ["git status"]` matches `git status -s` but NOT
                // `git push origin main`.  Fall back to regex-style
                // `pattern_matches` for wildcard patterns (e.g. `cargo *`).
                if prefix_allow_matches(pattern, command) || pattern_matches(pattern, command) {
                    let _ = group;
                    return RuleDecision::Allow;
                }
            }
        }

        RuleDecision::AskUser("execpolicy: no matching allow rule".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_execpolicy_evaluate() {
        let config = ExecPolicyConfig {
            rules: BTreeMap::from([
                (
                    "git".to_string(),
                    RuleSet {
                        allow: vec!["git status".to_string(), "git log *".to_string()],
                        deny: vec!["git push --force".to_string()],
                    },
                ),
                (
                    "danger".to_string(),
                    RuleSet {
                        allow: vec![],
                        deny: vec!["rm -rf /".to_string()],
                    },
                ),
            ]),
        };

        assert!(matches!(config.evaluate("git status"), RuleDecision::Allow));
        assert!(matches!(
            config.evaluate("git log --oneline"),
            RuleDecision::Allow
        ));
        assert!(matches!(
            config.evaluate("git push --force"),
            RuleDecision::Deny(_)
        ));
        assert!(matches!(
            config.evaluate("unknown command"),
            RuleDecision::AskUser(_)
        ));
    }

    #[test]
    fn test_prefix_rule_allows_git_status_with_flags() {
        // Arity-aware: `allow = ["git status"]` must match `git status -s`.
        let config = ExecPolicyConfig {
            rules: BTreeMap::from([(
                "git".to_string(),
                RuleSet {
                    allow: vec!["git status".to_string()],
                    deny: vec![],
                },
            )]),
        };

        assert!(matches!(
            config.evaluate("git status -s"),
            RuleDecision::Allow
        ));
        assert!(matches!(
            config.evaluate("git status --porcelain"),
            RuleDecision::Allow
        ));
        // Push must NOT match the "git status" allow rule.
        assert!(matches!(
            config.evaluate("git push origin main"),
            RuleDecision::AskUser(_)
        ));
    }

    fn danger_policy() -> ExecPolicyConfig {
        ExecPolicyConfig {
            rules: BTreeMap::from([(
                "danger".to_string(),
                RuleSet {
                    allow: vec!["echo *".to_string()],
                    deny: vec!["rm -rf /".to_string()],
                },
            )]),
        }
    }

    /// #security: the deny pattern must survive every way a shell can spell the
    /// command it names. A whole-string match saw only the text as typed.
    #[test]
    fn deny_pattern_covers_every_shell_spelling() {
        let config = danger_policy();
        let mut evaded = Vec::new();
        for command in [
            "rm -rf /",
            "ls && rm -rf /",
            "ls & rm -rf /",
            "true; rm -rf /",
            "ls | rm -rf /",
            "ls\nrm -rf /",
            "(rm -rf /)",
            "{ rm -rf /; }",
            "`rm -rf /`",
            "echo `rm -rf /`",
            "echo \"`rm -rf /`\"",
            "$(rm -rf /)",
            "echo $(rm -rf /)",
            "x=$(rm -rf /)",
            "diff <(rm -rf /) b",
            "rm -rf \"/\"",
            "rm -rf '/'",
            "eval 'rm -rf /'",
            "bash -c 'rm -rf /'",
            "sh -lc \"rm -rf /\"",
            "sudo rm -rf /",
            "env rm -rf /",
            "timeout 5 rm -rf /",
            "xargs rm -rf /",
        ] {
            if !matches!(config.evaluate(command), RuleDecision::Deny(_)) {
                evaded.push(command);
            }
        }
        assert!(evaded.is_empty(), "deny pattern bypassed by: {evaded:#?}");
    }

    #[test]
    fn deny_pattern_is_a_command_prefix() {
        let config = ExecPolicyConfig {
            rules: BTreeMap::from([(
                "danger".to_string(),
                RuleSet {
                    allow: vec!["git *".to_string()],
                    deny: vec!["git push --force".to_string(), "rm -rf /".to_string()],
                },
            )]),
        };
        for command in [
            "git push --force",
            "git push --force origin main",
            "git push origin main --force",
            "rm -rf / --no-preserve-root",
            "git -C . push --force",
            "git commit -m 'a\nb' && git push --force",
        ] {
            assert!(
                matches!(config.evaluate(command), RuleDecision::Deny(_)),
                "{command:?} must be denied"
            );
        }
        for command in [
            "git push origin main",
            "git push --force-with-lease",
            "rm -rf ./x",
        ] {
            assert!(
                !matches!(config.evaluate(command), RuleDecision::Deny(_)),
                "{command:?} must not be denied"
            );
        }
    }

    /// The fix must not deny a command merely for containing a metacharacter.
    #[test]
    fn deny_pattern_leaves_harmless_metacharacter_uses_alone() {
        let config = danger_policy();
        for command in [
            // Substitution of something the rule does not name.
            "echo \"built at $(date)\"",
            "echo `date`",
            // Single quotes are literal: this prints the text, runs nothing.
            "echo '`rm -rf /`'",
            "echo 'rm -rf /'",
        ] {
            assert!(
                !matches!(config.evaluate(command), RuleDecision::Deny(_)),
                "harmless command wrongly denied: {command:?}"
            );
        }
    }

    #[test]
    fn test_prefix_rule_allows_cargo_check_variants() {
        let config = ExecPolicyConfig {
            rules: BTreeMap::from([(
                "cargo".to_string(),
                RuleSet {
                    allow: vec!["cargo check".to_string()],
                    deny: vec![],
                },
            )]),
        };

        assert!(matches!(
            config.evaluate("cargo check"),
            RuleDecision::Allow
        ));
        assert!(matches!(
            config.evaluate("cargo check --workspace"),
            RuleDecision::Allow
        ));
        assert!(matches!(
            config.evaluate("cargo build --release"),
            RuleDecision::AskUser(_)
        ));
    }
}
