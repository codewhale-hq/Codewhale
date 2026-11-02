//! Schema table for the compaction survival contract.
//!
//! Compiled only under `cfg(test)`: this is compile-time documentation of
//! the field table, not the runtime enforcement path (`last_round.rs`).
//! Field names and locations are language-invariant protocol data. The B1
//! strategy (Rust compaction path) enforces them over the session-tree
//! journal / API transcript. See [`SURVIVAL_CONTRACT.md`](./SURVIVAL_CONTRACT.md).

/// Documented schema version. Bump when a survive/summarize/prune rule
/// changes; TS/Go strategies pin this number.
pub const SURVIVAL_CONTRACT_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurvivalRule {
    Always,
    LastRoundVerbatim,
    LastRoundBounded,
    LatestOnly,
    RestateFromLiveState,
    Summarize,
    Prune,
    NeverDurable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurvivalField {
    pub name: &'static str,
    pub location: &'static str,
    pub rule: SurvivalRule,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntrySurvival {
    pub kind: &'static str,
    pub fields: &'static [SurvivalField],
}

/// Protocol journal kinds plus session-tree `message` / `system` carriers.
pub const ENTRY_SURVIVAL: &[EntrySurvival] = &[
    EntrySurvival {
        kind: "header",
        fields: &[
            SurvivalField {
                name: "id",
                location: "JournalEntry.id",
                rule: SurvivalRule::Always,
            },
            SurvivalField {
                name: "created_at",
                location: "JournalEntry.created_at",
                rule: SurvivalRule::Always,
            },
            SurvivalField {
                name: "secrets",
                location: "payload",
                rule: SurvivalRule::NeverDurable,
            },
        ],
    },
    EntrySurvival {
        kind: "user",
        fields: &[
            SurvivalField {
                name: "text",
                location: "payload / Message.content[type=text].text",
                rule: SurvivalRule::LastRoundVerbatim,
            },
            SurvivalField {
                name: "older_turns",
                location: "payload / Message.content[type=text].text",
                rule: SurvivalRule::Summarize,
            },
        ],
    },
    EntrySurvival {
        kind: "assistant",
        fields: &[
            SurvivalField {
                name: "text",
                location: "payload / Message.content[type=text].text",
                rule: SurvivalRule::LastRoundVerbatim,
            },
            SurvivalField {
                name: "tool_use.id",
                location: "Message.content[type=tool_use].id",
                rule: SurvivalRule::LastRoundVerbatim,
            },
        ],
    },
    EntrySurvival {
        kind: "tool_result",
        fields: &[
            SurvivalField {
                name: "tool_use_id",
                location: "Message.content[type=tool_result].tool_use_id",
                rule: SurvivalRule::LastRoundVerbatim,
            },
            SurvivalField {
                name: "content",
                location: "Message.content[type=tool_result].content",
                rule: SurvivalRule::LastRoundBounded,
            },
            SurvivalField {
                name: "is_error",
                location: "Message.content[type=tool_result].is_error",
                rule: SurvivalRule::LastRoundVerbatim,
            },
            SurvivalField {
                name: "older_results",
                location: "Message.content[type=tool_result].content",
                rule: SurvivalRule::Prune,
            },
        ],
    },
    EntrySurvival {
        kind: "compaction",
        fields: &[
            SurvivalField {
                name: "summary",
                location: "payload.summary / checkpoint user text",
                rule: SurvivalRule::LatestOnly,
            },
            SurvivalField {
                name: "coverage",
                location: "CompactionCoverage / /context inspector",
                rule: SurvivalRule::LatestOnly,
            },
        ],
    },
    EntrySurvival {
        kind: "branch_summary",
        fields: &[
            SurvivalField {
                name: "branch_id",
                location: "payload.branch_id",
                rule: SurvivalRule::Always,
            },
            SurvivalField {
                name: "summary",
                location: "payload.summary",
                rule: SurvivalRule::Always,
            },
        ],
    },
    EntrySurvival {
        kind: "message",
        fields: &[
            SurvivalField {
                name: "role",
                location: "SessionEntryKind::Message.message.role",
                rule: SurvivalRule::LastRoundVerbatim,
            },
            SurvivalField {
                name: "content",
                location: "SessionEntryKind::Message.message.content",
                rule: SurvivalRule::LastRoundBounded,
            },
        ],
    },
    EntrySurvival {
        kind: "system",
        fields: &[SurvivalField {
            name: "content",
            location: "SessionEntryKind::System.content",
            rule: SurvivalRule::Always,
        }],
    },
];

pub const LIVE_STATE_FIELDS: &[SurvivalField] = &[
    SurvivalField {
        name: "anchors",
        location: ".codewhale/anchors.md restated on the checkpoint",
        rule: SurvivalRule::RestateFromLiveState,
    },
    SurvivalField {
        name: "branch_worktree",
        location: "live git / workspace",
        rule: SurvivalRule::RestateFromLiveState,
    },
    SurvivalField {
        name: "background_work_handles",
        location: "session + worker registry",
        rule: SurvivalRule::RestateFromLiveState,
    },
    SurvivalField {
        name: "billed_parent_prompt_tokens",
        location: "TurnContext.latest_parent_input_tokens",
        rule: SurvivalRule::LatestOnly,
    },
    SurvivalField {
        name: "compaction_receipt",
        location: "checkpoint user message + HistoryCell::System",
        rule: SurvivalRule::LatestOnly,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA_DOC: &str = include_str!("SURVIVAL_CONTRACT.md");

    #[test]
    fn schema_doc_lists_every_entry_kind_and_survive_field() {
        let kinds: Vec<_> = ENTRY_SURVIVAL.iter().map(|entry| entry.kind).collect();
        assert!(kinds.contains(&"user"));
        assert!(kinds.contains(&"tool_result"));
        assert!(kinds.contains(&"compaction"));
        for entry in ENTRY_SURVIVAL {
            assert!(
                SCHEMA_DOC.contains(&format!("`{}`", entry.kind)),
                "SURVIVAL_CONTRACT.md must document kind {}",
                entry.kind
            );
            for field in entry.fields.iter().filter(|field| {
                matches!(
                    field.rule,
                    SurvivalRule::Always
                        | SurvivalRule::LastRoundVerbatim
                        | SurvivalRule::LastRoundBounded
                        | SurvivalRule::LatestOnly
                )
            }) {
                let token = field.name.split('.').next().unwrap_or(field.name);
                assert!(
                    SCHEMA_DOC.contains(token) || SCHEMA_DOC.contains(field.name),
                    "SURVIVAL_CONTRACT.md must locate {}.{}",
                    entry.kind,
                    field.name
                );
            }
        }
        for field in LIVE_STATE_FIELDS {
            assert!(
                SCHEMA_DOC.to_ascii_lowercase().contains("anchor")
                    && SCHEMA_DOC.contains("receipt"),
                "live-state field {} must stay in the schema doc",
                field.name
            );
        }
        assert!(SCHEMA_DOC.contains("Failed compact must not replace live history"));
        assert!(
            SCHEMA_DOC.contains(&format!("Schema version {SURVIVAL_CONTRACT_VERSION}")),
            "SURVIVAL_CONTRACT.md must pin schema version {SURVIVAL_CONTRACT_VERSION}"
        );
    }

    #[test]
    fn last_round_kinds_have_verbatim_or_bounded_rules() {
        for kind in ["user", "assistant", "tool_result", "message"] {
            let entry = ENTRY_SURVIVAL
                .iter()
                .find(|entry| entry.kind == kind)
                .unwrap_or_else(|| panic!("missing {kind}"));
            assert!(
                entry.fields.iter().any(|field| matches!(
                    field.rule,
                    SurvivalRule::LastRoundVerbatim | SurvivalRule::LastRoundBounded
                )),
                "{kind} must declare a last-round survival rule"
            );
        }
    }

    #[test]
    fn compaction_kind_keeps_latest_receipt_only() {
        let entry = ENTRY_SURVIVAL
            .iter()
            .find(|entry| entry.kind == "compaction")
            .expect("compaction kind");
        assert!(
            entry
                .fields
                .iter()
                .any(|field| field.name == "summary" && field.rule == SurvivalRule::LatestOnly)
        );
        assert!(
            LIVE_STATE_FIELDS
                .iter()
                .any(|field| field.name == "compaction_receipt")
        );
        assert!(
            LIVE_STATE_FIELDS
                .iter()
                .any(|field| field.name == "anchors")
        );
    }
}
