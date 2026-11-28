use crate::tui::pager::PagerView;
use codewhale_config::model_reference::ModelReferenceDatabase;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;

pub(super) fn view() -> PagerView {
    let db = ModelReferenceDatabase::bundled();
    let title = format!(
        "Model Reference — {} offerings · {} providers",
        db.len(),
        db.providers().len()
    );
    PagerView::new(title, reference_lines(&db))
}

/// Render the reference database as aligned, browsable pager lines.
///
/// Cards are grouped under a provider/kind header (the database is already
/// sorted by `(provider, model id)`), so each row only needs the model-scoped
/// columns. Column widths are computed across the whole table for stable
/// alignment.
fn reference_lines(db: &ModelReferenceDatabase) -> Vec<Line<'static>> {
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let dim = Style::default().add_modifier(Modifier::DIM);

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::styled(
        "Bundled curated model reference catalog.".to_string(),
        bold,
    ));
    lines.push(Line::styled(
        "Attributes are stated facts; \"unknown\" means the catalog did not state it (never guessed)."
            .to_string(),
        dim,
    ));
    lines.push(Line::from(String::new()));

    if db.is_empty() {
        lines.push(Line::from("(no models in catalog)".to_string()));
        return lines;
    }

    let cards = db.cards();
    let id_w = cards
        .iter()
        .map(|card| card.model_id.chars().count())
        .chain(std::iter::once("MODEL ID".len()))
        .max()
        .unwrap_or(8)
        .clamp(8, 46);
    let ctx_w = cards
        .iter()
        .map(|card| card.context_window_label().chars().count())
        .chain(std::iter::once("CTX".len()))
        .max()
        .unwrap_or(3);
    let out_w = cards
        .iter()
        .map(|card| card.max_output_label().chars().count())
        .chain(std::iter::once("MAX OUT".len()))
        .max()
        .unwrap_or(7);
    // "multimodal" (10) is the widest possible label and exceeds "MODALITY".
    let mod_w = "multimodal".len();

    lines.push(Line::styled(
        format!(
            "  {}  {}  {}  {}  {}",
            pad("MODEL ID", id_w),
            pad("CTX", ctx_w),
            pad("MAX OUT", out_w),
            pad("MODALITY", mod_w),
            "PRICE (USD/Mtok)"
        ),
        bold,
    ));

    let mut current_provider: Option<&str> = None;
    for card in cards {
        if current_provider != Some(card.provider.as_str()) {
            lines.push(Line::from(String::new()));
            lines.push(Line::styled(
                format!(
                    "{}   ·   kind: {}",
                    card.provider,
                    card.provider_kind_label()
                ),
                bold,
            ));
            current_provider = Some(card.provider.as_str());
        }
        lines.push(Line::from(format!(
            "  {}  {}  {}  {}  {}",
            pad(&truncate_to(&card.model_id, id_w), id_w),
            pad(&card.context_window_label(), ctx_w),
            pad(&card.max_output_label(), out_w),
            pad(card.modality.as_str(), mod_w),
            card.price_label(),
        )));
    }

    lines
}

/// Left-justify `s` to `width` display columns (counted by `char`).
fn pad(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(width - len))
    }
}

/// Truncate `s` to at most `width` chars, marking elision with `…`.
fn truncate_to(s: &str, width: usize) -> String {
    let count = s.chars().count();
    if count <= width {
        return s.to_string();
    }
    if width <= 1 {
        return s.chars().take(width).collect();
    }
    let mut truncated: String = s.chars().take(width - 1).collect();
    truncated.push('…');
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(db: &ModelReferenceDatabase) -> Vec<String> {
        reference_lines(db)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn bundled_reference_lists_models_with_factual_columns() {
        let db = ModelReferenceDatabase::bundled();
        let text = rendered(&db).join("\n");

        // Legend states the honesty contract.
        assert!(text.contains("never guessed"));
        // Column key present.
        assert!(text.contains("MODEL ID"));
        assert!(text.contains("MODALITY"));
        assert!(text.contains("PRICE (USD/Mtok)"));
        // A provider header and a verbatim model id row.
        assert!(text.contains("kind: deepseek"));
        assert!(text.contains("deepseek-v4-pro"));
        // Stated modality and an honest unknown price both appear.
        assert!(text.contains("text"));
        assert!(text.contains("unknown"));
        // A priced row surfaces a concrete rate.
        assert!(text.contains("$0.30 / $1.20 per Mtok"));
    }

    #[test]
    fn empty_database_renders_placeholder_not_a_crash() {
        let db = ModelReferenceDatabase::from_offerings(&[]);
        let text = rendered(&db).join("\n");
        assert!(text.contains("(no models in catalog)"));
    }

    #[test]
    fn pad_and_truncate_are_width_safe() {
        assert_eq!(pad("ab", 5), "ab   ");
        assert_eq!(pad("abcde", 3), "abcde");
        assert_eq!(truncate_to("short", 10), "short");
        assert_eq!(truncate_to("abcdefghij", 5), "abcd…");
    }
}
