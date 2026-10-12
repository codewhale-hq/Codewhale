//! Real CLI/Engine denial boundary with a loopback-only mock provider.
#![cfg(unix)]

use serde_json::{Value, json};
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use tempfile::TempDir;
use wait_timeout::ChildExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const MODEL: &str = "shell-denial-fixture";
const COMMAND: &str = "printf fixture > denial-canary.txt";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn denied_bash_cannot_be_reached_through_task_search_and_start() {
    scenario("bash", true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn denied_bash_cannot_be_reached_through_tasks_gate_action() {
    scenario("tasks", true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unrestricted_task_search_and_start_still_executes() {
    scenario("bash", false).await;
}

#[derive(Clone)]
struct Script {
    calls: Arc<Vec<(&'static str, &'static str, Value)>>,
    count: Arc<AtomicUsize>,
}
impl Respond for Script {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        assert_eq!(request.body_json::<Value>().unwrap()["model"], MODEL);
        let sequence = self.count.fetch_add(1, Ordering::SeqCst);
        let (delta, finish) = match self.calls.get(sequence) {
            Some((id, tool, input)) => (
                json!({"tool_calls":[{"index":0,"id":id,"type":"function","function":{"name":tool,"arguments":input.to_string()}}]}),
                "tool_calls",
            ),
            None => (json!({"content":"PERMISSION_FIXTURE_FINISHED"}), "stop"),
        };
        let mut response = String::new();
        for (delta, finish) in [(delta, None), (json!({}), Some(finish))] {
            let chunk = json!({"id":"fixture","model":MODEL,"choices":[{"index":0,"delta":delta,"finish_reason":finish}]});
            response.push_str(&format!("data: {chunk}\n\n"));
        }
        response.push_str("data: [DONE]\n\n");
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(response)
    }
}

async fn scenario(tool: &'static str, deny: bool) {
    let workspace = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    let mut calls = Vec::new();
    if deny {
        calls.push(("direct", "Bash", json!({"command":COMMAND})));
    }
    calls.push((
        "search",
        "tool_search",
        json!({"query":tool, "max_results":8}),
    ));
    let input = if tool == "tasks" {
        json!({"action":"gate_run", "gate":"custom", "command":COMMAND})
    } else {
        json!({"command":COMMAND})
    };
    calls.push(("route", tool, input.clone()));
    // Repeat the attempted call even when catalog shaping hid it. A guessed
    // name, a cached schema, and deferred hydration must not grant execution.
    calls.push(("route-retry", tool, input));
    let expected_calls = calls.len() + 1;
    let count = Arc::new(AtomicUsize::new(0));
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(Script {
            calls: Arc::new(calls),
            count: count.clone(),
        })
        .mount(&server)
        .await;
    let config = home.path().join(".codewhale/config.toml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(
        &config,
        "allow_shell = true\ntelemetry = false\n[retry]\nenabled = false\n",
    )
    .unwrap();
    let mut command = Command::new(crate::binary::codewhale());
    command.env_clear();
    for key in ["PATH", "LANG", "TMPDIR"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
        .current_dir(workspace.path())
        .arg("--workspace")
        .arg(workspace.path())
        .args([
            "--no-project-config",
            "exec",
            "--auto",
            "--provider",
            "deepseek",
            "--model",
            MODEL,
            "--output-format",
            "stream-json",
        ])
        .env("HOME", home.path())
        .env("CODEWHALE_HOME", config.parent().unwrap())
        .env("CODEWHALE_CONFIG_PATH", &config)
        .env("DEEPSEEK_API_KEY", "fixture-key-not-real")
        .env("DEEPSEEK_BASE_URL", server.uri())
        .env("CODEWHALE_BASE_URL", server.uri())
        .env("DEEPSEEK_MODEL", MODEL)
        .env("CODEWHALE_MODEL", MODEL)
        .env("CODEWHALE_TELEMETRY", "0")
        .env("RUST_LOG", "warn")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if deny {
        command.args(["--disallowed-tools", "Bash"]);
    }
    command.arg("Execute the local permission fixture.");
    let mut child = command.spawn().unwrap();
    let drain = |mut pipe: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            pipe.read_to_end(&mut bytes).unwrap();
            bytes
        })
    };
    let stdout = drain(Box::new(child.stdout.take().unwrap()));
    let stderr = drain(Box::new(child.stderr.take().unwrap()));
    let status = child
        .wait_timeout(Duration::from_secs(60))
        .unwrap()
        .unwrap_or_else(|| {
            child.kill().ok();
            child.wait().ok();
            panic!("permission fixture timed out")
        });
    let stdout = String::from_utf8_lossy(&stdout.join().unwrap()).into_owned();
    let stderr = String::from_utf8_lossy(&stderr.join().unwrap()).into_owned();
    assert!(status.success(), "{stdout}\n{stderr}");
    assert_eq!(count.load(Ordering::SeqCst), expected_calls);
    assert_eq!(
        workspace.path().join("denial-canary.txt").exists(),
        !deny,
        "{stdout}\n{stderr}"
    );
    let requests = server.received_requests().await.unwrap();
    let final_request: Value = requests.last().unwrap().body_json().unwrap();
    let results: Vec<_> = final_request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "tool")
        .collect();
    if deny {
        for id in ["direct", "route", "route-retry"] {
            let receipt = results
                .iter()
                .find(|message| message["tool_call_id"] == id)
                .unwrap();
            assert!(
                receipt["content"]
                    .as_str()
                    .unwrap()
                    .contains("disallowed-tools"),
                "{receipt}"
            );
        }
    } else {
        assert!(results.iter().any(|message| {
            message["tool_call_id"] == "route-retry"
                && !message["content"]
                    .as_str()
                    .unwrap()
                    .contains("disallowed-tools")
        }));
    }
}
