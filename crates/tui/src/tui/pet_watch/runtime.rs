use super::live::{Batch, Client, Scene, session_source};
use codewhale_engine::runtime_threads::RuntimeEventRecord;
use codewhale_protocol::engine_owner::{OwnerActivityKind, OwnerOperationOutcome};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use tokio::sync::broadcast;

struct Observed {
    thread: String,
    turn: String,
    item: Option<String>,
    event: String,
    fields: Value,
}

pub fn observe_runtime(mut events: broadcast::Receiver<RuntimeEventRecord>) {
    let (sender, receiver) = mpsc::sync_channel(128);
    let gap = Arc::new(AtomicBool::new(false));
    let worker_gap = gap.clone();
    if let Err(error) = std::thread::Builder::new()
        .name("pet-runtime-producer".into())
        .spawn(move || run(receiver, worker_gap))
    {
        tracing::warn!(%error, "Pet Runtime projection could not start");
        return;
    }
    tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(record) => match extract(record) {
                    Ok(Some(event)) => match sender.try_send(event) {
                        Ok(()) => {}
                        Err(mpsc::TrySendError::Full(_)) => gap.store(true, Ordering::Release),
                        Err(mpsc::TrySendError::Disconnected(_)) => break,
                    },
                    Ok(None) => {}
                    Err(()) => gap.store(true, Ordering::Release),
                },
                Err(broadcast::error::RecvError::Lagged(_)) => gap.store(true, Ordering::Release),
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

fn identifier(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|value| {
            !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
        })
        .map(str::to_string)
}

fn extract(record: RuntimeEventRecord) -> Result<Option<Observed>, ()> {
    if record.event == codewhale_engine::runtime_threads::RUNTIME_STORE_FAILURE_EVENT {
        return Err(());
    }
    if !matches!(
        record.event.as_str(),
        "turn.started"
            | "turn.completed"
            | "item.started"
            | "item.completed"
            | "item.delta"
            | "operation.activity_started"
            | "operation.activity_completed"
            | "operation.heartbeat"
            | "agent.spawned"
            | "agent.progress"
            | "agent.completed"
            | "approval.required"
            | "approval.decided"
            | "approval.timeout"
            | "user_input.required"
            | "user_input.answered"
            | "user_input.canceled"
    ) {
        return Ok(None);
    }
    let Some(turn) = record.turn_id else {
        return Ok(None);
    };
    if record.thread_id.is_empty()
        || record.thread_id.len() > 256
        || record.thread_id.chars().any(char::is_control)
        || turn.is_empty()
        || turn.len() > 256
        || turn.chars().any(char::is_control)
    {
        return Err(());
    }
    let payload = record.payload;
    let mut fields = json!({});
    for key in ["span_id", "action_id", "agent_id", "approval_id", "id"] {
        if let Some(value) = identifier(&payload[key]) {
            fields[key] = json!(value);
        }
    }
    for (key, value) in [
        (
            "kind",
            payload
                .get("kind")
                .or_else(|| payload.get("item").and_then(|item| item.get("kind"))),
        ),
        (
            "status",
            payload.get("turn").and_then(|turn| turn.get("status")),
        ),
        ("decision", payload.get("decision")),
        ("worker_status", payload.get("worker_status")),
    ] {
        if let Some(value) = value.and_then(identifier) {
            fields[key] = json!(value);
        }
    }
    if let Ok(kind) = serde_json::from_value::<OwnerActivityKind>(payload["activity_kind"].clone())
    {
        fields["activity_kind"] = json!(kind);
    }
    if let Ok(outcome) = serde_json::from_value::<OwnerOperationOutcome>(payload["outcome"].clone())
    {
        fields["outcome"] = json!(outcome);
    }
    Ok(Some(Observed {
        thread: record.thread_id,
        turn,
        item: record
            .item_id
            .filter(|id| !id.is_empty() && id.len() <= 256),
        event: record.event,
        fields,
    }))
}

struct Connection {
    client: Client,
    frame: Scene,
    source: String,
    thread: String,
    turn: String,
    seq: u64,
    batch: Batch,
    pending: HashSet<String>,
    items: HashMap<String, u32>,
    sent: Instant,
    release_at: Option<Instant>,
}

impl Connection {
    fn post(&mut self, id: &str, seq: u64, events: &[Value]) -> io::Result<()> {
        self.client.post(
            "/v1/producer",
            &json!({
                "identity":self.frame.identity,"epoch":self.frame.epoch,"client":id,
                "source":self.source,"source_revision":self.frame.source_revision,
                "seq":seq,"waiting":!self.pending.is_empty(),"events":events
            }),
        )?;
        self.seq = seq;
        self.sent = Instant::now();
        Ok(())
    }

    fn reset(&mut self, id: &str) -> io::Result<()> {
        self.batch.clear();
        self.pending.clear();
        self.items.clear();
        self.turn.clear();
        self.release_at = Some(Instant::now() + Duration::from_secs(12));
        let _ = self.post(id, self.seq.saturating_add(2), &[]);
        self.post(id, 0, &[])
    }

    fn project(&mut self, event: &Observed) -> Result<Option<Value>, ()> {
        let fields = &event.fields;
        let id = |key: &str| identifier(&fields[key]).ok_or(());
        let value = match event.event.as_str() {
            "turn.started" => {
                self.turn.clone_from(&event.turn);
                self.items.clear();
                self.pending.clear();
                self.release_at = None;
                json!({"event":"turn_started","turn_id":event.turn})
            }
            _ if event.turn != self.turn => return Ok(None),
            "turn.completed" => {
                let outcome = match fields["status"].as_str() {
                    Some("completed") => "completed",
                    Some("failed") => "failed",
                    Some("interrupted" | "canceled" | "cancelled") => "interrupted",
                    _ => return Err(()),
                };
                self.pending.clear();
                self.release_at = Some(Instant::now() + Duration::from_secs(12));
                json!({"event":"turn_complete","turn_id":event.turn,"turn_outcome":outcome})
            }
            "item.started" | "item.completed" | "item.delta" => {
                let channel = match fields["kind"].as_str() {
                    Some("agent_message") => "text",
                    Some("agent_reasoning") => "reasoning",
                    _ => return Ok(None),
                };
                let item = event.item.as_ref().ok_or(())?;
                let next = u32::try_from(self.items.len()).map_err(|_| ())?;
                if next >= 256 && !self.items.contains_key(item) {
                    return Err(());
                }
                let index = *self.items.entry(item.clone()).or_insert(next);
                let name = match (event.event.as_str(), channel) {
                    ("item.delta", _) => "response_delta",
                    ("item.started", "text") => "message_started",
                    ("item.started", _) => "thinking_started",
                    ("item.completed", "text") => "message_complete",
                    _ => "thinking_complete",
                };
                let mut value = json!({"event":name,"index":index});
                if name == "response_delta" {
                    value["channel"] = json!(channel)
                }
                value
            }
            "operation.activity_started" | "operation.activity_completed" => {
                let kind: OwnerActivityKind =
                    serde_json::from_value(fields["activity_kind"].clone()).map_err(|_| ())?;
                let mut value = json!({"event":if event.event.ends_with("started") {"operation_activity_started"} else {"operation_activity_completed"},"span_id":id("span_id")?,"activity_kind":kind});
                if let Some(action) = identifier(&fields["action_id"]) {
                    value["action_id"] = json!(action)
                }
                if event.event.ends_with("completed") {
                    let outcome: OwnerOperationOutcome =
                        serde_json::from_value(fields["outcome"].clone()).map_err(|_| ())?;
                    value["outcome"] = json!(outcome);
                }
                value
            }
            "operation.heartbeat" => json!({"event":"tool_call_heartbeat"}),
            "agent.spawned" => json!({"event":"agent_spawned","id":id("agent_id")?}),
            "agent.completed" => json!({"event":"agent_complete","id":id("agent_id")?}),
            "agent.progress" => {
                let status = fields["worker_status"]
                    .as_str()
                    .filter(|status| {
                        matches!(
                            *status,
                            "queued"
                                | "starting"
                                | "running"
                                | "waiting_for_user"
                                | "model_wait"
                                | "running_tool"
                                | "completed"
                                | "failed"
                                | "cancelled"
                                | "interrupted"
                        )
                    })
                    .ok_or(())?;
                json!({"event":"agent_progress","id":id("agent_id")?,"worker_status":status})
            }
            "approval.required" | "user_input.required" => {
                let key = id(if event.event == "approval.required" {
                    "approval_id"
                } else {
                    "id"
                })?;
                if self.pending.len() >= 128 && !self.pending.contains(&key) {
                    return Err(());
                }
                self.pending.insert(key.clone());
                json!({"event":if event.event == "approval.required" {"approval_required"} else {"user_input_required"},"id":key})
            }
            "approval.decided"
            | "approval.timeout"
            | "user_input.answered"
            | "user_input.canceled" => {
                let key = id(if event.event.starts_with("approval.") {
                    "approval_id"
                } else {
                    "id"
                })?;
                self.pending.remove(&key);
                let outcome = match event.event.as_str() {
                    "approval.timeout" | "user_input.canceled" => "cancelled",
                    "user_input.answered" => "approved",
                    _ => match fields["decision"].as_str() {
                        Some("allow" | "approved") => "approved",
                        Some("deny" | "denied") => "denied",
                        _ => return Err(()),
                    },
                };
                json!({"event":"approval_resolved","id":key,"outcome":outcome})
            }
            _ => return Ok(None),
        };
        Ok(Some(value))
    }
}

fn frame(client: &Client) -> io::Result<Scene> {
    let frame: Scene =
        serde_json::from_value(client.get("/v1/frame")?).map_err(io::Error::other)?;
    if !frame.valid() || frame.identity != client.descriptor.identity {
        return Err(io::Error::other("Invalid companion frame"));
    }
    Ok(frame)
}

fn attach(
    event: &Observed,
    id: &str,
    action_id: &mut String,
    action_seq: &mut u64,
) -> io::Result<Option<Connection>> {
    let client = Client::attach()?;
    let mut scene = frame(&client)?;
    if scene.producer_connected {
        return Ok(None);
    }
    let source = session_source(&event.thread);
    if scene.source != source {
        let next_revision = scene
            .source_revision
            .checked_add(1)
            .ok_or_else(|| io::Error::other("Companion source revision exhausted"))?;
        let action = json!({"identity":scene.identity,"client":action_id,"seq":*action_seq+1,"source_revision":scene.source_revision,"action":{"kind":"select","source":source,"only_if_idle":true}});
        if let Err(error) = client.post("/v1/action", &action) {
            *action_id = uuid::Uuid::new_v4().to_string();
            *action_seq = 0;
            return Err(error);
        }
        *action_seq += 1;
        scene.source.clone_from(&source);
        scene.source_revision = next_revision;
    }
    if scene.source != source || scene.producer_connected {
        return Ok(None);
    }
    let mut connection = Connection {
        client,
        frame: scene,
        source,
        thread: event.thread.clone(),
        turn: event.turn.clone(),
        seq: 0,
        batch: Batch::default(),
        pending: HashSet::new(),
        items: HashMap::new(),
        sent: Instant::now(),
        release_at: None,
    };
    connection.post(id, 0, &[])?;
    Ok(Some(connection))
}

fn run(receiver: mpsc::Receiver<Observed>, gap: Arc<AtomicBool>) {
    let id = uuid::Uuid::new_v4().to_string();
    let mut action_id = uuid::Uuid::new_v4().to_string();
    let mut action_seq = 0;
    let mut connection: Option<Connection> = None;
    let mut warned = false;
    loop {
        let incoming = match &connection {
            Some(_) => match receiver.recv_timeout(Duration::from_millis(100)) {
                Ok(event) => Some(event),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            },
            None => match receiver.recv() {
                Ok(event) => Some(event),
                Err(_) => break,
            },
        };
        if gap.swap(false, Ordering::AcqRel) {
            while receiver.try_recv().is_ok() {}
            if connection
                .as_mut()
                .is_some_and(|current| current.reset(&id).is_err())
            {
                connection = None;
            }
            continue;
        }
        if let Some(event) = incoming {
            if event.event == "turn.started" && connection.is_none() {
                match attach(&event, &id, &mut action_id, &mut action_seq) {
                    Ok(next) => connection = next,
                    Err(error) if error.kind() != io::ErrorKind::NotFound && !warned => {
                        tracing::warn!(%error, "Existing pet companion could not accept Runtime activity");
                        warned = true;
                    }
                    Err(_) => {}
                }
            }
            if let Some(current) = &mut connection
                && current.thread == event.thread
            {
                let queued = match current.project(&event) {
                    Ok(Some(value)) => serde_json::to_string(&value)
                        .ok()
                        .filter(|text| text.len() <= 16_384)
                        .is_some_and(|text| current.batch.push(&text).is_ok_and(|fits| fits)),
                    Ok(None) => true,
                    Err(()) => false,
                };
                if !queued {
                    while receiver.try_recv().is_ok() {}
                    if current.reset(&id).is_err() {
                        connection = None
                    }
                }
            }
        }
        if connection.as_ref().is_some_and(|current| {
            current
                .release_at
                .is_some_and(|deadline| Instant::now() >= deadline)
        }) {
            connection = None;
        }
        if let Some(current) = &mut connection
            && current.sent.elapsed()
                >= Duration::from_millis(if current.batch.events.is_empty() {
                    500
                } else {
                    100
                })
        {
            let queued = std::mem::take(&mut current.batch.events);
            let sent = current.post(&id, current.seq.saturating_add(1), &queued);
            current.batch.clear();
            if sent.is_err() {
                connection = None
            }
        }
    }
}
