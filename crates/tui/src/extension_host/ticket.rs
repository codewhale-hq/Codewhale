//! Capability tickets: what lets the host ask the core for one specific thing.
//!
//! A ticket is an opaque id the core mints and keeps a row for. The host can
//! only hand it back; what the ticket is good for lives in the row, on the
//! Rust side, and [`TicketTable::redeem`] checks every field of it under one
//! lock. Today there is one kind, [`TicketKind::Invocation`]: minted for one
//! `tool/call` that runs under the turn loop's permission gate, and redeemed
//! by `core/call` (`super::core_call`). Other kinds (a process launch, a
//! fetch, an MCP grant) join the enum with their first redeemer.
//!
//! A ticket narrows what a frame may do; it does not isolate plugins that
//! share the host process (design section 4.4): a plugin can read the ticket
//! of a call made to it, and so can others in the same process. What stops
//! that being useful is the row: the owner (its generation and token), the
//! tier and the host generation are all checked, so a ticket presented by
//! another owner, another tier, a restarted host or for another method is
//! refused, and it dies with its invocation, its owner and its host.
//!
//! Tickets are never persisted, logged or put in a diagnostic: [`Ticket`]'s
//! `Debug` is redacted, the table has none, and no refusal quotes one.
//!
//! A burst of invalid presentations from one host is a protocol violation
//! ([`TicketTable::redeem`] reports it; the channel ends the host): a host
//! that guesses ids or replays old ones is not honestly confused.
//!
//! Known limit: ids are 244 random bits from the OS generator (two UUIDv4
//! bodies), found by hash lookup rather than compared in constant time; a
//! host that can time that lookup already holds the ticket it is probing for
//! or none worth having.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::tier::HostTier;
use codewhale_config::OwnerRef;

/// What a ticket is for. A kind says how it is used up, and nothing else; the
/// row says the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TicketKind {
    /// One `tool/call` running under the turn loop's gate: redeemable by that
    /// call's `core/call` requests, up to the row's `uses`.
    Invocation,
    McpLaunch,
    McpOperation,
    Execution,
}

impl TicketKind {
    /// A single-use ticket is removed when used; a budgeted one stays (at zero
    /// uses it refuses as exhausted) until it is revoked.
    fn single_use(self) -> bool {
        match self {
            Self::Invocation => false,
            Self::McpLaunch | Self::McpOperation | Self::Execution => true,
        }
    }
}

/// An opaque ticket id. Not `Display`, and `Debug` is redacted.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct Ticket(String);

impl Ticket {
    /// The id, to send to the host (and nowhere else).
    #[must_use]
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Ticket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Ticket(..)")
    }
}

/// What a new ticket is good for.
pub(crate) struct Grant {
    pub kind: TicketKind,
    pub tier: HostTier,
    /// The host process generation that may present it.
    pub host_generation: u64,
    pub owner: OwnerRef,
    /// The protocol method that may redeem it.
    pub method: &'static str,
    /// What it names: for an invocation, the `tool/call` id. Compared as
    /// parsed JSON when a redeemer states one.
    pub target: Value,
    pub ttl: Duration,
    pub uses: u32,
}

struct Row {
    kind: TicketKind,
    tier: HostTier,
    host_generation: u64,
    owner: OwnerRef,
    method: &'static str,
    target: Value,
    expires: Instant,
    uses_left: u32,
}

/// What the redeemer says it is.
pub(crate) struct Presented<'a> {
    pub ticket: &'a str,
    pub kind: TicketKind,
    pub tier: HostTier,
    pub host_generation: u64,
    pub owner: &'a OwnerRef,
    pub method: &'a str,
    /// The target the request names, if it names one.
    pub target: Option<&'a Value>,
}

/// Why a presentation was refused. Never carries a ticket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    Unknown,
    Expired,
    /// A valid ticket whose uses are spent: a limit, not a forgery.
    Exhausted,
    WrongKind,
    WrongTier,
    WrongGeneration,
    WrongOwner,
    WrongMethod,
    WrongTarget,
}

impl Refusal {
    /// The words the host is told. Generic on purpose: which field mismatched
    /// is not an oracle for the host.
    #[must_use]
    pub(crate) fn describe(self) -> &'static str {
        match self {
            Self::Exhausted => "this invocation's core/call limit is used up",
            Self::Expired => "this invocation's ticket expired",
            _ => "the ticket is not valid for this request",
        }
    }
}

/// A refusal, and whether it makes the host's frames a protocol violation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Refused {
    pub reason: Refusal,
    /// This refusal tipped the host into a burst of invalid presentations
    /// ([`INVALID_BURST`] within [`INVALID_WINDOW`]).
    pub violation: bool,
}

/// This many invalid presentations from one host process within
/// [`INVALID_WINDOW`] are a protocol violation.
pub(crate) const INVALID_BURST: usize = 8;
pub(crate) const INVALID_WINDOW: Duration = Duration::from_secs(60);

#[derive(Default)]
struct State {
    rows: HashMap<String, Row>,
    /// Recent invalid presentations, by host process `(tier, generation)`.
    invalid: HashMap<(HostTier, u64), VecDeque<Instant>>,
}

/// Every live ticket, behind one mutex.
#[derive(Default)]
pub(crate) struct TicketTable {
    state: Mutex<State>,
}

fn mint_id() -> String {
    // Two v4 UUIDs: 244 random bits from the OS generator.
    format!(
        "cwt.{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

impl TicketTable {
    /// Mint a ticket for `grant`.
    pub(crate) fn mint(&self, grant: Grant) -> Ticket {
        let id = mint_id();
        self.state.lock().expect("ticket lock").rows.insert(
            id.clone(),
            Row {
                kind: grant.kind,
                tier: grant.tier,
                host_generation: grant.host_generation,
                owner: grant.owner,
                method: grant.method,
                target: grant.target,
                expires: Instant::now() + grant.ttl,
                uses_left: grant.uses,
            },
        );
        Ticket(id)
    }

    /// Check `presented` against its row, every field, under one lock, and
    /// use one of its uses. Returns the row's target. A refusal that is not a
    /// mere limit counts toward the host's burst.
    pub(crate) fn redeem(&self, presented: &Presented<'_>) -> Result<Value, Refused> {
        self.redeem_at(presented, Instant::now())
    }

    fn redeem_at(&self, presented: &Presented<'_>, now: Instant) -> Result<Value, Refused> {
        let mut state = self.state.lock().expect("ticket lock");
        let verdict = match state.rows.get_mut(presented.ticket) {
            None => Err(Refusal::Unknown),
            Some(row) => Self::check(row, presented, now).map(|target| {
                row.uses_left -= 1;
                (target, row.uses_left == 0 && row.kind.single_use())
            }),
        };
        match verdict {
            Ok((target, spent)) => {
                if spent {
                    state.rows.remove(presented.ticket);
                }
                Ok(target)
            }
            Err(reason) => {
                let violation =
                    reason != Refusal::Exhausted && Self::note_invalid(&mut state, presented, now);
                Err(Refused { reason, violation })
            }
        }
    }

    fn check(row: &Row, presented: &Presented<'_>, now: Instant) -> Result<Value, Refusal> {
        if row.kind != presented.kind {
            return Err(Refusal::WrongKind);
        }
        if row.tier != presented.tier {
            return Err(Refusal::WrongTier);
        }
        if row.host_generation != presented.host_generation {
            return Err(Refusal::WrongGeneration);
        }
        if row.owner != *presented.owner {
            return Err(Refusal::WrongOwner);
        }
        if row.method != presented.method {
            return Err(Refusal::WrongMethod);
        }
        if presented.target.is_some_and(|target| *target != row.target) {
            return Err(Refusal::WrongTarget);
        }
        if now >= row.expires {
            return Err(Refusal::Expired);
        }
        if row.uses_left == 0 {
            return Err(Refusal::Exhausted);
        }
        Ok(row.target.clone())
    }

    /// Record one invalid presentation by `presented`'s host; whether that
    /// made a burst.
    fn note_invalid(state: &mut State, presented: &Presented<'_>, now: Instant) -> bool {
        let recent = state
            .invalid
            .entry((presented.tier, presented.host_generation))
            .or_default();
        while recent
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= INVALID_WINDOW)
        {
            recent.pop_front();
        }
        recent.push_back(now);
        recent.len() >= INVALID_BURST
    }

    /// Revoke one ticket (its invocation ended). Idempotent.
    pub(crate) fn revoke(&self, ticket: &Ticket) {
        self.state
            .lock()
            .expect("ticket lock")
            .rows
            .remove(ticket.expose());
    }

    /// Revoke every ticket of `plugin_id` (its owner was revoked).
    pub(crate) fn revoke_owner(&self, plugin_id: &str) {
        self.state
            .lock()
            .expect("ticket lock")
            .rows
            .retain(|_, row| row.owner.plugin_id != plugin_id);
    }

    /// Revoke every ticket of one host process and forget its invalid-burst
    /// record (the host exited).
    pub(crate) fn revoke_host(&self, tier: HostTier, host_generation: u64) {
        let mut state = self.state.lock().expect("ticket lock");
        state
            .rows
            .retain(|_, row| !(row.tier == tier && row.host_generation == host_generation));
        state.invalid.remove(&(tier, host_generation));
    }

    /// How many tickets are live.
    #[cfg(test)]
    pub(crate) fn live(&self) -> usize {
        self.state.lock().expect("ticket lock").rows.len()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn owner(id: &str, token: &str) -> OwnerRef {
        OwnerRef {
            plugin_id: id.to_string(),
            generation: 1,
            owner_token: token.to_string(),
        }
    }

    fn grant(uses: u32) -> Grant {
        Grant {
            kind: TicketKind::Invocation,
            tier: HostTier::Plugin,
            host_generation: 3,
            owner: owner("a", "token-a"),
            method: "core/call",
            target: json!({"call_id": "c1", "n": 1}),
            ttl: Duration::from_secs(60),
            uses,
        }
    }

    fn presented<'a>(ticket: &'a Ticket, owner: &'a OwnerRef) -> Presented<'a> {
        Presented {
            ticket: ticket.expose(),
            kind: TicketKind::Invocation,
            tier: HostTier::Plugin,
            host_generation: 3,
            owner,
            method: "core/call",
            target: None,
        }
    }

    fn refusal_of(table: &TicketTable, presented: &Presented<'_>) -> Refusal {
        table.redeem(presented).unwrap_err().reason
    }

    #[test]
    fn a_ticket_redeems_only_for_exactly_what_it_was_minted_for() {
        let table = TicketTable::default();
        let ticket = table.mint(grant(5));
        let a = owner("a", "token-a");
        assert_eq!(
            table.redeem(&presented(&ticket, &a)).unwrap(),
            json!({"call_id": "c1", "n": 1})
        );

        // Every field is checked: owner (plugin, token, generation), tier,
        // host generation, method; and a stated target, as parsed JSON.
        let mut other_token = a.clone();
        other_token.owner_token = "token-x".to_string();
        let mut other_generation = a.clone();
        other_generation.generation = 2;
        let b = owner("b", "token-a");
        for (owner, expected) in [
            (&other_token, Refusal::WrongOwner),
            (&other_generation, Refusal::WrongOwner),
            (&b, Refusal::WrongOwner),
        ] {
            assert_eq!(refusal_of(&table, &presented(&ticket, owner)), expected);
        }
        let mut wrong_tier = presented(&ticket, &a);
        wrong_tier.tier = HostTier::Builtin;
        assert_eq!(refusal_of(&table, &wrong_tier), Refusal::WrongTier);
        let mut wrong_generation = presented(&ticket, &a);
        wrong_generation.host_generation = 4;
        assert_eq!(
            refusal_of(&table, &wrong_generation),
            Refusal::WrongGeneration
        );
        let mut wrong_method = presented(&ticket, &a);
        wrong_method.method = "registry/register";
        assert_eq!(refusal_of(&table, &wrong_method), Refusal::WrongMethod);
        let other_target = json!({"call_id": "c2", "n": 1});
        let mut wrong_target = presented(&ticket, &a);
        wrong_target.target = Some(&other_target);
        assert_eq!(refusal_of(&table, &wrong_target), Refusal::WrongTarget);
        // Parsed JSON, not bytes: key order does not matter.
        let same_target: Value = serde_json::from_str(r#"{"n": 1, "call_id": "c1"}"#).unwrap();
        let mut right_target = presented(&ticket, &a);
        right_target.target = Some(&same_target);
        assert!(table.redeem(&right_target).is_ok());
        // Unknown.
        let unknown = Ticket("cwt.nope".to_string());
        assert_eq!(
            refusal_of(&table, &presented(&unknown, &a)),
            Refusal::Unknown
        );
    }

    #[test]
    fn a_ticket_expires_and_runs_out_of_uses() {
        let table = TicketTable::default();
        let ticket = table.mint(grant(2));
        let a = owner("a", "token-a");
        let now = Instant::now();
        assert!(table.redeem_at(&presented(&ticket, &a), now).is_ok());
        assert!(table.redeem_at(&presented(&ticket, &a), now).is_ok());
        // Used up: a limit, which is not counted as a forgery.
        let refused = table.redeem_at(&presented(&ticket, &a), now).unwrap_err();
        assert_eq!(
            refused,
            Refused {
                reason: Refusal::Exhausted,
                violation: false
            }
        );
        // Expired: checked against the clock.
        let fresh = table.mint(grant(5));
        let later = now + Duration::from_secs(61);
        let refused = table.redeem_at(&presented(&fresh, &a), later).unwrap_err();
        assert_eq!(refused.reason, Refusal::Expired);
        assert!(table.redeem_at(&presented(&fresh, &a), now).is_ok());
    }

    #[test]
    fn a_burst_of_invalid_presentations_from_one_host_is_a_violation_and_only_that_host() {
        let table = TicketTable::default();
        let a = owner("a", "token-a");
        let guess = Ticket("cwt.guess".to_string());
        let now = Instant::now();
        for count in 1..=INVALID_BURST {
            let refused = table.redeem_at(&presented(&guess, &a), now).unwrap_err();
            assert_eq!(
                refused.violation,
                count >= INVALID_BURST,
                "presentation {count}"
            );
        }
        // Another host process is counted on its own.
        let mut elsewhere = presented(&guess, &a);
        elsewhere.host_generation = 4;
        assert!(!table.redeem_at(&elsewhere, now).unwrap_err().violation);
        // The window: old presentations stop counting.
        let table = TicketTable::default();
        for _ in 0..INVALID_BURST - 1 {
            let _ = table.redeem_at(&presented(&guess, &a), now);
        }
        let later = now + INVALID_WINDOW;
        assert!(
            !table
                .redeem_at(&presented(&guess, &a), later)
                .unwrap_err()
                .violation
        );
        // Exhausting a valid ticket never counts toward a burst.
        let table = TicketTable::default();
        let ticket = table.mint(grant(1));
        assert!(table.redeem_at(&presented(&ticket, &a), now).is_ok());
        for _ in 0..INVALID_BURST * 2 {
            let refused = table.redeem_at(&presented(&ticket, &a), now).unwrap_err();
            assert!(!refused.violation);
        }
    }

    #[test]
    fn tickets_are_revoked_with_their_invocation_owner_and_host_and_never_printed() {
        let table = TicketTable::default();
        let a = owner("a", "token-a");
        let by_call = table.mint(grant(5));
        let by_owner = table.mint(grant(5));
        let mut other_owner_grant = grant(5);
        other_owner_grant.owner = owner("b", "token-b");
        let other_owner = table.mint(other_owner_grant);
        let mut other_host_grant = grant(5);
        other_host_grant.host_generation = 9;
        other_host_grant.owner = owner("c", "token-c");
        let other_host = table.mint(other_host_grant);
        assert_eq!(table.live(), 4);

        table.revoke(&by_call);
        table.revoke(&by_call);
        assert_eq!(
            refusal_of(&table, &presented(&by_call, &a)),
            Refusal::Unknown
        );
        table.revoke_owner("a");
        assert_eq!(
            refusal_of(&table, &presented(&by_owner, &a)),
            Refusal::Unknown
        );
        assert_eq!(table.live(), 2, "only a's tickets went");
        table.revoke_host(HostTier::Plugin, 9);
        assert_eq!(table.live(), 1);
        let _ = (other_owner, other_host);

        // A ticket's Debug never shows it.
        let shown = format!("{:?}", Ticket("cwt.secret-id".to_string()));
        assert!(!shown.contains("secret"), "{shown}");
        // Two tickets are never the same, and are long random ids.
        let one = table.mint(grant(1));
        let two = table.mint(grant(1));
        assert_ne!(one.expose(), two.expose());
        assert!(one.expose().len() >= 64);
    }
}
