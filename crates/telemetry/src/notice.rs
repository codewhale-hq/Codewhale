//! The first-run notice copy.
//!
//! One string, owned by the crate that owns what is collected, so the TUI and
//! the CLI cannot drift into describing two different products. Every claim
//! below is checked against [`crate::event`] by a test: if the schema grows a
//! field this text does not cover, that test fails.
//!
//! Two properties of the wording are deliberate and load-bearing:
//!
//! 1. **Usage defaults on with durable opt-out.** Disclosure presentation
//!    records only that it was shown, never fictional human acceptance.
//! 2. **The red lines are stated as "not collected", not as "anonymized".**
//!    Sampling and hashing are not the same promise, and a notice that implies
//!    them when neither is true is worse than no notice.

/// Headline shown above [`NOTICE_BODY`].
pub const NOTICE_HEADLINE: &str = "Codewhale usage reporting";

/// The notice itself.
///
/// Wrapped at 72 columns so it renders unchanged in the native responsive
/// modal and remains readable in an 80-column terminal.
pub const NOTICE_BODY: &str = "\
Codewhale counts: which version you run, OS and CPU family, session
duration and outcome, and aggregate feature and error counters.

It also counts the providers and public catalog models you use, their
token totals, and how often each built-in tool runs.

It never collects your conversations, code, prompts, files, repo or
branch names, model content, or credentials — and it never sends a
per-turn or per-tool timeline of agent activity.

You are identified only by a random ID stored on this machine, replaced
every 90 days. Change your mind any time:
                              codewhale config set telemetry false

Full schema, field by field:  docs/TELEMETRY.md

Usage reporting is on by default. Codewhale and PostHog process these
counts when delivery is configured. No IP is collected.";

/// Concise disclosure for every armed runtime surface, including headless CLI.
pub const STARTUP_DISCLOSURE: &str = "Usage reporting is on by default: Codewhale and PostHog process aggregate version/platform, session, feature, error, model/token and built-in tool counts when delivery is configured. No content or IP. Turn off: codewhale config set telemetry false. Details: codewhale config telemetry";

/// Present the policy once per revision without recording human acceptance.
/// Failure to save the display marker only causes a later repeat disclosure.
/// The interactive TUI draws its own localized notice and records its own
/// presentation, so it never gets a stray stderr line before the first frame.
pub(crate) fn show_startup_disclosure(surface: crate::event::Surface) {
    if surface == crate::event::Surface::Tui {
        return;
    }
    let Ok(path) = codewhale_config::SetupState::path() else {
        return;
    };
    let Some(state) = crate::load_setup_state_for_decision_at(&path) else {
        return;
    };
    if state.telemetry_opted_out()
        || !state.needs_telemetry_notice(codewhale_config::TELEMETRY_NOTICE_VERSION)
    {
        return;
    }
    use std::io::Write;
    if writeln!(std::io::stderr().lock(), "{STARTUP_DISCLOSURE}").is_ok() {
        let _ = codewhale_config::SetupState::update_at(&path, |latest| {
            latest.record_telemetry_notice_shown(codewhale_config::TELEMETRY_NOTICE_VERSION);
        });
    }
}
