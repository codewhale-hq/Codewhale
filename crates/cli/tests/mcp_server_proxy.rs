//! The retired aggregation proxy's CLI spelling delegates to the native server.
//! Exercise both real binary entrypoints with isolated config; never launch or
//! rewrite legacy child-server definitions. The active MCP client is separate.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::{Value, json};
use tempfile::TempDir;

struct Fixture {
    _root: TempDir,
    home: PathBuf,
}

impl Fixture {
    /// Seal HOME before anything writes config. The suite has written to the
    /// real `~/.codewhale/config.toml` before (#4831); this test must never be
    /// the one that does it again.
    fn new() -> Self {
        let root = TempDir::new().expect("fixture root");
        let home = root.path().join("sealed-home");
        fs::create_dir_all(home.join(".codewhale")).expect("sealed config dir");
        fs::write(home.join(".codewhale").join("config.toml"), "").expect("seed config");
        Self { _root: root, home }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(codewhale_binary());
        command
            .current_dir(&self.home)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("CODEWHALE_HOME", self.home.join(".codewhale"))
            .env("CODEWHALE_SECRET_BACKEND", "file");
        command
    }

    fn configure_servers(&self, definitions: Value) {
        let output = self
            .command()
            .args(["config", "set", "mcp.server_definitions"])
            .arg(definitions.to_string())
            .output()
            .expect("run config set");
        assert!(
            output.status.success(),
            "config set failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Drive `codewhale mcp-server` over stdio with `requests`, returning the
    /// parsed JSON-RPC responses plus stderr.
    fn run_mcp_server(&self, args: &[&str], requests: &[Value]) -> (Vec<Value>, String) {
        let mut child = self
            .command()
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn codewhale mcp-server");

        let mut stdin = child.stdin.take().expect("mcp-server stdin");
        let mut stdout = BufReader::new(child.stdout.take().expect("mcp-server stdout"));
        let mut responses = Vec::new();
        for request in requests {
            writeln!(stdin, "{request}").expect("write request");
            stdin.flush().expect("flush request");
            if let Some(id) = request.get("id") {
                let mut line = String::new();
                assert!(stdout.read_line(&mut line).expect("read response") > 0);
                let response: Value = serde_json::from_str(&line).expect("stdout is only JSON-RPC");
                assert_eq!(response.get("id"), Some(id));
                responses.push(response);
            }
        }
        drop(stdin);
        let mut remainder = String::new();
        stdout
            .read_to_string(&mut remainder)
            .expect("read remaining output");
        assert!(
            remainder.trim().is_empty(),
            "unexpected response: {remainder}"
        );
        let output = child.wait_with_output().expect("mcp-server output");
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        assert!(
            output.status.success(),
            "native MCP server failed: {stderr}"
        );
        (responses, stderr)
    }
}

fn codewhale_binary() -> PathBuf {
    if let Some(path) = option_env!("CARGO_BIN_EXE_codewhale") {
        return PathBuf::from(path);
    }
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_codewhale") {
        return PathBuf::from(path);
    }
    let mut path = std::env::current_exe().expect("current test executable path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join("codewhale")
}

fn initialize() -> Value {
    json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
        "protocolVersion": "2024-11-05",
        "clientInfo": {"name": "native-alias-test", "version": "1"},
        "capabilities": {}
    }})
}

#[test]
fn both_cli_spellings_expose_the_same_native_read_only_tools() {
    let fixture = Fixture::new();
    let requests = [
        initialize(),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "ping"}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
            "name": "file_write", "approved": true,
            "arguments": {"path": "forbidden.txt", "content": "must not be written"}
        }}),
    ];
    let (alias, _) = fixture.run_mcp_server(&["mcp-server"], &requests);
    let (native, _) = fixture.run_mcp_server(&["serve", "--mcp"], &requests);
    assert_eq!(alias, native);
    assert_eq!(alias.len(), 4);
    assert_eq!(
        alias[0]["result"]["serverInfo"]["name"],
        "codewhale-mcp-server"
    );
    assert_eq!(alias[0]["result"]["protocolVersion"], "2024-11-05");
    let names: Vec<&str> = alias[1]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["file_read", "search"]);
    assert_eq!(alias[2]["result"], json!({}));
    assert_eq!(alias[3]["error"]["code"], -32602);
    assert!(!fixture.home.join("forbidden.txt").exists());
}

#[test]
fn alias_neither_launches_nor_rewrites_legacy_child_server_definitions() {
    let fixture = Fixture::new();
    let marker = fixture.home.join("legacy-child-was-launched");
    // Even an invalid executable would produce a visible startup failure in
    // the old proxy. The native server must never attempt this launch.
    fixture.configure_servers(json!([{"config": {
        "name": "retired-proxy", "command": marker,
        "env": {"PRIVATE_TOKEN": "test-only-sentinel"}
    }}]));
    let config = fixture.home.join(".codewhale/config.toml");
    let before = fs::read(&config).unwrap();
    let (responses, stderr) = fixture.run_mcp_server(
        &["mcp-server"],
        &[
            initialize(),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
        ],
    );
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[1]["result"]["tools"].as_array().unwrap().len(), 2);
    assert_eq!(
        fs::read(config).unwrap(),
        before,
        "saved legacy data is untouched"
    );
    assert!(!stderr.contains("retired-proxy"), "{stderr}");
    assert!(!stderr.contains("PRIVATE_TOKEN"), "{stderr}");
    assert!(!stderr.contains("test-only-sentinel"), "{stderr}");
    assert!(!marker.exists());
}
