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
    pub notes: String,
    pub url: String,
    headline_template: String,
}

impl WhatsNewNotice {
    #[must_use]
    pub fn for_version(locale: Locale, version: &str) -> Self {
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
            notes: tr(locale, MessageId::UpdateWhatsNewNotes).replace("{url}", &url),
            url,
            headline_template: tr(locale, MessageId::UpdateWhatsNewHeadline).into_owned(),
        }
    }

    #[must_use]
    pub fn headline(&self) -> String {
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
    Some(WhatsNewNotice::for_version(locale, &change.current))
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
