use crate::provider_kind::ProviderKind;

/// Ordered primary-plus-fallback provider list for future provider routing.
///
/// The helper is intentionally dormant: constructing or parsing a chain does
/// not change `ConfigToml::resolve_runtime_options`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderChain {
    providers: Vec<ProviderKind>,
    position: usize,
}

impl ProviderChain {
    #[must_use]
    pub fn new(active: ProviderKind, fallbacks: &[ProviderKind]) -> Self {
        let mut providers = vec![active];
        for fallback in fallbacks {
            if *fallback != active && !providers.contains(fallback) {
                providers.push(*fallback);
            }
        }
        Self {
            providers,
            position: 0,
        }
    }

    #[must_use]
    pub fn providers(&self) -> &[ProviderKind] {
        &self.providers
    }

    #[must_use]
    pub fn position(&self) -> usize {
        self.position
    }

    #[must_use]
    pub fn current(&self) -> ProviderKind {
        self.providers
            .get(self.position)
            .copied()
            .or_else(|| self.providers.first().copied())
            .unwrap_or_default()
    }

    #[must_use]
    pub fn has_next(&self) -> bool {
        self.position + 1 < self.providers.len()
    }

    pub fn advance(&mut self) -> Option<ProviderKind> {
        if !self.has_next() {
            return None;
        }
        self.position += 1;
        Some(self.current())
    }

    pub fn reset(&mut self) {
        self.position = 0;
    }

    #[must_use]
    pub fn is_fallback_active(&self) -> bool {
        self.position > 0
    }

    /// Count the current provider plus untried chain entries.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.providers.len() - self.position
    }
}

#[cfg(test)]
mod provider_chain_tests {
    use super::*;

    #[test]
    fn current_on_empty_chain_returns_default_provider() {
        let chain = ProviderChain {
            providers: vec![],
            position: 0,
        };
        assert_eq!(chain.current(), ProviderKind::default());
    }
}
