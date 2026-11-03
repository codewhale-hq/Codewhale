use serde::{Deserialize, Serialize};

pub const MAX_WATCH_EVENT_RECEIPT_BYTES: usize = 4096;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WatchEventReceipt {
    pub version: u32,
    pub platform_account_id: String,
    pub project_id: String,
    pub watch_id: String,
    pub watch_revision: u64,
    pub definition_sha256: String,
    pub source_kind: WatchEventSourceKind,
    pub occurrence_sha256: String,
    pub event_sha256: String,
    pub run_id: String,
    pub room_thread_root_id: String,
    pub source: WatchEventSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum WatchEventSourceKind {
    #[serde(rename = "schedule")]
    Schedule,
    #[serde(rename = "github.pull_request")]
    GitHubPullRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum WatchEventSource {
    Schedule(WatchScheduleSource),
    GitHubPullRequest(WatchPullRequestSource),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WatchScheduleSource {
    pub slot: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WatchPullRequestSource {
    pub installation_id: String,
    pub binding_id: String,
    pub repo: String,
    pub number: u64,
    pub delivery_id: String,
    pub event_name: String,
}

fn reference(value: &str) -> bool {
    (1..=200).contains(&value.len())
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:/@#-".contains(&byte))
}

fn fingerprint(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn canonical_utc_slot(value: &str) -> bool {
    value.len() == 24
        && chrono::DateTime::parse_from_rfc3339(value).is_ok_and(|parsed| {
            parsed.timestamp_subsec_nanos() < 1_000_000_000
                && parsed.to_rfc3339_opts(chrono::SecondsFormat::Millis, true) == value
        })
}

impl WatchEventReceipt {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != 1 || self.watch_revision == 0 || self.watch_revision > MAX_SAFE_INTEGER {
            return Err("unsupported watch event version or revision");
        }
        if [
            &self.platform_account_id,
            &self.project_id,
            &self.watch_id,
            &self.run_id,
        ]
        .iter()
        .any(|value| !reference(value))
            || [
                &self.definition_sha256,
                &self.occurrence_sha256,
                &self.event_sha256,
            ]
            .iter()
            .any(|value| !fingerprint(value))
        {
            return Err("invalid watch event reference or fingerprint");
        }
        if !self.room_thread_root_id.is_empty()
            && (self.room_thread_root_id.len() != 35
                || !self.room_thread_root_id.starts_with("pm_")
                || !self.room_thread_root_id.as_bytes()[3..]
                    .iter()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)))
        {
            return Err("invalid watch event room thread");
        }
        match (&self.source_kind, &self.source) {
            (WatchEventSourceKind::Schedule, WatchEventSource::Schedule(source)) => {
                if source.slot.len() != 24
                    || !reference(&source.slot)
                    || !canonical_utc_slot(&source.slot)
                {
                    return Err("invalid watch event schedule occurrence");
                }
            }
            (
                WatchEventSourceKind::GitHubPullRequest,
                WatchEventSource::GitHubPullRequest(source),
            ) => {
                let valid_repo = source.repo.split_once('/').is_some_and(|(owner, name)| {
                    [owner, name].iter().all(|part| {
                        (1..=100).contains(&part.len())
                            && part
                                .bytes()
                                .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
                    })
                });
                if source.number == 0
                    || source.number > MAX_SAFE_INTEGER
                    || [
                        &source.installation_id,
                        &source.binding_id,
                        &source.delivery_id,
                        &source.event_name,
                    ]
                    .iter()
                    .any(|value| !reference(value))
                    || !valid_repo
                {
                    return Err("invalid watch event pull request occurrence");
                }
            }
            _ => return Err("watch event kind does not match its source"),
        }
        if serde_json::to_vec(self)
            .map_err(|_| "watch event cannot be encoded")?
            .len()
            > MAX_WATCH_EVENT_RECEIPT_BYTES
        {
            return Err("watch event receipt exceeds 4 KiB");
        }
        Ok(())
    }
}
