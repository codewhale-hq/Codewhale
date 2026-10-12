//! Feature flags and metadata for codewhale.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};

use serde::{Deserialize, Deserializer, Serialize, de};

/// Lifecycle stage for a feature flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Experimental,
    Beta,
    Stable,
}

impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Experimental => "experimental",
            Self::Beta => "beta",
            Self::Stable => "stable",
        }
    }
}

/// Unique features toggled via configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Feature {
    /// Enable the default shell tool.
    ShellTool,
    /// Enable background sub-agent tooling.
    Subagents,
    /// Enable web search tool.
    WebSearch,
    /// Enable apply_patch tool.
    ApplyPatch,
    /// Enable MCP tools.
    Mcp,
    /// Enable execpolicy integration/tooling.
    ExecPolicy,
    /// Enable vision model for image analysis.
    VisionModel,
    /// Enable the agent-callable `verify` adversarial self-critique tool (#4196).
    Verify,
    /// Expose `execute_tools` eagerly so the model composes by default (CodeMode).
    CodeMode,
    /// Run reviewed plugins' `native` host code in the TypeScript extension
    /// host (experimental). Toggling it changes the plugin activation policy,
    /// so every plugin is re-reviewed after a restart, in either direction.
    ExtensionHost,
    /// Use the pinned TypeScript finance normalization adapter; no runtime fallback.
    FinanceHost,
    /// Use the pinned TypeScript data validation presenter; no runtime fallback.
    DataHost,
    /// Use pinned speech preparation for the CLI and tool; no runtime fallback.
    SpeechHost,
    /// Pinned PDF outcome orchestration; Core retains process and document bytes.
    PdfHost,
    /// Pinned Native/Tesseract selection; Core retains image/process authority.
    OcrHost,
    /// Pinned web query/provider adapters; Core retains all HTTP and policy.
    WebSearchHost,
    /// Pinned HTML region selection; complete document bytes stay in Core.
    WebExtractHost,
    /// Pinned GitHub and local report presentation; Core retains effects and disclosure.
    GithubHost,
    /// Captured review presentation; Core retains provider, evidence and publication.
    ReviewHost,
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Holds the effective set of enabled features.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Features {
    enabled: BTreeSet<Feature>,
}

impl Features {
    /// Starts with built-in defaults.
    pub fn with_defaults() -> Self {
        let mut set = BTreeSet::new();
        for spec in FEATURES {
            if spec.default_enabled {
                set.insert(spec.id);
            }
        }
        Self { enabled: set }
    }

    pub fn enabled(&self, feature: Feature) -> bool {
        self.enabled.contains(&feature)
    }

    pub fn enable(&mut self, feature: Feature) -> &mut Self {
        self.enabled.insert(feature);
        self
    }

    pub fn disable(&mut self, feature: Feature) -> &mut Self {
        self.enabled.remove(&feature);
        self
    }

    pub fn apply_map(&mut self, entries: &BTreeMap<String, bool>) {
        for (key, enabled) in entries {
            if let Some(feature) = feature_from_key(key) {
                if *enabled {
                    self.enable(feature);
                } else {
                    self.disable(feature);
                }
            }
        }
    }
}

/// Keys accepted in `[features]` tables.
pub fn is_known_feature_key(key: &str) -> bool {
    FEATURES.iter().any(|spec| spec.key == key)
}

pub fn feature_from_key(key: &str) -> Option<Feature> {
    FEATURES
        .iter()
        .find(|spec| spec.key == key)
        .map(|spec| spec.id)
}

pub fn render_feature_table(features: &Features) -> String {
    let mut output = String::from("feature\tstage\tenabled\n");
    for spec in FEATURES {
        let _ = writeln!(
            output,
            "{}\t{}\t{}",
            spec.key,
            spec.stage,
            features.enabled(spec.id)
        );
    }
    output
}

/// Deserializable features table for TOML.
#[derive(Serialize, Debug, Clone, Default, PartialEq)]
pub struct FeaturesToml {
    #[serde(flatten)]
    pub entries: BTreeMap<String, bool>,
}

impl<'de> Deserialize<'de> for FeaturesToml {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = BTreeMap::<String, toml::Value>::deserialize(deserializer)?;
        let mut entries = BTreeMap::new();

        for (key, value) in raw {
            match value {
                toml::Value::Boolean(enabled) => {
                    entries.insert(key, enabled);
                }
                toml::Value::Table(table) if key == "enabled" => {
                    for (feature_key, feature_value) in table {
                        match feature_value {
                            toml::Value::Boolean(enabled) => {
                                entries.insert(feature_key, enabled);
                            }
                            other => {
                                return Err(de::Error::custom(format!(
                                    "features.enabled.{feature_key} must be a boolean, got {other:?}"
                                )));
                            }
                        }
                    }
                }
                other if is_known_feature_key(&key) => {
                    return Err(de::Error::custom(format!(
                        "features.{key} must be a boolean, got {other:?}"
                    )));
                }
                _ => {}
            }
        }

        Ok(Self { entries })
    }
}

/// Single registry of all feature definitions.
#[derive(Debug, Clone, Copy)]
pub struct FeatureSpec {
    pub id: Feature,
    pub key: &'static str,
    pub stage: Stage,
    pub default_enabled: bool,
}

pub const FEATURES: &[FeatureSpec] = &[
    FeatureSpec {
        id: Feature::ShellTool,
        key: "shell_tool",
        stage: Stage::Stable,
        default_enabled: true,
    },
    FeatureSpec {
        id: Feature::Subagents,
        key: "subagents",
        stage: Stage::Stable,
        default_enabled: true,
    },
    FeatureSpec {
        id: Feature::WebSearch,
        key: "web_search",
        stage: Stage::Stable,
        default_enabled: true,
    },
    FeatureSpec {
        id: Feature::ApplyPatch,
        key: "apply_patch",
        stage: Stage::Stable,
        default_enabled: true,
    },
    FeatureSpec {
        id: Feature::Mcp,
        key: "mcp",
        stage: Stage::Stable,
        default_enabled: true,
    },
    FeatureSpec {
        id: Feature::ExecPolicy,
        key: "exec_policy",
        stage: Stage::Stable,
        default_enabled: true,
    },
    FeatureSpec {
        id: Feature::VisionModel,
        key: "vision_model",
        stage: Stage::Beta,
        default_enabled: false,
    },
    FeatureSpec {
        id: Feature::Verify,
        key: "verify_tool",
        stage: Stage::Stable,
        default_enabled: true,
    },
    FeatureSpec {
        id: Feature::CodeMode,
        key: "code_mode",
        // Stays listed as experimental so `/config` shows the escape hatch.
        stage: Stage::Experimental,
        // #6562: code mode is the default way MCP and plugin tools compose.
        // `[features] code_mode = false` is the escape hatch: execute_tools
        // goes back to deferred (reachable through tool_search).
        default_enabled: true,
    },
    FeatureSpec {
        id: Feature::FinanceHost,
        key: "finance_host",
        stage: Stage::Experimental,
        default_enabled: false,
    },
    FeatureSpec {
        id: Feature::DataHost,
        key: "data_host",
        stage: Stage::Experimental,
        default_enabled: false,
    },
    FeatureSpec {
        id: Feature::SpeechHost,
        key: "speech_host",
        stage: Stage::Experimental,
        default_enabled: false,
    },
    FeatureSpec {
        id: Feature::PdfHost,
        key: "pdf_host",
        stage: Stage::Experimental,
        default_enabled: false,
    },
    FeatureSpec {
        id: Feature::OcrHost,
        key: "ocr_host",
        stage: Stage::Experimental,
        default_enabled: false,
    },
    FeatureSpec {
        id: Feature::GithubHost,
        key: "github_host",
        stage: Stage::Experimental,
        default_enabled: false,
    },
    FeatureSpec {
        id: Feature::WebSearchHost,
        key: "web_search_host",
        stage: Stage::Experimental,
        default_enabled: false,
    },
    FeatureSpec {
        id: Feature::WebExtractHost,
        key: "web_extract_host",
        stage: Stage::Experimental,
        default_enabled: false,
    },
    FeatureSpec {
        id: Feature::ReviewHost,
        key: "review_host",
        stage: Stage::Experimental,
        default_enabled: false,
    },
    FeatureSpec {
        id: Feature::ExtensionHost,
        key: "extension_host",
        stage: Stage::Experimental,
        default_enabled: false,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_map_toggles_known_features_and_ignores_unknown_keys() {
        let mut features = Features::with_defaults();
        let entries = BTreeMap::from([
            ("mcp".to_string(), false),
            ("shell_tool".to_string(), false),
            ("not_real".to_string(), false),
        ]);

        features.apply_map(&entries);

        assert!(!features.enabled(Feature::Mcp));
        assert!(!features.enabled(Feature::ShellTool));
        assert_eq!(feature_from_key("not_real"), None);
    }

    #[test]
    fn code_mode_flag_parses_and_defaults_on_with_an_off_switch() {
        assert_eq!(feature_from_key("code_mode"), Some(Feature::CodeMode));
        let mut features = Features::with_defaults();
        assert!(features.enabled(Feature::CodeMode));
        features.apply_map(&BTreeMap::from([("code_mode".to_string(), false)]));
        assert!(!features.enabled(Feature::CodeMode));
    }

    #[test]
    fn extension_host_flag_parses_and_defaults_off() {
        assert_eq!(
            feature_from_key("extension_host"),
            Some(Feature::ExtensionHost)
        );
        assert!(!Features::with_defaults().enabled(Feature::ExtensionHost));
    }

    #[test]
    fn render_feature_table_uses_registry_order_and_effective_state() {
        let mut features = Features::with_defaults();
        features.disable(Feature::Mcp);

        let table = render_feature_table(&features);
        let lines = table.lines().collect::<Vec<_>>();

        assert_eq!(lines.first(), Some(&"feature\tstage\tenabled"));
        assert!(lines.contains(&"shell_tool\tstable\ttrue"));
        assert!(lines.contains(&"mcp\tstable\tfalse"));
    }
}
