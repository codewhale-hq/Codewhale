//! Reasoning preferences and effective tiers (#5316).
//!
//! Separated from TUI app state so engine, client routing, settings, and
//! tools can resolve and normalize reasoning effort without depending on
//! presentation types.

use codewhale_config::ProviderKind;
use codewhale_models::reasoning_effort_tier::ReasoningEffortTier;

/// Reasoning-effort tier, mirrored across DeepSeek and Codex effort pickers.
///
/// The config file accepts every supported string value for forward-compat with
/// providers that expose the full spectrum; first-party DeepSeek routes send
/// `low`/`high`/`max`, so `Minimal` → `low`, `Medium`/`XHigh` → `high`, and
/// `Ultra` → `max`. OpenAI Codex normalizes inherited DeepSeek-only
/// `Off` to `Low` and keeps `XHigh`, `Max`, and `Ultra` distinct at the
/// provider boundary. The default keyboard cycler walks the three DeepSeek-distinct
/// tiers: `Off` → `High` → `Max` → `Off`; provider-aware callers should use
/// [`ReasoningEffort::cycle_next_in`] with the route's picker ladder
/// (`picker_efforts_for_route`), which also covers Auto routing.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningEffort {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Ultra,
    Auto,
    #[default]
    Max,
}

/// Provider-effective reasoning state used by durable receipts and visible
/// requested-to-effective labels.
///
/// Some routes, notably first-party GLM-5-Turbo, support a thinking toggle but
/// publish no effort tiers. Keeping that state distinct prevents a requested
/// `max` from being displayed or persisted as an effective `max` claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectiveReasoningEffort {
    Tier(ReasoningEffort),
    ThinkingEnabledGranularityUnavailable,
    Unavailable,
}

impl EffectiveReasoningEffort {
    /// Reconstruct a safe request tier for cache replay and inspection.
    ///
    /// Routes with an enabled-but-untiered receipt collapse every non-Off
    /// request to the same wire toggle, so High is the canonical value that
    /// keeps reasoning enabled without claiming a granular effective tier.
    #[must_use]
    pub const fn request_tier_for_replay(self) -> Option<ReasoningEffort> {
        match self {
            Self::Tier(tier) => Some(tier),
            Self::ThinkingEnabledGranularityUnavailable => Some(ReasoningEffort::High),
            Self::Unavailable => None,
        }
    }
}

impl From<EffectiveReasoningEffort> for ReasoningEffortTier {
    fn from(value: EffectiveReasoningEffort) -> Self {
        match value {
            EffectiveReasoningEffort::Tier(tier) => tier.into(),
            EffectiveReasoningEffort::ThinkingEnabledGranularityUnavailable => {
                Self::ThinkingEnabledGranularityUnavailable
            }
            EffectiveReasoningEffort::Unavailable => Self::Unavailable,
        }
    }
}

impl From<ReasoningEffortTier> for EffectiveReasoningEffort {
    fn from(value: ReasoningEffortTier) -> Self {
        match value {
            ReasoningEffortTier::Off => Self::Tier(ReasoningEffort::Off),
            ReasoningEffortTier::Minimal => Self::Tier(ReasoningEffort::Minimal),
            ReasoningEffortTier::Low => Self::Tier(ReasoningEffort::Low),
            ReasoningEffortTier::Medium => Self::Tier(ReasoningEffort::Medium),
            ReasoningEffortTier::High => Self::Tier(ReasoningEffort::High),
            ReasoningEffortTier::XHigh => Self::Tier(ReasoningEffort::XHigh),
            ReasoningEffortTier::Ultra => Self::Tier(ReasoningEffort::Ultra),
            ReasoningEffortTier::Auto => Self::Tier(ReasoningEffort::Auto),
            ReasoningEffortTier::Max => Self::Tier(ReasoningEffort::Max),
            ReasoningEffortTier::ThinkingEnabledGranularityUnavailable => {
                Self::ThinkingEnabledGranularityUnavailable
            }
            ReasoningEffortTier::Unavailable => Self::Unavailable,
        }
    }
}

impl From<ReasoningEffort> for ReasoningEffortTier {
    fn from(value: ReasoningEffort) -> Self {
        match value {
            ReasoningEffort::Off => Self::Off,
            ReasoningEffort::Minimal => Self::Minimal,
            ReasoningEffort::Low => Self::Low,
            ReasoningEffort::Medium => Self::Medium,
            ReasoningEffort::High => Self::High,
            ReasoningEffort::XHigh => Self::XHigh,
            ReasoningEffort::Ultra => Self::Ultra,
            ReasoningEffort::Auto => Self::Auto,
            ReasoningEffort::Max => Self::Max,
        }
    }
}

impl ReasoningEffort {
    /// Parse an operator-supplied effort value.
    ///
    /// This is deliberately the one canonical spelling table for every
    /// human-facing route. Every canonical setting spelling round-trips:
    /// `parse_strict(as_setting(effort)) == effort`, including `minimal`.
    /// Callers that read an old persisted config may use [`Self::from_setting`]
    /// for its compatibility fallback, but a new CLI, settings, or tool input
    /// must reject an unknown value instead of quietly turning it into `max`.
    pub fn parse_strict(value: &str) -> Result<Self, String> {
        let canonical = codewhale_config::settings_schema::canonical_reasoning_effort(value)?;
        Ok([
            Self::Off,
            Self::Minimal,
            Self::Low,
            Self::Medium,
            Self::High,
            Self::XHigh,
            Self::Ultra,
            Self::Auto,
            Self::Max,
        ]
        .into_iter()
        .find(|effort| effort.as_setting() == canonical)
        .unwrap_or_default())
    }

    /// Parse a persisted config-file string into an effort tier. Unknown
    /// legacy values fall back to the default (`Max`) so an old malformed
    /// settings file never prevents startup. New user input should use
    /// [`Self::parse_strict`] instead.
    #[must_use]
    pub fn from_setting(value: &str) -> Self {
        Self::parse_strict(value).unwrap_or_default()
    }

    #[must_use]
    pub fn from_setting_for_provider(value: &str, provider: ProviderKind) -> Self {
        Self::from_setting(value).normalize_for_provider(provider)
    }

    /// Canonical lowercase label used for config storage and UI hints.
    #[must_use]
    pub fn as_setting(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Ultra => "ultra",
            Self::Auto => "auto",
            Self::Max => "max",
        }
    }

    /// Short label for the header chip.
    #[must_use]
    pub fn short_label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "med",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Ultra => "ultra",
            Self::Auto => "auto",
            Self::Max => "max",
        }
    }

    /// Provider-facing label for user-visible surfaces.
    #[must_use]
    pub fn display_label_for_provider(self, provider: ProviderKind) -> &'static str {
        match (provider, self.normalize_for_provider(provider)) {
            (ProviderKind::OpenaiCodex, Self::Minimal) => "low",
            (ProviderKind::OpenaiCodex, Self::Low) => "low",
            (ProviderKind::OpenaiCodex, Self::Medium) => "medium",
            (ProviderKind::OpenaiCodex, Self::High) => "high",
            // `xhigh`, `max` and `ultra` are three distinct rungs the Codex
            // roster publishes per model; collapsing them onto "xhigh" dates
            // from when xhigh was the ceiling and made the top tiers
            // unreachable and indistinguishable.
            (ProviderKind::OpenaiCodex, Self::XHigh) => "xhigh",
            (ProviderKind::OpenaiCodex, Self::Ultra) => "ultra",
            (ProviderKind::OpenaiCodex, Self::Max) => "max",
            (ProviderKind::Xai, Self::XHigh) => "xhigh",
            (_, effort) => effort.short_label(),
        }
    }

    /// Value forwarded to the engine/client. `None` means "provider default"
    /// (for `Off` we still emit `"off"` so the client can inject
    /// `thinking = {"type": "disabled"}`).
    #[must_use]
    pub fn api_value(self) -> Option<&'static str> {
        Some(self.as_setting())
    }

    #[must_use]
    pub fn normalize_for_provider(self, provider: ProviderKind) -> Self {
        if provider != ProviderKind::OpenaiCodex {
            return self;
        }
        match self {
            Self::Off => Self::Low,
            Self::Auto => Self::Medium,
            other => other,
        }
    }

    /// Resolve an effort against the exact provider route that will receive
    /// the request. Both K3 routes are always-thinking, so `off` becomes the
    /// lowest supported tier. The Kimi Code membership route otherwise keeps
    /// its low/high/max mapping; direct Moonshot K3 additionally maps `medium`
    /// to `high`. First-party DeepSeek routes keep `low` (the wire documents
    /// low/high/max) while rounding `medium` up to `high`. Models that publish
    /// a Models.dev `reasoning_options` effort list keep that vocabulary
    /// instead of the historic Low/Medium collapse. Generic Moonshot and
    /// every other non-Codex route retain the historic high coercion.
    /// This intentionally does not change [`Self::normalize_for_provider`],
    /// whose generic wire semantics are used by older callers that do not yet
    /// have a route receipt.
    #[must_use]
    pub fn normalize_for_route(
        self,
        provider: ProviderKind,
        base_url: &str,
        wire_model: &str,
    ) -> Self {
        self.normalize_for_route_identity(provider, provider.as_str(), base_url, wire_model)
    }

    #[must_use]
    pub fn normalize_for_route_identity(
        self,
        provider: ProviderKind,
        identity: &str,
        base_url: &str,
        wire_model: &str,
    ) -> Self {
        let normalized = self.normalize_for_provider(provider);
        if codewhale_runtime::config::is_exact_kimi_code_k3_route(provider, base_url, wire_model) {
            return match normalized {
                Self::Off => Self::Low,
                other => other,
            };
        }
        if codewhale_runtime::config::is_exact_direct_moonshot_k3_route(
            provider, base_url, wire_model,
        ) {
            return match normalized {
                Self::Off => Self::Low,
                Self::Medium => Self::High,
                other => other,
            };
        }
        if provider == ProviderKind::OpenaiCodex {
            return normalized;
        }
        // First-party DeepSeek routes document `reasoning_effort` low/high/max
        // on the wire (no medium), so `low` is a real, cheaper tier there and
        // must reach the wire as low; `medium` rounds up to high because the
        // dialect has no such value (#52). `minimal`, `xhigh`, and `ultra`
        // collapse exactly as `client::deepseek_effort` sends them, so the
        // effective tier names what the wire receives (#6650).
        if matches!(provider, ProviderKind::Deepseek) {
            return match normalized {
                Self::Minimal => Self::Low,
                Self::Medium | Self::XHigh => Self::High,
                Self::Ultra => Self::Max,
                other => other,
            };
        }
        // Ollama's current OpenAI-compatible Chat Completions contract
        // documents the complete none/low/medium/high/max ladder. Keep every
        // real tier distinct for normal turns; only Codewhale-only synonyms
        // are folded onto the nearest documented spelling.
        if provider == ProviderKind::OllamaCloud {
            return match normalized {
                Self::Minimal => Self::Low,
                Self::XHigh | Self::Ultra => Self::Max,
                other => other,
            };
        }
        let catalog_identity = if provider == ProviderKind::Custom {
            identity
        } else {
            provider.as_str()
        };
        if let Some(offering) = crate::provider_lake::catalog_offering_for_route(
            provider,
            catalog_identity,
            base_url,
            wire_model,
        ) && let Some(values) = Self::catalog_effort_values_for_offering(&offering)
        {
            if normalized == Self::Off
                && provider != ProviderKind::Custom
                && !codewhale_config::provider_base_url_is_official(provider, base_url)
            {
                return Self::Off;
            }
            return Self::clamp_to_catalog_efforts(
                normalized,
                Self::catalog_default_for_offering(&offering),
                &values,
            );
        }
        match normalized {
            Self::Low | Self::Medium => Self::High,
            other => other,
        }
    }

    pub(crate) fn offering_for_reasoning(
        provider: ProviderKind,
        base_url: &str,
        wire_model: &str,
    ) -> Option<codewhale_config::catalog::CatalogOffering> {
        crate::provider_lake::catalog_offering_for_route(
            provider,
            provider.as_str(),
            base_url,
            wire_model,
        )
    }

    pub fn catalog_default(
        provider: ProviderKind,
        base_url: &str,
        wire_model: &str,
    ) -> Option<Self> {
        let offering = Self::offering_for_reasoning(provider, base_url, wire_model)?;
        Self::catalog_default_for_offering(&offering)
    }

    fn catalog_default_for_offering(
        offering: &codewhale_config::catalog::CatalogOffering,
    ) -> Option<Self> {
        offering.reasoning_options.iter().find_map(|option| {
            option
                .get("type")
                .and_then(|value| value.as_str())
                .filter(|kind| kind.eq_ignore_ascii_case("effort"))?;
            option
                .get("default")
                .and_then(|value| value.as_str())
                .and_then(Self::from_catalog_token)
        })
    }

    pub fn catalog_effort_values(
        provider: ProviderKind,
        base_url: &str,
        wire_model: &str,
    ) -> Option<Vec<Self>> {
        let offering = Self::offering_for_reasoning(provider, base_url, wire_model)?;
        Self::catalog_effort_values_for_offering(&offering)
    }

    pub(crate) fn catalog_effort_values_for_offering(
        offering: &codewhale_config::catalog::CatalogOffering,
    ) -> Option<Vec<Self>> {
        let mut efforts = Vec::new();
        let mut has_toggle = false;
        for option in &offering.reasoning_options {
            let kind = option.get("type").and_then(|value| value.as_str());
            if kind.is_some_and(|kind| kind.eq_ignore_ascii_case("toggle")) {
                has_toggle = true;
                continue;
            }
            if !kind.is_some_and(|kind| kind.eq_ignore_ascii_case("effort")) {
                continue;
            }
            let Some(values) = option.get("values").and_then(|value| value.as_array()) else {
                continue;
            };
            for value in values {
                if let Some(effort) = value.as_str().and_then(Self::from_catalog_token)
                    && !efforts.contains(&effort)
                {
                    efforts.push(effort);
                }
            }
        }
        // A Models.dev `toggle` beside an effort ladder means reasoning can also
        // be switched off (#6396); an explicit off must not round up.
        if has_toggle && !efforts.is_empty() && !efforts.contains(&Self::Off) {
            efforts.insert(0, Self::Off);
        }
        (!efforts.is_empty()).then_some(efforts)
    }

    pub fn from_catalog_token(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "off" | "disabled" | "none" | "false" => Some(Self::Off),
            "minimal" | "minimum" => Some(Self::Minimal),
            "low" | "light" => Some(Self::Low),
            "medium" | "mid" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" => Some(Self::XHigh),
            "ultra" | "ultracode" => Some(Self::Ultra),
            "max" | "maximum" => Some(Self::Max),
            "auto" | "automatic" | "adaptive" | "default" => Some(Self::Auto),
            _ => None,
        }
    }

    fn clamp_to_catalog_efforts(normalized: Self, default: Option<Self>, values: &[Self]) -> Self {
        if matches!(normalized, Self::Auto) || values.contains(&normalized) {
            return normalized;
        }
        let aliased = match normalized {
            Self::Minimal if values.contains(&Self::Low) => Self::Low,
            Self::Max | Self::Ultra if values.contains(&Self::XHigh) => Self::XHigh,
            Self::Off => default.unwrap_or(Self::High),
            other => other,
        };
        if values.contains(&aliased) {
            return aliased;
        }
        if let Some(default) = default {
            return default;
        }
        if values.contains(&Self::High) {
            return Self::High;
        }
        normalized.nearest_in(values).unwrap_or(Self::High)
    }

    #[must_use]
    pub fn api_value_for_provider(self, provider: ProviderKind) -> Option<&'static str> {
        if provider != ProviderKind::OpenaiCodex {
            return self.api_value();
        }
        Some(match self.normalize_for_provider(provider) {
            Self::Minimal => "low",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Ultra => "ultra",
            Self::Max => "max",
            Self::Off => "low",
            Self::Auto => "medium",
        })
    }

    /// Provider-facing value after exact-route normalization.
    #[must_use]
    pub fn api_value_for_route(
        self,
        provider: ProviderKind,
        base_url: &str,
        wire_model: &str,
    ) -> Option<&'static str> {
        self.normalize_for_route(provider, base_url, wire_model)
            .api_value_for_provider(provider)
    }

    #[must_use]
    pub fn as_setting_for_provider(self, provider: ProviderKind) -> &'static str {
        self.api_value_for_provider(provider)
            .unwrap_or_else(|| self.as_setting())
    }

    /// Persist the canonical setting after exact-route normalization.
    #[must_use]
    pub fn as_setting_for_route(
        self,
        provider: ProviderKind,
        base_url: &str,
        wire_model: &str,
    ) -> &'static str {
        self.normalize_for_route(provider, base_url, wire_model)
            .as_setting_for_provider(provider)
    }

    /// Cycle through the three behaviorally distinct tiers.
    #[must_use]
    pub fn cycle_next(self) -> Self {
        match self {
            Self::Off => Self::High,
            Self::Auto => Self::Off,
            Self::Minimal | Self::Low | Self::Medium | Self::High | Self::XHigh | Self::Ultra => {
                Self::Max
            }
            Self::Max => Self::Off,
        }
    }

    /// Advance through an exact-route effort list.
    ///
    /// A value that is literally on the ladder advances one rung and wraps.
    /// An off-ladder value enters at [`Self::nearest_in`]'s rung and advances
    /// from there, so the cycler only ever moves forward: a persisted `max` on
    /// an `xhigh` ladder, `ultra` on a `max` ladder, or `minimal` on a ladder
    /// starting at `off` still walks upward instead of reversing.
    #[must_use]
    pub fn cycle_next_in(self, efforts: &[Self]) -> Self {
        if efforts.is_empty() {
            return self.cycle_next();
        }
        if let Some(index) = self.index_in(efforts) {
            return efforts[(index + 1) % efforts.len()];
        }
        let Some(entry) = self.nearest_in(efforts) else {
            return self.cycle_next();
        };
        let index = entry.index_in(efforts).unwrap_or(0);
        efforts[(index + 1) % efforts.len()]
    }

    /// Ladder rank of a concrete tier.
    ///
    /// `Auto` is the unresolved sentinel: it names no rung, so it has no rank
    /// and is never a projection target.
    const fn rung_rank(self) -> Option<u8> {
        Some(match self {
            Self::Off => 0,
            Self::Minimal => 1,
            Self::Low => 2,
            Self::Medium => 3,
            Self::High => 4,
            Self::XHigh => 5,
            Self::Ultra => 6,
            Self::Max => 7,
            Self::Auto => return None,
        })
    }

    /// Project an effort onto the rungs a route actually offers.
    ///
    /// One deterministic rule replaces the hand-written alias buckets: the
    /// highest allowed rung at or below the current one, or the lowest allowed
    /// rung when the current value sits below every rung or names no rung at
    /// all (`Auto`). `None` only when the ladder publishes no rung.
    #[must_use]
    pub fn nearest_in(self, efforts: &[Self]) -> Option<Self> {
        let own_rank = self.rung_rank();
        let mut nearest_below: Option<Self> = None;
        let mut lowest: Option<Self> = None;
        for effort in efforts.iter().copied() {
            let Some(rank) = effort.rung_rank() else {
                continue;
            };
            if lowest.is_none_or(|current: Self| rank < current.rung_rank().unwrap_or(u8::MAX)) {
                lowest = Some(effort);
            }
            if own_rank.is_some_and(|own| rank <= own)
                && nearest_below.is_none_or(|current: Self| rank > current.rung_rank().unwrap_or(0))
            {
                nearest_below = Some(effort);
            }
        }
        nearest_below.or(lowest)
    }

    /// Position of a literally listed value on the ladder. Values that are not
    /// listed are handled by [`Self::nearest_in`], never by an alias table.
    fn index_in(self, efforts: &[Self]) -> Option<usize> {
        efforts.iter().position(|&effort| effort == self)
    }
}

#[cfg(test)]
mod tests {
    use super::ReasoningEffort;

    const CANONICAL: [ReasoningEffort; 9] = [
        ReasoningEffort::Auto,
        ReasoningEffort::Off,
        ReasoningEffort::Minimal,
        ReasoningEffort::Low,
        ReasoningEffort::Medium,
        ReasoningEffort::High,
        ReasoningEffort::XHigh,
        ReasoningEffort::Ultra,
        ReasoningEffort::Max,
    ];

    #[test]
    fn reasoning_effort_parse_strict_round_trips_every_canonical_spelling() {
        for effort in CANONICAL {
            assert_eq!(
                ReasoningEffort::parse_strict(effort.as_setting()),
                Ok(effort),
                "{} must parse back to itself",
                effort.as_setting()
            );
            assert_eq!(
                ReasoningEffort::from_setting(effort.as_setting()),
                effort,
                "a persisted {} must load as itself",
                effort.as_setting()
            );
        }
        assert_eq!(
            ReasoningEffort::parse_strict("minimal"),
            Ok(ReasoningEffort::Minimal)
        );
        assert_eq!(
            ReasoningEffort::parse_strict("minimum"),
            Ok(ReasoningEffort::Low)
        );
        assert_eq!(
            ReasoningEffort::parse_strict("light"),
            Ok(ReasoningEffort::Low)
        );
    }

    #[test]
    fn reasoning_effort_parse_strict_error_lists_the_complete_vocabulary() {
        let error = ReasoningEffort::parse_strict("xhihg").expect_err("unknown value must fail");
        for spelling in [
            "auto", "off", "minimal", "low", "medium", "high", "xhigh", "ultra", "max",
        ] {
            assert!(
                error.contains(spelling),
                "the error must name {spelling}, got {error:?}"
            );
        }
    }

    #[test]
    fn reasoning_effort_nearest_in_projects_one_value_per_direction() {
        use ReasoningEffort::*;
        let full = [Off, Minimal, Low, Medium, High, XHigh, Ultra, Max];
        // Literal members project onto themselves.
        for effort in full {
            assert_eq!(effort.nearest_in(&full), Some(effort));
        }
        // A capped ladder: the highest rung at or below wins, never a rung above.
        let capped = [Off, Low, Medium, High, XHigh];
        assert_eq!(Max.nearest_in(&capped), Some(XHigh));
        assert_eq!(Ultra.nearest_in(&capped), Some(XHigh));
        assert_eq!(Minimal.nearest_in(&capped), Some(Off));
        // Below every rung: the lowest allowed rung is the entry point.
        assert_eq!(Off.nearest_in(&[Low, Medium, High, Max]), Some(Low));
        // `Auto` names no rung, so it enters at the lowest allowed rung.
        assert_eq!(Auto.nearest_in(&[Low, Medium, High, Max]), Some(Low));
        assert_eq!(Auto.nearest_in(&[Off, Low, High, Max]), Some(Off));
        // A ladder that publishes no rung has nothing to project onto.
        assert_eq!(Medium.nearest_in(&[Auto]), None);
        assert_eq!(Medium.nearest_in(&[]), None);
    }

    #[test]
    fn reasoning_effort_cycle_next_in_only_moves_forward_on_any_ladder() {
        use ReasoningEffort::*;
        // Off-ladder values enter at their nearest rung and advance one rung.
        let capped = [Off, Low, Medium, High, XHigh];
        assert_eq!(
            Max.cycle_next_in(&capped),
            Off,
            "xhigh-capped ladder wraps forward"
        );
        assert_eq!(Ultra.cycle_next_in(&capped), Off);
        assert_eq!(Minimal.cycle_next_in(&capped), Low);
        // A value below every rung enters at the lowest rung.
        assert_eq!(Off.cycle_next_in(&[Low, Medium, High, Max]), Medium);
        // `Auto` enters at the ladder's lowest rung.
        assert_eq!(Auto.cycle_next_in(&[Off, Low, High, Max]), Low);
        // Literal members advance and wrap without an alias table.
        assert_eq!(High.cycle_next_in(&capped), XHigh);
        assert_eq!(XHigh.cycle_next_in(&capped), Off);
        // An empty ladder falls back to the provider-neutral cycle.
        assert_eq!(Off.cycle_next_in(&[]), Off.cycle_next());
    }
}
