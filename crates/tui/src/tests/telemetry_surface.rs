use super::*;
use clap::Parser;
use codewhale_telemetry::{SessionSource, Surface};

fn command_of(args: &[&str]) -> Option<Commands> {
    Cli::try_parse_from(args)
        .expect("CLI args should parse")
        .command
}

#[test]
fn every_surface_is_named_by_the_subcommand_not_the_executable() {
    assert_eq!(telemetry_surface(None), Surface::Tui);
    assert_eq!(
        telemetry_surface(command_of(&["codewhale-tui", "resume", "--last"]).as_ref()),
        Surface::Tui
    );
    assert_eq!(
        telemetry_surface(command_of(&["codewhale-tui", "fork", "--last"]).as_ref()),
        Surface::Tui
    );
    assert_eq!(
        telemetry_surface(command_of(&["codewhale-tui", "pr", "42"]).as_ref()),
        Surface::Tui
    );
    assert_eq!(
        telemetry_surface(command_of(&["codewhale-tui", "exec", "hello"]).as_ref()),
        Surface::Exec
    );
    assert_eq!(
        telemetry_surface(command_of(&["codewhale-tui", "serve", "--http"]).as_ref()),
        Surface::Serve
    );
    assert_eq!(
        telemetry_surface(command_of(&["codewhale-tui", "serve", "--mcp"]).as_ref()),
        Surface::McpServer
    );
    assert_eq!(
        telemetry_surface(command_of(&["codewhale-tui", "doctor"]).as_ref()),
        Surface::Cli
    );
}

#[test]
fn an_embedder_may_name_the_server_it_started() {
    // The one surface a third party legitimately starts, and the one they may
    // therefore declare. The editor extension's case.
    assert_eq!(
        embedded_surface_override_from(Some("vscode-extension".to_string())),
        Some(Surface::VscodeExtension)
    );
    assert_eq!(
        embedded_surface_override_from(Some("desktop".to_string())),
        Some(Surface::Desktop)
    );
    // Surrounding whitespace is a shell artifact, not a different name.
    assert_eq!(
        embedded_surface_override_from(Some(" vscode-extension\n".to_string())),
        Some(Surface::VscodeExtension)
    );
    assert_eq!(
        embedded_surface_override_from(None),
        None,
        "an unset variable declares nothing"
    );
}

#[test]
fn the_interactive_surface_can_never_be_declared_by_an_embedder() {
    // Nothing starts the terminal UI on a user's behalf: a server reporting
    // itself as `tui` would land in the one surface whose numbers are the
    // terminal's.
    assert_eq!(
        embedded_surface_override_from(Some("tui".to_string())),
        None
    );
    assert_eq!(
        embedded_surface_override_from(Some(" tui ".to_string())),
        None
    );
}

#[test]
fn an_unknown_surface_name_is_dropped_rather_than_guessed_at() {
    // Not an error and not a repair: a name outside the closed set falls back
    // to the caller's default. `vscode` and `ide` are the likely typos and are
    // deliberately not aliases of anything.
    for name in [
        "",
        "  ",
        "vscode",
        "ide",
        "TUI",
        "VSCODE-EXTENSION",
        "unknown",
    ] {
        assert_eq!(
            embedded_surface_override_from(Some(name.to_string())),
            None,
            "{name:?} must not resolve to a surface"
        );
    }
}

#[test]
fn the_declared_surface_is_consulted_for_the_server_and_nowhere_else() {
    // The variable is inherited by descendants, and an agent's shell command
    // runs codewhale descendants with this environment. Only the branch that
    // *is* an embedder's server may consult it, so a nested `exec` cannot
    // report itself as the embedder.
    let declared = Some(Surface::VscodeExtension);

    assert_eq!(telemetry_surface_with(None, declared), Surface::Tui);
    assert_eq!(
        telemetry_surface_with(
            command_of(&["codewhale-tui", "exec", "hello"]).as_ref(),
            declared
        ),
        Surface::Exec
    );
    assert_eq!(
        telemetry_surface_with(command_of(&["codewhale-tui", "doctor"]).as_ref(), declared),
        Surface::Cli
    );
    // An MCP server still says so; it is decided before the declaration.
    assert_eq!(
        telemetry_surface_with(
            command_of(&["codewhale-tui", "serve", "--mcp"]).as_ref(),
            declared
        ),
        Surface::McpServer
    );
    // Only the plain server picks the declared name up…
    assert_eq!(
        telemetry_surface_with(
            command_of(&["codewhale-tui", "serve", "--http"]).as_ref(),
            declared
        ),
        Surface::VscodeExtension
    );
    // …and with nothing declared it is an anonymous server, exactly as before.
    assert_eq!(
        telemetry_surface_with(
            command_of(&["codewhale-tui", "serve", "--http"]).as_ref(),
            None
        ),
        Surface::Serve
    );
}

#[test]
fn read_only_commands_never_arm_usage_counting() {
    for args in [
        vec!["codewhale-tui", "doctor"],
        vec!["codewhale-tui", "doctor", "--json"],
        vec!["codewhale-tui", "session-diagnostics", "session.jsonl"],
        vec!["codewhale-tui", "sessions"],
        vec!["codewhale-tui", "setup", "--status"],
    ] {
        let command = command_of(&args);
        assert!(
            telemetry_command_is_read_only(command.as_ref()),
            "{args:?} must remain state-free"
        );
    }

    for args in [
        vec!["codewhale-tui", "exec", "hello"],
        vec!["codewhale-tui", "setup", "--skills"],
    ] {
        let command = command_of(&args);
        assert!(
            !telemetry_command_is_read_only(command.as_ref()),
            "{args:?} is not a read-only command"
        );
    }
}

#[test]
fn windows_resume_session_listing_cannot_consume_the_process_telemetry_arm() {
    // Bare `codewhale resume` on Windows invokes `sessions` in-process before
    // starting the selected resumed TUI. The listing must remain state-free so
    // that the process-global telemetry initializer is still available to the
    // resumed TUI after its native privacy decision.
    let command = command_of(&["codewhale-tui", "sessions"]);
    assert_eq!(telemetry_surface(command.as_ref()), Surface::Cli);
    assert!(telemetry_command_is_read_only(command.as_ref()));
}

#[test]
fn unreadable_existing_setup_state_suppresses_telemetry_instead_of_defaulting_on() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state_path = dir.path().join("setup_state.json");
    std::fs::write(&state_path, "not-json").expect("seed corrupt state");

    assert!(codewhale_telemetry::load_setup_state_for_decision_at(&state_path).is_none());
    assert_eq!(
        std::fs::read_to_string(state_path).expect("corrupt state remains untouched"),
        "not-json"
    );
}

#[test]
fn the_session_source_distinguishes_resume_and_fork_from_a_fresh_launch() {
    assert_eq!(telemetry_session_source(None), SessionSource::Interactive);
    assert_eq!(
        telemetry_session_source(command_of(&["codewhale-tui", "resume", "--last"]).as_ref()),
        SessionSource::Resume
    );
    assert_eq!(
        telemetry_session_source(command_of(&["codewhale-tui", "fork", "--last"]).as_ref()),
        SessionSource::Fork
    );
    assert_eq!(
        telemetry_session_source(command_of(&["codewhale-tui", "pr", "42"]).as_ref()),
        SessionSource::Interactive
    );
    assert_eq!(
        telemetry_session_source(command_of(&["codewhale-tui", "serve", "--http"]).as_ref()),
        SessionSource::Api
    );
    assert_eq!(
        telemetry_session_source(command_of(&["codewhale-tui", "doctor"]).as_ref()),
        SessionSource::Unknown
    );
}

#[test]
fn a_session_end_built_without_arming_writes_nothing() {
    let event = telemetry_session_end();
    assert!(matches!(
        event,
        codewhale_telemetry::Event::SessionEnd { .. }
    ));
    codewhale_telemetry::record_blocking(event);
    assert!(!codewhale_telemetry::is_armed());
}

#[test]
fn canceled_run_reports_exit_class_error_not_signal() {
    use codewhale_engine::core::termination::RunTerminationReason;
    assert_eq!(RunTerminationReason::Canceled.process_exit_code(), 130);
    assert_eq!(
        codewhale_telemetry::ExitClass::Signal.as_str(),
        "signal",
        "the SIGINT path's class is a distinct value, not a synonym for error"
    );
    assert!(!RunTerminationReason::Canceled.is_success());
    assert!(RunTerminationReason::Resolved.is_success());
    assert!(!codewhale_telemetry::is_armed());
    codewhale_telemetry::set_exit_class(codewhale_telemetry::ExitClass::Error);
    assert_eq!(
        codewhale_telemetry::exit_class(),
        codewhale_telemetry::ExitClass::Clean
    );
}
