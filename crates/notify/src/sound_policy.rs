//! One audio decision for notification delivery. The caller first applies
//! attention, duration, method, quiet and category gates. This policy selects
//! one cue and preserves per-category repeat history across settings updates.

use super::payload::NotificationKind;
pub use codewhale_config::notifications::NotificationEvent as SoundEvent;
use codewhale_config::notifications::{CompletionSound, NotificationsConfig};
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};

pub fn event_for_kind(kind: NotificationKind) -> SoundEvent {
    match kind {
        NotificationKind::TurnComplete => SoundEvent::TurnComplete,
        NotificationKind::SubagentTerminal | NotificationKind::BackgroundTerminal => {
            SoundEvent::SubagentTerminal
        }
        NotificationKind::ApprovalNeeded => SoundEvent::ApprovalNeeded,
        NotificationKind::InputNeeded => SoundEvent::InputNeeded,
        NotificationKind::ElevationNeeded => SoundEvent::ElevationNeeded,
        NotificationKind::ModelNotify => SoundEvent::ModelNotify,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SoundCue {
    Bell,
    DoubleBell,
    Beep,
    Whale,
    File(PathBuf),
}

pub fn cue_for(event: SoundEvent) -> SoundCue {
    match event {
        SoundEvent::TurnComplete | SoundEvent::SubagentTerminal => SoundCue::Bell,
        SoundEvent::ApprovalNeeded | SoundEvent::ElevationNeeded => SoundCue::DoubleBell,
        SoundEvent::InputNeeded | SoundEvent::ModelNotify => SoundCue::Beep,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuppressReason {
    Disabled,
    QuietMode,
    NotListed,
    RateLimited,
    MissingFile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SoundDecision {
    Play(SoundCue),
    Suppress(SuppressReason),
}

#[derive(Debug, Clone, Default)]
pub struct EventSoundPolicy {
    config: NotificationsConfig,
    last_played_ms: [Option<u64>; 6],
}

impl EventSoundPolicy {
    pub fn from_config(config: &NotificationsConfig) -> Self {
        Self {
            config: config.clone(),
            last_played_ms: [None; 6],
        }
    }

    pub fn decide(
        &mut self,
        event: SoundEvent,
        now_ms: u64,
        bell_transport: bool,
    ) -> SoundDecision {
        Self::decide_configured(
            &self.config,
            &mut self.last_played_ms,
            event,
            now_ms,
            bell_transport,
        )
    }

    fn decide_configured(
        config: &NotificationsConfig,
        last_played_ms: &mut [Option<u64>; 6],
        event: SoundEvent,
        now_ms: u64,
        bell_transport: bool,
    ) -> SoundDecision {
        if config.quiet {
            return SoundDecision::Suppress(SuppressReason::QuietMode);
        }
        let selected = if let Some(sound) = config.sound {
            Some(sound)
        } else if event == SoundEvent::TurnComplete
            && config.completion_sound != CompletionSound::Off
        {
            Some(config.completion_sound)
        } else {
            if config.event_sound.quiet {
                return SoundDecision::Suppress(SuppressReason::QuietMode);
            }
            if config.event_sound.enabled {
                if !config
                    .event_sound
                    .events
                    .iter()
                    .any(|name| SoundEvent::parse(name) == Some(event))
                {
                    return SoundDecision::Suppress(SuppressReason::NotListed);
                }
                None
            } else if bell_transport {
                Some(CompletionSound::Bell)
            } else {
                return SoundDecision::Suppress(SuppressReason::Disabled);
            }
        };
        let cue = match selected {
            Some(CompletionSound::Off) => return SoundDecision::Suppress(SuppressReason::Disabled),
            Some(CompletionSound::Whale) => SoundCue::Whale,
            Some(CompletionSound::Bell) => SoundCue::Bell,
            Some(CompletionSound::Beep) => SoundCue::Beep,
            Some(CompletionSound::File) => match &config.sound_file {
                Some(path) => SoundCue::File(path.clone()),
                None => return SoundDecision::Suppress(SuppressReason::MissingFile),
            },
            None => cue_for(event),
        };
        let slot = &mut last_played_ms[event.index()];
        if slot.is_some_and(|last| now_ms.saturating_sub(last) < config.event_sound.min_interval_ms)
        {
            return SoundDecision::Suppress(SuppressReason::RateLimited);
        }
        *slot = Some(now_ms);
        SoundDecision::Play(cue)
    }
}

pub fn epoch_millis_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

static POLICY: OnceLock<RwLock<EventSoundPolicy>> = OnceLock::new();
fn policy_cell() -> &'static RwLock<EventSoundPolicy> {
    POLICY.get_or_init(|| RwLock::new(EventSoundPolicy::default()))
}

pub fn reconfigure(mut policy: EventSoundPolicy) {
    if let Ok(mut slot) = policy_cell().write() {
        policy.last_played_ms = slot.last_played_ms;
        *slot = policy;
    }
}

pub fn decide(kind: NotificationKind, now_ms: u64, bell_transport: bool) -> SoundDecision {
    policy_cell()
        .write()
        .map(|mut policy| policy.decide(event_for_kind(kind), now_ms, bell_transport))
        .unwrap_or(SoundDecision::Suppress(SuppressReason::Disabled))
}

/// Give back a repeat slot that `decide` reserved for a cue that did not play.
/// Only the exact reservation is released, so a later decision in the same
/// category keeps its own slot.
pub fn release(kind: NotificationKind, reserved_ms: u64) {
    if let Ok(mut policy) = policy_cell().write() {
        let slot = &mut policy.last_played_ms[event_for_kind(kind).index()];
        if *slot == Some(reserved_ms) {
            *slot = None;
        }
    }
}

/// Decide from this request's configuration while holding the shared history
/// lock. Request snapshots never replace the installed TUI/model policy.
pub fn decide_configured(
    config: &NotificationsConfig,
    kind: NotificationKind,
    now_ms: u64,
    bell_transport: bool,
) -> SoundDecision {
    policy_cell()
        .write()
        .map(|mut policy| {
            EventSoundPolicy::decide_configured(
                config,
                &mut policy.last_played_ms,
                event_for_kind(kind),
                now_ms,
                bell_transport,
            )
        })
        .unwrap_or(SoundDecision::Suppress(SuppressReason::Disabled))
}

#[cfg(any(test, feature = "test-support"))]
pub fn configure(policy: EventSoundPolicy) {
    *policy_cell().write().unwrap() = policy;
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_config::notifications::EventSoundConfig;

    #[test]
    fn canonical_whale_controls_all_six_categories_over_legacy_controls() {
        let config = NotificationsConfig {
            sound: Some(CompletionSound::Whale),
            completion_sound: CompletionSound::Bell,
            event_sound: EventSoundConfig {
                quiet: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut policy = EventSoundPolicy::from_config(&config);
        for event in SoundEvent::ALL {
            assert_eq!(
                policy.decide(event, 0, false),
                SoundDecision::Play(SoundCue::Whale)
            );
        }
    }

    #[test]
    fn explicit_off_silences_legacy_sounds_and_bell_transport() {
        let config = NotificationsConfig {
            sound: Some(CompletionSound::Off),
            completion_sound: CompletionSound::Whale,
            event_sound: EventSoundConfig {
                enabled: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut policy = EventSoundPolicy::from_config(&config);
        for event in SoundEvent::ALL {
            assert_eq!(
                policy.decide(event, 0, true),
                SoundDecision::Suppress(SuppressReason::Disabled)
            );
        }
    }

    #[test]
    fn legacy_completion_uses_same_decision_and_other_categories_keep_allowlist() {
        let mut policy = EventSoundPolicy::from_config(&NotificationsConfig {
            completion_sound: CompletionSound::Whale,
            event_sound: EventSoundConfig {
                enabled: true,
                ..Default::default()
            },
            ..Default::default()
        });
        assert_eq!(
            policy.decide(SoundEvent::TurnComplete, 0, false),
            SoundDecision::Play(SoundCue::Whale)
        );
        assert_eq!(
            policy.decide(SoundEvent::ApprovalNeeded, 0, false),
            SoundDecision::Play(SoundCue::DoubleBell)
        );
        assert_eq!(
            policy.decide(SoundEvent::InputNeeded, 0, false),
            SoundDecision::Suppress(SuppressReason::NotListed)
        );
    }

    #[test]
    fn default_sound_is_opt_in_but_explicit_bell_transport_selects_one_bell() {
        let mut policy = EventSoundPolicy::default();
        assert_eq!(
            policy.decide(SoundEvent::TurnComplete, 0, false),
            SoundDecision::Suppress(SuppressReason::Disabled)
        );
        assert_eq!(
            policy.decide(SoundEvent::TurnComplete, 0, true),
            SoundDecision::Play(SoundCue::Bell)
        );
    }

    #[test]
    fn rate_limit_is_per_category_handles_clock_reversal_and_exact_boundary() {
        let mut policy = EventSoundPolicy::from_config(&NotificationsConfig {
            sound: Some(CompletionSound::Whale),
            ..Default::default()
        });
        assert_eq!(
            policy.decide(SoundEvent::TurnComplete, 5000, false),
            SoundDecision::Play(SoundCue::Whale)
        );
        for now in [1000, 5001, 6999] {
            assert_eq!(
                policy.decide(SoundEvent::TurnComplete, now, false),
                SoundDecision::Suppress(SuppressReason::RateLimited)
            );
        }
        assert_eq!(
            policy.decide(SoundEvent::ApprovalNeeded, 5001, false),
            SoundDecision::Play(SoundCue::Whale)
        );
        assert_eq!(
            policy.decide(SoundEvent::TurnComplete, 7000, false),
            SoundDecision::Play(SoundCue::Whale)
        );
    }

    #[test]
    fn settings_refresh_preserves_repeat_history() {
        let _lock = codewhale_runtime::test_support::lock_test_env();
        let config = NotificationsConfig {
            sound: Some(CompletionSound::Whale),
            ..Default::default()
        };
        configure(EventSoundPolicy::from_config(&config));
        assert_eq!(
            decide(NotificationKind::ApprovalNeeded, 10, false),
            SoundDecision::Play(SoundCue::Whale)
        );
        reconfigure(EventSoundPolicy::from_config(&config));
        assert_eq!(
            decide(NotificationKind::ApprovalNeeded, 11, false),
            SoundDecision::Suppress(SuppressReason::RateLimited)
        );
        configure(EventSoundPolicy::default());
    }

    #[test]
    fn configured_off_decision_ignores_intervening_installed_whale_policy() {
        let _lock = codewhale_runtime::test_support::lock_test_env();
        let off = NotificationsConfig {
            sound: Some(CompletionSound::Off),
            ..Default::default()
        };
        configure(EventSoundPolicy::from_config(&off));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let request = std::thread::spawn(move || {
            ready_tx.send(()).unwrap();
            resume_rx.recv().unwrap();
            decide_configured(&off, NotificationKind::ApprovalNeeded, 10, false)
        });
        ready_rx.recv().unwrap();
        reconfigure(EventSoundPolicy::from_config(&NotificationsConfig {
            sound: Some(CompletionSound::Whale),
            ..Default::default()
        }));
        resume_tx.send(()).unwrap();
        assert_eq!(
            request.join().unwrap(),
            SoundDecision::Suppress(SuppressReason::Disabled)
        );
        assert_eq!(
            decide(NotificationKind::ApprovalNeeded, 10, false),
            SoundDecision::Play(SoundCue::Whale),
            "the suppressed request neither replaces defaults nor consumes history"
        );
        configure(EventSoundPolicy::default());
    }

    #[test]
    fn configured_sound_keeps_installed_defaults_and_suppression_keeps_history() {
        let _lock = codewhale_runtime::test_support::lock_test_env();
        let off = NotificationsConfig {
            sound: Some(CompletionSound::Off),
            ..Default::default()
        };
        let whale = NotificationsConfig {
            sound: Some(CompletionSound::Whale),
            ..Default::default()
        };
        configure(EventSoundPolicy::from_config(&off));
        assert_eq!(
            decide_configured(&whale, NotificationKind::InputNeeded, 100, false),
            SoundDecision::Play(SoundCue::Whale)
        );
        for config in [
            off,
            NotificationsConfig {
                quiet: true,
                ..whale.clone()
            },
        ] {
            assert!(matches!(
                decide_configured(&config, NotificationKind::InputNeeded, 101, false),
                SoundDecision::Suppress(_)
            ));
        }
        assert_eq!(
            decide_configured(&whale, NotificationKind::InputNeeded, 102, false),
            SoundDecision::Suppress(SuppressReason::RateLimited)
        );
        assert_eq!(
            decide(NotificationKind::InputNeeded, 2_100, false),
            SoundDecision::Suppress(SuppressReason::Disabled),
            "a request must not replace installed defaults, even after cooldown"
        );
        assert_eq!(
            decide_configured(&whale, NotificationKind::InputNeeded, 2_100, false),
            SoundDecision::Play(SoundCue::Whale),
            "suppression must not move the original exact cooldown boundary"
        );
        configure(EventSoundPolicy::default());
    }

    #[test]
    fn concurrent_configured_decisions_share_history_with_settings_refresh() {
        let _lock = codewhale_runtime::test_support::lock_test_env();
        configure(EventSoundPolicy::default());
        let whale = NotificationsConfig {
            sound: Some(CompletionSound::Whale),
            ..Default::default()
        };
        let start = std::sync::Arc::new(std::sync::Barrier::new(3));
        let requests = (0..2)
            .map(|_| {
                let start = start.clone();
                let config = whale.clone();
                std::thread::spawn(move || {
                    start.wait();
                    decide_configured(&config, NotificationKind::ApprovalNeeded, 10, false)
                })
            })
            .collect::<Vec<_>>();
        start.wait();
        let results = requests
            .into_iter()
            .map(|request| request.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            results
                .iter()
                .filter(|result| **result == SoundDecision::Play(SoundCue::Whale))
                .count(),
            1
        );
        assert_eq!(
            results
                .iter()
                .filter(|result| **result == SoundDecision::Suppress(SuppressReason::RateLimited))
                .count(),
            1
        );
        reconfigure(EventSoundPolicy::from_config(&whale));
        assert_eq!(
            decide(NotificationKind::ApprovalNeeded, 11, false),
            SoundDecision::Suppress(SuppressReason::RateLimited)
        );
        assert_eq!(
            decide_configured(&whale, NotificationKind::InputNeeded, 11, false),
            SoundDecision::Play(SoundCue::Whale)
        );
        assert_eq!(
            decide_configured(&whale, NotificationKind::ApprovalNeeded, 2_010, false),
            SoundDecision::Play(SoundCue::Whale)
        );
        configure(EventSoundPolicy::default());
    }

    #[test]
    fn custom_file_requires_path_and_never_substitutes_bell() {
        let mut config = NotificationsConfig {
            sound: Some(CompletionSound::File),
            ..Default::default()
        };
        assert_eq!(
            EventSoundPolicy::from_config(&config).decide(SoundEvent::TurnComplete, 0, true),
            SoundDecision::Suppress(SuppressReason::MissingFile)
        );
        config.sound_file = Some(PathBuf::from("file with spaces.wav"));
        assert_eq!(
            EventSoundPolicy::from_config(&config).decide(SoundEvent::TurnComplete, 0, true),
            SoundDecision::Play(SoundCue::File(PathBuf::from("file with spaces.wav")))
        );
    }

    #[test]
    fn master_quiet_suppresses_every_sound_and_does_not_consume_repeat_slot() {
        let mut policy = EventSoundPolicy::from_config(&NotificationsConfig {
            sound: Some(CompletionSound::Whale),
            quiet: true,
            ..Default::default()
        });
        for event in SoundEvent::ALL {
            assert_eq!(
                policy.decide(event, 0, true),
                SoundDecision::Suppress(SuppressReason::QuietMode)
            );
        }
        policy.config.quiet = false;
        assert_eq!(
            policy.decide(SoundEvent::TurnComplete, 1, false),
            SoundDecision::Play(SoundCue::Whale)
        );
    }

    #[test]
    fn shared_event_vocabulary_maps_every_payload_kind() {
        let kinds = [
            NotificationKind::TurnComplete,
            NotificationKind::SubagentTerminal,
            NotificationKind::ApprovalNeeded,
            NotificationKind::InputNeeded,
            NotificationKind::ElevationNeeded,
            NotificationKind::ModelNotify,
        ];
        assert_eq!(kinds.map(event_for_kind), SoundEvent::ALL);
        assert_eq!(
            event_for_kind(NotificationKind::BackgroundTerminal),
            SoundEvent::SubagentTerminal
        );
        for event in SoundEvent::ALL {
            assert_eq!(SoundEvent::parse(event.as_str()), Some(event));
        }
        assert_eq!(SoundEvent::parse("unknown"), None);
    }
}
