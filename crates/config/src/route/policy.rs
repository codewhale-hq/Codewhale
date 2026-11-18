//! Catalog policy evaluated after every catalog layer (Phase 1 hook).
//!
//! Policy decides whether an allowed offering may be used. It never configures
//! endpoints or credentials, and it never makes an unusable route usable.
//! `DENY` is applied last and is never overridden by a catalog layer.

use serde::{Deserialize, Serialize};

/// Effect of one policy rule. Last match wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PolicyEffect {
    /// Permit the matched resource.
    Allow,
    /// Forbid the matched resource. Never overridden by a later catalog layer.
    Deny,
}

/// Action a policy rule addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PolicyAction {
    /// Using a model on a route (`<route-id>/<model-id>`).
    ModelUse,
    /// Using a route at all (`<route-id>`).
    ProviderUse,
}

/// One wildcard-matched policy rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyRule {
    /// Allow or deny.
    pub effect: PolicyEffect,
    /// Action being gated.
    pub action: PolicyAction,
    /// Resource glob: `"<route-id>/<model-id>"` or `"<route-id>"`.
    pub resource: String,
}

/// Ordered policy document. Empty means allow-all.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogPolicy {
    /// Rules in file order; last match wins.
    #[serde(default)]
    pub rules: Vec<PolicyRule>,
}

impl CatalogPolicy {
    /// Empty policy (allow everything).
    #[must_use]
    pub fn allow_all() -> Self {
        Self::default()
    }

    /// Whether `route_id` / `model_id` survives policy.
    ///
    /// Default is allow. Each matching rule overwrites the decision; the last
    /// match wins. A final DENY cannot be undone by a catalog layer because
    /// callers apply this *after* merge.
    #[must_use]
    pub fn allows(&self, route_id: &str, model_id: &str) -> bool {
        let mut allowed = true;
        for rule in &self.rules {
            if rule.matches(route_id, model_id) {
                allowed = rule.effect == PolicyEffect::Allow;
            }
        }
        allowed
    }
}

impl PolicyRule {
    fn matches(&self, route_id: &str, model_id: &str) -> bool {
        let resource = match self.action {
            PolicyAction::ProviderUse => route_id.to_string(),
            PolicyAction::ModelUse => format!("{route_id}/{model_id}"),
        };
        wildcard_match(&self.resource, &resource)
    }
}

/// Single-`*` wildcard match. `*` does not cross `/`.
pub(crate) fn wildcard_match(pattern: &str, value: &str) -> bool {
    wildcard_match_parts(pattern.as_bytes(), value.as_bytes())
}

fn wildcard_match_parts(pattern: &[u8], value: &[u8]) -> bool {
    let mut p = 0;
    let mut v = 0;
    let mut star = None;
    while v < value.len() {
        if p < pattern.len() && pattern[p] == b'*' {
            star = Some((p, v));
            p += 1;
            continue;
        }
        if p < pattern.len() && pattern[p] == value[v] {
            p += 1;
            v += 1;
            continue;
        }
        if let Some((star_p, star_v)) = star {
            if value[star_v] == b'/' {
                return false;
            }
            v = star_v + 1;
            p = star_p + 1;
            star = Some((star_p, v));
            continue;
        }
        return false;
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deny_survives_every_layer_order() {
        let policy = CatalogPolicy {
            rules: vec![PolicyRule {
                effect: PolicyEffect::Deny,
                action: PolicyAction::ModelUse,
                resource: "*-cn/*".to_string(),
            }],
        };
        assert!(!policy.allows("zai-coding-cn", "glm-5"));
        assert!(policy.allows("zai", "glm-5"));
        assert!(policy.allows("deepseek", "deepseek-v4-pro"));
    }

    #[test]
    fn last_match_wins() {
        let policy = CatalogPolicy {
            rules: vec![
                PolicyRule {
                    effect: PolicyEffect::Deny,
                    action: PolicyAction::ProviderUse,
                    resource: "deepseek".to_string(),
                },
                PolicyRule {
                    effect: PolicyEffect::Allow,
                    action: PolicyAction::ProviderUse,
                    resource: "deepseek".to_string(),
                },
            ],
        };
        assert!(policy.allows("deepseek", "any"));
    }

    #[test]
    fn empty_policy_allows() {
        assert!(CatalogPolicy::allow_all().allows("anything", "model"));
    }
}
