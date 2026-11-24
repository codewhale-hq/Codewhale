//! The Codewhale × OrcaRouter offer. It is hidden after the campaign ends,
//! when the interface is in Simplified Chinese, or when the computer is set to
//! a mainland China time zone or locale. A VPN, a remote shell or a time zone
//! set by hand elsewhere is not detected, and the end is judged by the local
//! clock.

use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use codewhale_localization::Locale;

pub(crate) const OFFER_URL: &str =
    "https://www.orcarouter.ai/redeem/1a6250545fdc4e0aaccb0a4470159e67";

const MAINLAND_ZONES: &[&str] = &[
    "Asia/Shanghai",
    "Asia/Chongqing",
    "Asia/Chungking",
    "Asia/Harbin",
    "Asia/Urumqi",
    "Asia/Kashgar",
    "PRC",
];

const ENDS_AT: Duration = Duration::from_secs(1_795_264_197);

pub(crate) fn available(locale: Locale) -> bool {
    running_at(SystemTime::now()) && locale != Locale::ZhHans && !mainland_china()
}

fn running_at(now: SystemTime) -> bool {
    now.duration_since(UNIX_EPOCH)
        .is_ok_and(|elapsed| elapsed < ENDS_AT)
}

fn mainland_china() -> bool {
    static MAINLAND: OnceLock<bool> = OnceLock::new();
    *MAINLAND.get_or_init(|| {
        let zone = std::env::var("TZ")
            .ok()
            .filter(|zone| !zone.is_empty())
            .or_else(|| {
                std::fs::read_link("/etc/localtime")
                    .ok()
                    .map(|link| link.to_string_lossy().into_owned())
            });
        let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .filter_map(|name| std::env::var(name).ok())
            .find(|value| !value.is_empty());
        mainland(zone.as_deref(), locale.as_deref())
    })
}

fn mainland(zone: Option<&str>, locale: Option<&str>) -> bool {
    let zone_is_mainland = zone.is_some_and(|zone| {
        let zone = zone.trim_start_matches(':');
        MAINLAND_ZONES.iter().any(|name| {
            zone == *name
                || zone
                    .strip_suffix(name)
                    .is_some_and(|prefix| prefix.ends_with('/'))
        })
    });
    let locale_is_mainland = locale.is_some_and(|value| {
        let tag = value.split(['.', '@']).next().unwrap_or_default();
        tag.ends_with("_CN") || tag.ends_with("-CN")
    });
    zone_is_mainland || locale_is_mainland
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offer_ends_when_the_web_campaign_ends() {
        assert!(running_at(UNIX_EPOCH + ENDS_AT - Duration::from_secs(1)));
        assert!(!running_at(UNIX_EPOCH + ENDS_AT));
    }
}
