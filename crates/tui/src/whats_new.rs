use codewhale_commands::debug::change::{
    count_changelog_entries, extract_changelog_section_by_version,
};
use codewhale_localization::{Locale, MessageId, tr};

pub(crate) const BUNDLED_CHANGELOG: &str = include_str!("../CHANGELOG.md");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhatsNewNotice {
    pub version: String,
    pub updated: String,
    pub counts: Option<String>,
    /// The summary paragraph of every release from the one this copy last
    /// ran (exclusive) up to this one, newest first.
    pub summary: Option<String>,
    pub notes: String,
    pub url: String,
    headline_template: String,
}

impl WhatsNewNotice {
    #[must_use]
    pub fn for_version(locale: Locale, version: &str) -> Self {
        Self::for_change(locale, None, version)
    }

    /// The notice for moving from `previous` to `version`, rolling up the
    /// summaries of any releases skipped in between.
    #[must_use]
    pub fn for_change(locale: Locale, previous: Option<&str>, version: &str) -> Self {
        let summary = rolled_up_summary(BUNDLED_CHANGELOG, previous, version);
        let section = extract_changelog_section_by_version(BUNDLED_CHANGELOG, version);
        let counts = section
            .as_deref()
            .map(count_changelog_entries)
            .filter(|counts| counts.features + counts.fixes + counts.other > 0)
            .map(|counts| {
                tr(locale, MessageId::UpdateWhatsNewCounts)
                    .replace(
                        "{features}",
                        &count_phrase(
                            locale,
                            counts.features,
                            MessageId::UpdateWhatsNewFeatureSingular,
                            MessageId::UpdateWhatsNewFeaturesPlural,
                        ),
                    )
                    .replace(
                        "{fixes}",
                        &count_phrase(
                            locale,
                            counts.fixes,
                            MessageId::UpdateWhatsNewFixSingular,
                            MessageId::UpdateWhatsNewFixesPlural,
                        ),
                    )
                    .replace(
                        "{other}",
                        &count_phrase(
                            locale,
                            counts.other,
                            MessageId::UpdateWhatsNewOtherSingular,
                            MessageId::UpdateWhatsNewOtherPlural,
                        ),
                    )
            });
        let url = release_notes_url(locale, section.is_some().then_some(version));
        Self {
            version: version.to_string(),
            updated: tr(locale, MessageId::UpdateWhatsNewUpdated).replace("{version}", version),
            counts,
            summary,
            notes: tr(locale, MessageId::UpdateWhatsNewNotes).replace("{url}", &url),
            url,
            headline_template: tr(locale, MessageId::UpdateWhatsNewHeadline).into_owned(),
        }
    }

    #[must_use]
    pub fn headline(&self) -> String {
        if let Some(summary) = &self.summary {
            return format!("{} {summary}", self.updated);
        }
        match &self.counts {
            Some(counts) => self
                .headline_template
                .replace("{updated}", &self.updated)
                .replace("{counts}", counts),
            None => self.updated.clone(),
        }
    }

    #[must_use]
    pub fn notes_with_command(&self) -> String {
        format!("{} · /change {}", self.notes, self.version)
    }

    #[must_use]
    pub fn plain_text(&self) -> String {
        format!("{}\n{}", self.headline(), self.notes)
    }
}

fn version_key(version: &str) -> Option<(u64, u64, u64)> {
    let core = version
        .trim()
        .trim_start_matches('v')
        .split(['-', '+'])
        .next()?;
    let mut parts = core.split('.').map(|part| part.parse::<u64>().ok());
    Some((
        parts.next()??,
        parts.next()??,
        parts.next().flatten().unwrap_or(0),
    ))
}

/// The summary paragraph (the prose before the first `###`) of each
/// released section after `previous` up to `current`, newest first.
fn rolled_up_summary(changelog: &str, previous: Option<&str>, current: &str) -> Option<String> {
    let current_key = version_key(current)?;
    let floor = previous.and_then(version_key);
    let summaries: Vec<String> = changelog
        .lines()
        .filter_map(|line| line.strip_prefix("## [")?.split(']').next())
        .filter(|version| {
            version_key(version)
                .is_some_and(|key| key <= current_key && floor.is_none_or(|floor| key > floor))
        })
        .filter(|version| floor.is_some() || version_key(version) == Some(current_key))
        .filter_map(|version| {
            let section = extract_changelog_section_by_version(changelog, version)?;
            let paragraph = section
                .lines()
                .skip_while(|line| line.starts_with("## ") || line.trim().is_empty())
                .take_while(|line| !line.starts_with("### ") && !line.trim().is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            (!paragraph.trim().is_empty()).then(|| paragraph.trim().to_string())
        })
        .collect();
    (!summaries.is_empty()).then(|| summaries.join("\n"))
}

fn count_phrase(locale: Locale, count: usize, one: MessageId, many: MessageId) -> String {
    if count == 1 {
        tr(locale, one).into_owned()
    } else {
        tr(locale, many).replace("{count}", &count.to_string())
    }
}

#[must_use]
pub fn release_notes_url(locale: Locale, version: Option<&str>) -> String {
    let site_locale =
        codewhale_command_contract::navigation::public_site_locale_segment(locale.tag());
    match version {
        Some(version) => format!("https://codewhale.net/{site_locale}/changelog/{version}"),
        None => format!("https://codewhale.net/{site_locale}/changelog"),
    }
}

pub(crate) fn take_after_update(locale: Locale, show: bool) -> Option<WhatsNewNotice> {
    let home = codewhale_config::codewhale_home().ok()?;
    let outcome = codewhale_release::record_launch(&home, env!("CARGO_PKG_VERSION"));
    if let Some(record_error) = outcome.record_error {
        tracing::debug!(error = %record_error, "could not persist the last-launch record");
    }
    let change = outcome.change.filter(|_| show)?;
    Some(WhatsNewNotice::for_change(
        locale,
        Some(&change.previous),
        &change.current,
    ))
}

#[must_use]
pub fn plain_cli_notice(update_table: Option<&toml::Value>) -> Option<String> {
    let show = update_table
        .cloned()
        .and_then(|table| {
            table
                .try_into::<codewhale_runtime::config::UpdateConfig>()
                .ok()
        })
        .unwrap_or_default()
        .show_whats_new;
    let settings = codewhale_runtime::settings::Settings::load_read_only().unwrap_or_default();
    let locale = codewhale_localization::resolve_locale(&settings.locale);
    take_after_update(locale, show).map(|notice| notice.plain_text())
}

#[cfg(test)]
mod tests {
    use super::rolled_up_summary;

    const CHANGELOG: &str = "## [Unreleased]\n\n## [0.10.5] - 2026-12-01\n\nFive is here.\n\n### Fixed\n\n- A.\n\n## [0.10.4] - 2026-11-20\n\nFour is here.\n\n### Fixed\n\n- B.\n\n## [0.10.3] - 2026-11-01\n\nThree is here.\n";

    #[test]
    fn whats_new_rolls_up_skipped_release_summaries() {
        assert_eq!(
            rolled_up_summary(CHANGELOG, Some("0.10.3"), "0.10.5").as_deref(),
            Some("Five is here.\nFour is here.")
        );
        assert_eq!(
            rolled_up_summary(CHANGELOG, None, "0.10.4").as_deref(),
            Some("Four is here.")
        );
    }
}
