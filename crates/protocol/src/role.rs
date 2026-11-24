//! The closed set of message roles Codewhale can put on a transcript.
//!
//! Roles used to be free-form `String`s on [`crate::request::Message`], and
//! four wire adapters each decided independently what an unfamiliar role
//! meant: two dropped it silently, one forwarded it verbatim for the provider
//! to reject, one failed closed. [`Role`] closes the set so that decision can
//! be made once, in one table, instead of four times by accident.
//!
//! Two properties are load-bearing and are covered by tests:
//!
//! * **Byte-identical serialization.** A `Role` serializes as exactly the
//!   string it replaced and deserializes from any string. Saved transcripts
//!   therefore need no schema bump and no migration ladder, and a session
//!   written by a newer build still loads here — an unfamiliar role lands in
//!   [`Role::Unrecognized`] rather than failing the whole session load.
//! * **`assistant_interrupted` stays distinct.** The interrupted-assistant
//!   sentinel is its own variant, not a flavour of [`Role::Assistant`], so it
//!   keeps round-tripping as a separate session item.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::request::INTERRUPTED_ASSISTANT_ROLE;

/// Who authored a message in a transcript.
///
/// `Unrecognized` is deliberately part of the type: it is what lets a
/// transcript written by a future build round-trip through this one. It
/// carries no trust and grants no placement of its own — see the wire
/// adapters' placement table for what each dialect does with it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Role {
    /// Input authored by the human operator or by the harness on their behalf.
    User,
    /// Output authored by the model.
    Assistant,
    /// Harness-authored context injected into the transcript body — compaction
    /// summaries, branch summaries, sub-agent framing.
    System,
    /// Provider-supported instruction content embedded at a transcript
    /// position. Unlike the top-level system prompt, this role is load-bearing
    /// history and must retain its position on wires that support it.
    Developer,
    /// Assistant text that was visible before the turn was interrupted. Kept
    /// distinct from [`Role::Assistant`] so replay can mark it as incomplete.
    InterruptedAssistant,
    LatestReminder,
    /// A role string this build does not know. Preserved verbatim so loading
    /// and re-saving a transcript is lossless.
    Unrecognized(String),
}

impl Role {
    /// The exact wire/persisted string for this role.
    ///
    /// This is the serialization; do not let it drift from [`Serialize`].
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::System => "system",
            Self::Developer => "developer",
            Self::InterruptedAssistant => INTERRUPTED_ASSISTANT_ROLE,
            Self::LatestReminder => "latest_reminder",
            Self::Unrecognized(raw) => raw.as_str(),
        }
    }

    /// True for roles the model itself authored, interrupted output included.
    #[must_use]
    pub fn is_assistant_like(&self) -> bool {
        matches!(self, Self::Assistant | Self::InterruptedAssistant)
    }
}

impl From<&str> for Role {
    fn from(value: &str) -> Self {
        match value {
            "user" => Self::User,
            "assistant" => Self::Assistant,
            "system" => Self::System,
            "developer" => Self::Developer,
            INTERRUPTED_ASSISTANT_ROLE => Self::InterruptedAssistant,
            "latest_reminder" => Self::LatestReminder,
            other => Self::Unrecognized(other.to_string()),
        }
    }
}

impl From<String> for Role {
    fn from(value: String) -> Self {
        match value.as_str() {
            "user" => Self::User,
            "assistant" => Self::Assistant,
            "system" => Self::System,
            "developer" => Self::Developer,
            INTERRUPTED_ASSISTANT_ROLE => Self::InterruptedAssistant,
            "latest_reminder" => Self::LatestReminder,
            _ => Self::Unrecognized(value),
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl PartialEq<str> for Role {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for Role {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<String> for Role {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<Role> for str {
    fn eq(&self, other: &Role) -> bool {
        self == other.as_str()
    }
}

impl PartialEq<Role> for &str {
    fn eq(&self, other: &Role) -> bool {
        *self == other.as_str()
    }
}

impl PartialEq<Role> for String {
    fn eq(&self, other: &Role) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Serialize for Role {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Role {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from(String::deserialize(deserializer)?))
    }
}

#[cfg(test)]
mod tests {
    use super::Role;
    use crate::request::INTERRUPTED_ASSISTANT_ROLE;

    #[test]
    fn known_roles_serialize_as_the_strings_they_replaced() {
        for (role, expected) in [
            (Role::User, "\"user\""),
            (Role::Assistant, "\"assistant\""),
            (Role::System, "\"system\""),
            (Role::Developer, "\"developer\""),
            (Role::InterruptedAssistant, "\"assistant_interrupted\""),
        ] {
            assert_eq!(serde_json::to_string(&role).expect("serialize"), expected);
        }
    }

    #[test]
    fn serialized_bytes_match_the_raw_string_encoding() {
        // The persisted format must not shift: a `Role` has to produce the
        // same bytes the `String` field produced, or every saved session
        // would need a schema bump and a migration ladder.
        for raw in [
            "user",
            "assistant",
            "system",
            "developer",
            INTERRUPTED_ASSISTANT_ROLE,
        ] {
            assert_eq!(
                serde_json::to_vec(&Role::from(raw)).expect("serialize role"),
                serde_json::to_vec(raw).expect("serialize string"),
                "role {raw} must serialize like its string",
            );
        }
    }

    #[test]
    fn unknown_roles_round_trip_verbatim() {
        let role = Role::from("future_role");
        assert_eq!(role, Role::Unrecognized("future_role".to_string()));
        let encoded = serde_json::to_string(&role).expect("serialize");
        assert_eq!(encoded, "\"future_role\"");
        let decoded: Role = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded, role);
    }

    #[test]
    fn interrupted_assistant_is_not_assistant() {
        let decoded: Role =
            serde_json::from_str("\"assistant_interrupted\"").expect("deserialize sentinel");
        assert_eq!(decoded, Role::InterruptedAssistant);
        assert_ne!(decoded, Role::Assistant);
        assert_ne!(decoded, "assistant");
        assert!(decoded.is_assistant_like());
    }

    #[test]
    fn string_comparisons_work_in_both_directions() {
        assert_eq!(Role::User, "user");
        assert_eq!("user", Role::User);
        assert_eq!(Role::User, "user".to_string());
        assert_ne!(Role::System, "user");
    }

    #[test]
    fn latest_reminder_round_trips_distinct_from_system_and_user() {
        assert_eq!(Role::from("latest_reminder"), Role::LatestReminder);
        assert_eq!(Role::LatestReminder.as_str(), "latest_reminder");
        let encoded = serde_json::to_string(&Role::LatestReminder).expect("serialize");
        assert_eq!(encoded, "\"latest_reminder\"");
        let decoded: Role = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded, Role::LatestReminder);
        assert_ne!(decoded, Role::User);
        assert_ne!(decoded, Role::System);
    }
}
