//! Small string builders that compose status-bar / footer chips and
//! one-off informational messages.
//!
//! Each helper is a pure function over a small slice of `App` or
//! response data. Grouped here so the composer/footer renderer doesn't
//! need to scroll past their bodies, and so the labels can be unit
//! tested in isolation.

use codewhale_models::Usage;

/// Build the multi-line "Cache warmup complete: …" status message
/// shown after a prefix-cache warmup turn finishes. Handles all four
/// combinations of `prompt_cache_hit_tokens` / `prompt_cache_miss_tokens`
/// being present or absent so we never report "0% cache hit" for an
/// API call that didn't surface telemetry at all.
pub(super) fn cache_warmup_result(usage: &Usage) -> String {
    let cache = match (
        usage.prompt_cache_hit_tokens,
        usage.prompt_cache_miss_tokens,
    ) {
        (Some(hit), Some(miss)) => format!("Cache warmup complete: hit {hit} | miss {miss}"),
        (Some(hit), None) => format!("Cache warmup complete: hit {hit} | miss unavailable"),
        (None, Some(miss)) => format!("Cache warmup complete: hit unavailable | miss {miss}"),
        (None, None) => "Cache warmup complete: cache telemetry unavailable".to_string(),
    };
    format!(
        "{cache}\nNote: the first warmup is usually a miss. Later requests that reuse the same stable prefix may hit the provider cache; a hit is not guaranteed."
    )
}

/// Render the response body for `/models` / `models list` — the current
/// model is starred and other available models follow underneath.
pub(super) fn available_models_message(
    locale: codewhale_localization::Locale,
    current_provider: &str,
    current_model: &str,
    models: &[String],
    fleet: &Result<
        Vec<codewhale_engine::fleet::members::FleetModel>,
        codewhale_engine::fleet::store::FleetStoreError,
    >,
) -> String {
    use codewhale_localization::{MessageId, tr};
    let mut lines = Vec::new();
    // The fleet leads (design §10 F1): what the person added, with the roles
    // each model fills, before the provider's full list. A selected fleet
    // that cannot be read is named as such, never shown as "no fleet".
    match fleet.as_deref() {
        Err(error) => lines
            .push(tr(locale, MessageId::FleetModelsBroken).replace("{error}", &error.to_string())),
        Ok([]) => lines.push(tr(locale, MessageId::FleetModelsEmpty).into_owned()),
        Ok(fleet) => {
            lines.push(
                tr(locale, MessageId::FleetModelsHeader)
                    .replace("{fleet}", &fleet[0].fleet)
                    .replace("{count}", &fleet.len().to_string()),
            );
            for member in fleet {
                // The exact route, not the bare id: two providers may serve
                // the same model id and only one of them is the current route.
                let marker = if member.matches(current_provider, current_model) {
                    "*"
                } else {
                    " "
                };
                lines.push(format!(
                    "{marker} {}/{} · {}",
                    member.provider,
                    member.model,
                    member.roles_label()
                ));
            }
            lines.push(String::new());
        }
    }
    lines.push(format!("Available models ({})", models.len()));
    for model in models {
        if model == current_model {
            lines.push(format!("* {model} (current)"));
        } else {
            lines.push(format!("  {model}"));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn available_models_message_marks_current_model() {
        let models = vec![
            "deepseek-v4-pro".to_string(),
            "deepseek-v4-flash".to_string(),
        ];
        let msg = available_models_message(
            codewhale_localization::Locale::En,
            "deepseek",
            "deepseek-v4-pro",
            &models,
            &Ok(Vec::new()),
        );
        assert!(msg.contains("* deepseek-v4-pro (current)"), "got: {msg}");
        assert!(msg.contains("  deepseek-v4-flash"), "got: {msg}");
        assert!(
            msg.starts_with("Your team is the session model only"),
            "got: {msg}"
        );
        assert!(msg.contains("Available models (2)"), "got: {msg}");
    }

    /// A selected fleet that cannot be read is reported as an error, not as
    /// "the session model only".
    #[test]
    fn available_models_message_names_a_broken_fleet_selection() {
        let broken = Err(codewhale_engine::fleet::store::FleetStoreError::NotFound(
            "selected fleet `Ops`".to_string(),
        ));
        let msg = available_models_message(
            codewhale_localization::Locale::En,
            "deepseek",
            "deepseek-v4-pro",
            &[],
            &broken,
        );
        assert!(
            msg.starts_with(
                "Your selected team could not be loaded: fleet file not found: selected fleet `Ops`"
            ),
            "got: {msg}"
        );
        assert!(!msg.contains("session model only"), "got: {msg}");
    }

    #[test]
    fn fleet_current_marker_matches_the_exact_route_not_the_bare_id() {
        let fleet = vec![
            codewhale_engine::fleet::members::FleetModel {
                provider: "openrouter".to_string(),
                model: "deepseek/deepseek-v4-flash".to_string(),
                roles: vec!["scout".to_string()],
                fleet: "Ops".to_string(),
            },
            codewhale_engine::fleet::members::FleetModel {
                provider: "novita".to_string(),
                model: "deepseek/deepseek-v4-flash".to_string(),
                roles: Vec::new(),
                fleet: "Ops".to_string(),
            },
        ];
        let msg = available_models_message(
            codewhale_localization::Locale::En,
            "novita",
            "deepseek/deepseek-v4-flash",
            &[],
            &Ok(fleet),
        );
        assert!(
            msg.contains("  openrouter/deepseek/deepseek-v4-flash · explore"),
            "got: {msg}"
        );
        assert!(
            msg.contains("* novita/deepseek/deepseek-v4-flash · member"),
            "got: {msg}"
        );
    }

    #[test]
    fn cache_warmup_result_handles_missing_telemetry() {
        let usage = Usage {
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
            ..Default::default()
        };
        let msg = cache_warmup_result(&usage);
        assert!(msg.contains("cache telemetry unavailable"), "got: {msg}");
    }
}
