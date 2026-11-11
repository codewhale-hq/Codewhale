#[path = "support/acp_conformance.rs"]
mod conformance;

use codewhale_engine::fleet::acp::{AcpClient, ControlWrite, PermissionPolicy};
use codewhale_engine::fleet::host::{
    FleetHostAdapter, FleetWorkerCommand, FleetWorkerStartRequest, FleetWorkerTransport,
    LocalProcessFleetHostAdapter,
};
use conformance::{Allowed, Conformance, Side};
use std::path::{Path, PathBuf};
use std::time::Duration;

const KNOWN: &[Allowed] = &[
    Allowed {
        side: Side::Client,
        frame: "session/set_model request",
        problem: "`session/set_model` is not a client request in official v1",
        reason: "compatibility, kept: sent only to an agent that offers the pre-v1 `models` object and no `model` config option",
    },
    Allowed {
        side: Side::Agent,
        frame: "session/new response",
        problem: "field `.models` is not in the official type",
        reason: "the fixture's legacy leg offers the pre-v1 `models` object on purpose, to make the client send session/set_model",
    },
    Allowed {
        side: Side::Agent,
        frame: "initialize response",
        problem: "field `.agentCapabilities.sessionCapabilities.fork` is not in the official type",
        reason: "options fixture advertises session/fork, which is unstable_session_fork, not stable v1",
    },
    Allowed {
        side: Side::Agent,
        frame: "session/prompt response",
        problem: "field `.usage` is not in the official type",
        reason: "options fixture reports PromptResponse.usage, which is unstable_end_turn_token_usage, not stable v1",
    },
];

const STEP: Duration = Duration::from_secs(20);

fn fixture(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
        .display()
        .to_string()
}

fn captured(
    agent: &str,
    workspace: &Path,
    log: &Path,
    drive: impl FnOnce(&mut AcpClient),
) -> Vec<(Side, serde_json::Value)> {
    let mut host = LocalProcessFleetHostAdapter::new(workspace);
    let mut request = FleetWorkerStartRequest::new(
        "w-acp-conformance",
        FleetWorkerCommand::new(
            "node",
            [
                fixture("acp-tee.mjs"),
                log.display().to_string(),
                fixture(agent),
            ],
        ),
    );
    request.transport = FleetWorkerTransport::JsonRpc;
    request.cwd = Some(workspace.to_path_buf());
    host.start_worker(request).unwrap();
    let mut client = AcpClient::new(host.take_worker_io("w-acp-conformance").unwrap());
    drive(&mut client);
    host.stop_worker("w-acp-conformance").unwrap();
    std::fs::read_to_string(log)
        .unwrap()
        .lines()
        .map(|line| {
            let (tag, frame) = line.split_once(' ').unwrap();
            let side = if tag == "C" {
                Side::Client
            } else {
                Side::Agent
            };
            (side, serde_json::from_str(frame).unwrap())
        })
        .collect()
}

#[test]
fn the_acp_client_and_its_fixture_agents_speak_official_v1_shapes() {
    let has_node = std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success());
    if !has_node {
        eprintln!("skipped: node is not installed");
        return;
    }
    let capture = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let mut frames = captured(
        "acp-agent.mjs",
        workspace.path(),
        &capture.path().join("agent.log"),
        |client| {
            let identity = client.initialize(STEP).unwrap();
            let session = client
                .new_session(&identity, workspace.path(), &[], STEP)
                .unwrap()
                .session_id;
            client
                .set_config_option(&session, "model", "fixture-model", STEP)
                .unwrap();
            for policy in [
                PermissionPolicy::AllowOnce,
                PermissionPolicy::RejectOnce,
                PermissionPolicy::Cancel,
            ] {
                client.prompt(&session, "conform", policy, STEP).unwrap();
            }
            client.cancel(&session).unwrap();
            client
                .reopen_session(&identity, &session, workspace.path(), &[], STEP)
                .unwrap();
        },
    );
    let options_workspace = tempfile::tempdir().unwrap();
    frames.extend(captured(
        "acp-options-agent.mjs",
        options_workspace.path(),
        &capture.path().join("options.log"),
        |client| {
            let identity = client.initialize(STEP).unwrap();
            let created = client
                .new_session(&identity, options_workspace.path(), &[], STEP)
                .unwrap();
            let mut applied = 0;
            for control in &created.offer.controls {
                if control.write.is_none() || control.unavailable.is_some() {
                    continue;
                }
                if let Some(choice) = control.choices.last() {
                    client
                        .apply_control(&created.session_id, control, &choice.value, STEP)
                        .unwrap();
                    applied += 1;
                }
            }
            assert!(
                applied >= 2,
                "options fixture offered {applied} writable controls"
            );
            client
                .prompt(
                    &created.session_id,
                    "conform",
                    PermissionPolicy::AllowOnce,
                    STEP,
                )
                .unwrap();
        },
    ));

    let legacy_workspace = tempfile::tempdir().unwrap();
    std::fs::write(
        legacy_workspace.path().join("fixture-plan.json"),
        r#"{"legacyModels":true}"#,
    )
    .unwrap();
    frames.extend(captured(
        "acp-agent.mjs",
        legacy_workspace.path(),
        &capture.path().join("legacy.log"),
        |client| {
            let identity = client.initialize(STEP).unwrap();
            let created = client
                .new_session(&identity, legacy_workspace.path(), &[], STEP)
                .unwrap();
            let model = created.offer.control("model").unwrap();
            assert_eq!(model.write, Some(ControlWrite::SetModel));
            client
                .apply_control(&created.session_id, model, "legacy-b", STEP)
                .unwrap();
        },
    ));

    let mut observed = Conformance::checking(&[Side::Client, Side::Agent]);
    for (side, frame) in &frames {
        observed.observe(*side, frame);
    }
    assert!(
        observed.frames() > 30,
        "only {} frames observed",
        observed.frames()
    );
    observed.assert_only(KNOWN);
}
