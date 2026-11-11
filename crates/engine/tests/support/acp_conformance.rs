use agent_client_protocol_schema::v1 as acp;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Side {
    Client,
    Agent,
}

impl Side {
    fn name(self) -> &'static str {
        match self {
            Side::Client => "client",
            Side::Agent => "agent",
        }
    }
    fn peer(self) -> Side {
        match self {
            Side::Client => Side::Agent,
            Side::Agent => Side::Client,
        }
    }
}

pub struct Allowed {
    pub side: Side,
    pub frame: &'static str,
    pub problem: &'static str,
    pub reason: &'static str,
}

pub struct Conformance {
    checked: Vec<Side>,
    pending: HashMap<(Side, String), String>,
    findings: BTreeSet<(Side, String, String)>,
    frames: usize,
}

fn check<T: DeserializeOwned + Serialize>(value: &Value) -> Vec<String> {
    let typed: T = match serde_json::from_value(value.clone()) {
        Ok(typed) => typed,
        Err(error) => {
            let text = error.to_string();
            let text = text.split(" at line ").next().unwrap_or(&text);
            return vec![format!("not a valid {}: {text}", short_type::<T>())];
        }
    };
    let back = match serde_json::to_value(&typed) {
        Ok(back) => back,
        Err(error) => return vec![format!("does not re-serialize: {error}")],
    };
    let mut problems = Vec::new();
    diff("", value, &back, &mut problems);
    problems
}

fn short_type<T>() -> &'static str {
    let name = std::any::type_name::<T>();
    name.rsplit("::").next().unwrap_or(name)
}

fn skipped_default(key: &str, value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => key == "env" && map.is_empty(),
        Value::String(text) => matches!(
            (key, text.as_str()),
            ("kind", "other") | ("status", "pending")
        ),
        _ => false,
    }
}

fn diff(path: &str, ours: &Value, official: &Value, problems: &mut Vec<String>) {
    match (ours, official) {
        (Value::Object(ours), Value::Object(official)) => {
            for (key, value) in ours {
                if key == "_meta" {
                    continue;
                }
                let at = format!("{path}.{key}");
                match official.get(key) {
                    Some(back) => diff(&at, value, back, problems),
                    None if skipped_default(key, value) => {}
                    None => problems.push(format!("field `{at}` is not in the official type")),
                }
            }
        }
        (Value::Array(ours), Value::Array(official)) if ours.len() == official.len() => {
            for (value, back) in ours.iter().zip(official) {
                diff(&format!("{path}[]"), value, back, problems);
            }
        }
        (Value::Number(a), Value::Number(b)) if a.as_f64() == b.as_f64() => {}
        _ if ours == official => {}
        _ => problems.push(format!(
            "value at `{}` reads back as {official} instead of {ours}",
            if path.is_empty() { "." } else { path }
        )),
    }
}

fn request_params(side: Side, method: &str, params: &Value) -> Option<Vec<String>> {
    Some(match (side, method) {
        (Side::Client, "initialize") => check::<acp::InitializeRequest>(params),
        (Side::Client, "authenticate") => check::<acp::AuthenticateRequest>(params),
        (Side::Client, "session/new") => check::<acp::NewSessionRequest>(params),
        (Side::Client, "session/load") => check::<acp::LoadSessionRequest>(params),
        (Side::Client, "session/resume") => check::<acp::ResumeSessionRequest>(params),
        (Side::Client, "session/list") => check::<acp::ListSessionsRequest>(params),
        (Side::Client, "session/close") => check::<acp::CloseSessionRequest>(params),
        (Side::Client, "session/delete") => check::<acp::DeleteSessionRequest>(params),
        (Side::Client, "session/set_mode") => check::<acp::SetSessionModeRequest>(params),
        (Side::Client, "session/set_config_option") => {
            check::<acp::SetSessionConfigOptionRequest>(params)
        }
        (Side::Client, "session/prompt") => check::<acp::PromptRequest>(params),
        (Side::Agent, "session/request_permission") => {
            check::<acp::RequestPermissionRequest>(params)
        }
        (Side::Agent, "fs/read_text_file") => check::<acp::ReadTextFileRequest>(params),
        (Side::Agent, "fs/write_text_file") => check::<acp::WriteTextFileRequest>(params),
        (Side::Agent, "terminal/create") => check::<acp::CreateTerminalRequest>(params),
        (Side::Agent, "terminal/output") => check::<acp::TerminalOutputRequest>(params),
        (Side::Agent, "terminal/release") => check::<acp::ReleaseTerminalRequest>(params),
        (Side::Agent, "terminal/wait_for_exit") => check::<acp::WaitForTerminalExitRequest>(params),
        (Side::Agent, "terminal/kill") => check::<acp::KillTerminalRequest>(params),
        (Side::Agent, "elicitation/create") => check::<acp::CreateElicitationRequest>(params),
        _ => return None,
    })
}

fn response_result(method: &str, result: &Value) -> Option<Vec<String>> {
    Some(match method {
        "initialize" => check::<acp::InitializeResponse>(result),
        "authenticate" => check::<acp::AuthenticateResponse>(result),
        "session/new" => check::<acp::NewSessionResponse>(result),
        "session/load" => check::<acp::LoadSessionResponse>(result),
        "session/resume" => check::<acp::ResumeSessionResponse>(result),
        "session/list" => check::<acp::ListSessionsResponse>(result),
        "session/close" => check::<acp::CloseSessionResponse>(result),
        "session/delete" => check::<acp::DeleteSessionResponse>(result),
        "session/set_mode" => check::<acp::SetSessionModeResponse>(result),
        "session/set_config_option" => check::<acp::SetSessionConfigOptionResponse>(result),
        "session/prompt" => check::<acp::PromptResponse>(result),
        "session/request_permission" => check::<acp::RequestPermissionResponse>(result),
        "fs/read_text_file" => check::<acp::ReadTextFileResponse>(result),
        "fs/write_text_file" => check::<acp::WriteTextFileResponse>(result),
        "terminal/create" => check::<acp::CreateTerminalResponse>(result),
        "terminal/output" => check::<acp::TerminalOutputResponse>(result),
        "terminal/release" => check::<acp::ReleaseTerminalResponse>(result),
        "terminal/wait_for_exit" => check::<acp::WaitForTerminalExitResponse>(result),
        "terminal/kill" => check::<acp::KillTerminalResponse>(result),
        "elicitation/create" => check::<acp::CreateElicitationResponse>(result),
        _ => return None,
    })
}

fn notification_params(side: Side, method: &str, params: &Value) -> Option<Vec<String>> {
    Some(match (side, method) {
        (_, "$/cancel_request") => check::<acp::CancelRequestNotification>(params),
        (Side::Client, "session/cancel") => check::<acp::CancelNotification>(params),
        (Side::Agent, "session/update") => check::<acp::SessionNotification>(params),
        (Side::Agent, "elicitation/complete") => {
            check::<acp::CompleteElicitationNotification>(params)
        }
        _ => return None,
    })
}

fn envelope(frame: &Value, allowed: &[&str]) -> Vec<String> {
    let mut problems = Vec::new();
    if frame.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        problems.push("envelope lacks `jsonrpc: \"2.0\"`".to_string());
    }
    if let Some(map) = frame.as_object() {
        for key in map.keys() {
            if !allowed.contains(&key.as_str()) && key != "jsonrpc" {
                problems.push(format!("envelope carries `{key}`"));
            }
        }
    }
    problems
}

impl Conformance {
    pub fn checking(sides: &[Side]) -> Self {
        Self {
            checked: sides.to_vec(),
            pending: HashMap::new(),
            findings: BTreeSet::new(),
            frames: 0,
        }
    }

    pub fn observe(&mut self, side: Side, frame: &Value) {
        self.frames += 1;
        let method = frame.get("method").and_then(Value::as_str);
        let id = frame.get("id");
        let params = frame.get("params").cloned().unwrap_or(Value::Null);
        let (label, mut problems) = match (method, id) {
            (Some(method), Some(id)) => {
                self.pending
                    .insert((side, id.to_string()), method.to_string());
                let mut problems = envelope(frame, &["id", "method", "params"]);
                match request_params(side, method, &params) {
                    Some(found) => problems.extend(found),
                    None if method.starts_with('_') => {}
                    None => problems.push(format!(
                        "`{method}` is not a {} request in official v1",
                        side.name()
                    )),
                }
                (format!("{method} request"), problems)
            }
            (Some(method), None) => {
                let mut problems = envelope(frame, &["method", "params"]);
                match notification_params(side, method, &params) {
                    Some(found) => problems.extend(found),
                    None if method.starts_with('_') => {}
                    None => problems.push(format!(
                        "`{method}` is not a {} notification in official v1",
                        side.name()
                    )),
                }
                (format!("{method} notification"), problems)
            }
            (None, Some(id)) => {
                let answered = self.pending.remove(&(side.peer(), id.to_string()));
                let label = match &answered {
                    Some(method) => format!("{method} response"),
                    None => "response".to_string(),
                };
                let mut problems = Vec::new();
                if let Some(error) = frame.get("error") {
                    problems.extend(envelope(frame, &["id", "error"]));
                    problems.extend(check::<acp::Error>(error));
                } else if let Some(result) = frame.get("result") {
                    problems.extend(envelope(frame, &["id", "result"]));
                    if let Some(found) = answered
                        .as_deref()
                        .and_then(|method| response_result(method, result))
                    {
                        problems.extend(found);
                    }
                } else {
                    problems.push("response has neither result nor error".to_string());
                }
                if answered.is_none() {
                    problems.push(format!("answers no outstanding request (id {id})"));
                }
                (label, problems)
            }
            (None, None) => (
                "frame".to_string(),
                vec!["neither a request, a notification nor a response".to_string()],
            ),
        };
        let label = match frame
            .pointer("/params/update/sessionUpdate")
            .and_then(Value::as_str)
        {
            Some(kind) => format!("{label} ({kind})"),
            None => label,
        };
        if self.checked.contains(&side) {
            for problem in problems.drain(..) {
                self.findings.insert((side, label.clone(), problem));
            }
        }
    }

    pub fn assert_only(&self, allowed: &[Allowed]) {
        assert!(self.frames > 0, "no frames were observed");
        let mut used = vec![false; allowed.len()];
        let mut unexpected = Vec::new();
        for (side, frame, problem) in &self.findings {
            match allowed.iter().position(|entry| {
                entry.side == *side && entry.frame == frame && problem.contains(entry.problem)
            }) {
                Some(index) => used[index] = true,
                None => unexpected.push(format!("{} {frame}: {problem}", side.name())),
            }
        }
        let stale: Vec<_> = allowed
            .iter()
            .zip(&used)
            .filter(|(_, used)| !**used)
            .map(|(entry, _)| {
                format!(
                    "{} {}: {} ({})",
                    entry.side.name(),
                    entry.frame,
                    entry.problem,
                    entry.reason
                )
            })
            .collect();
        assert!(
            unexpected.is_empty() && stale.is_empty(),
            "ACP frames differ from the official v1 schema.\nnew deviations:\n  {}\nallowlisted deviations no longer seen (remove them):\n  {}",
            unexpected.join("\n  "),
            stale.join("\n  ")
        );
    }

    pub fn frames(&self) -> usize {
        self.frames
    }
}
