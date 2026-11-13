use super::*;
use agent_client_protocol::v1::{ConnectionTo, Responder};
use agent_client_protocol::{Agent, Client, Lines, UntypedMessage};
use codewhale_util::bounded_lines::{BoundedLines, FrameRefusal, MAX_FRAME_BYTES};
use futures_util::{Sink, Stream};
use std::io;
use std::sync::{MutexGuard, PoisonError};
use tokio::sync::{Notify, mpsc};

const PROMPT_BUSY: &str = "a session/prompt turn is already in progress";
const OUTGOING_QUEUE_FRAMES: usize = 256;
const OVERSIZE_RESPONSE: &str =
    "ACP response exceeds the 8 MiB transport limit; inspect canonical state without replay";
type Cx = ConnectionTo<Client>;

pub(super) struct Shared {
    server: tokio::sync::Mutex<AcpServer>,
    prompt: std::sync::Mutex<Option<mpsc::UnboundedSender<Inbound>>>,
    idle: Notify,
    ending: Notify,
    failure: std::sync::Mutex<Option<anyhow::Error>>,
}

struct PromptGuard(Arc<Shared>);

impl Drop for PromptGuard {
    fn drop(&mut self) {
        self.0.release();
    }
}

impl PromptGuard {
    fn finish(self, responder: Responder<Value>, result: std::result::Result<Value, SdkError>) {
        let mut slot = self.0.slot();
        let _ = responder.respond_with_result(result);
        *slot = None;
        drop(slot);
        self.0.idle.notify_waiters();
    }
}

impl Shared {
    pub(super) fn new(server: AcpServer) -> Self {
        Self {
            server: tokio::sync::Mutex::new(server),
            prompt: std::sync::Mutex::new(None),
            idle: Notify::new(),
            ending: Notify::new(),
            failure: std::sync::Mutex::new(None),
        }
    }

    pub(super) fn fail(&self, error: anyhow::Error) {
        self.failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert(error);
        self.tell_prompt(Inbound::Eof);
        self.ending.notify_one();
    }

    pub(super) fn take_failure(&self) -> Option<anyhow::Error> {
        self.failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    #[cfg(test)]
    pub(super) fn into_server(self) -> AcpServer {
        self.server.into_inner()
    }

    fn slot(&self) -> MutexGuard<'_, Option<mpsc::UnboundedSender<Inbound>>> {
        self.prompt.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn prompt_active(&self) -> bool {
        self.slot().is_some()
    }

    fn tell_prompt(&self, message: Inbound) {
        if let Some(inbox) = self.slot().as_ref() {
            let _ = inbox.send(message);
        }
    }

    fn release(&self) {
        *self.slot() = None;
        self.idle.notify_waiters();
    }

    async fn wait_idle(&self) {
        loop {
            let notified = self.idle.notified();
            if !self.prompt_active() {
                return;
            }
            notified.await;
        }
    }

    fn cancel(&self, params: &Value) {
        if let Some(session) = params.get("sessionId").and_then(Value::as_str) {
            self.tell_prompt(Inbound::Cancel {
                session: session.to_string(),
            });
        }
    }

    async fn request(
        self: &Arc<Self>,
        method: String,
        params: Value,
        responder: Responder<Value>,
        cx: Cx,
    ) {
        match method.as_str() {
            "session/cancel" => {
                self.cancel(&params);
                let _ = responder.respond(Value::Null);
            }
            "session/prompt" => self.start_prompt(params, responder, cx).await,
            _ if self.prompt_active() => self.during_prompt(&cx, method, params, responder),
            "shutdown" => {
                let _ = responder.respond(Value::Null);
                self.ending.notify_one();
            }
            "session/load" => self.load(params, responder, cx).await,
            _ => {
                let result = self
                    .server
                    .lock()
                    .await
                    .handle_request(&method, params)
                    .await;
                let _ = responder.respond_with_result(result);
            }
        }
    }

    fn during_prompt(
        self: &Arc<Self>,
        cx: &Cx,
        method: String,
        params: Value,
        responder: Responder<Value>,
    ) {
        let served = matches!(
            method.as_str(),
            "session/list"
                | "session/set_config_option"
                | "session/set_mode"
                | "session/set_model"
                | "session/listProviders"
                | "session/currentModel"
                | "session/selectModel"
        );
        if !served {
            let error = if matches!(
                method.as_str(),
                "initialize" | "session/new" | "session/load" | "shutdown"
            ) {
                SdkError::new(-32603, PROMPT_BUSY)
            } else {
                SdkError::method_not_found().data(Value::String(method))
            };
            let _ = responder.respond_with_error(error);
            return;
        }
        let shared = Arc::clone(self);
        let _ = cx.spawn(async move {
            let result = shared
                .server
                .lock()
                .await
                .handle_request(&method, params)
                .await;
            let _ = responder.respond_with_result(result);
            Ok(())
        });
    }

    async fn load(&self, params: Value, responder: Responder<Value>, cx: Cx) {
        let mut server = self.server.lock().await;
        let result = async {
            let loaded = server.load_session(params).await.map_err(SdkError::from)?;
            if let Some(session) = loaded.get("sessionId").and_then(Value::as_str) {
                let updates = server
                    .history_updates(session)
                    .await
                    .map_err(|error| SdkError::new(-32603, error.to_string()))?;
                for update in updates {
                    cx.send_notification(acp::SessionNotification::new(
                        session.to_string(),
                        update,
                    ))?;
                }
            }
            Ok::<_, SdkError>(loaded)
        }
        .await;
        drop(server);
        let _ = responder.respond_with_result(result);
    }

    async fn start_prompt(self: &Arc<Self>, params: Value, responder: Responder<Value>, cx: Cx) {
        if self.prompt_active() {
            let _ = responder.respond_with_error(SdkError::new(-32603, PROMPT_BUSY));
            return;
        }
        let admitted = {
            let server = self.server.lock().await;
            server.validate_prompt(&params).map(|(session, prompt)| {
                let thread = server.sessions[&session].thread_id.clone();
                (session, thread, prompt, server.prompt_run())
            })
        };
        let (session, thread, prompt, run) = match admitted {
            Ok(admitted) => admitted,
            Err(error) => {
                let _ = responder.respond_with_error(error.into());
                return;
            }
        };
        let (answers, mut inbox) = mpsc::unbounded_channel();
        *self.slot() = Some(answers.clone());
        let guard = PromptGuard(Arc::clone(self));
        let task_cx = cx.clone();
        let _ = cx.spawn(async move {
            let result = {
                let mut emit = |outgoing: Outgoing| -> Result<()> {
                    match outgoing {
                        Outgoing::Update(notification) => task_cx
                            .send_notification(notification)
                            .map_err(|error| anyhow!("ACP update was not sent: {}", error.message)),
                        Outgoing::Permission { key, request } => {
                            let answers = answers.clone();
                            task_cx
                                .send_request(request)
                                .on_receiving_result(async move |reply| {
                                    let gone = matches!(
                                        &reply,
                                        Err(error)
                                            if agent_client_protocol::is_incoming_transport_closed(error)
                                                || agent_client_protocol::is_connection_terminated(error)
                                    );
                                    if !gone {
                                        let _ = answers.send(Inbound::Permission {
                                            key,
                                            allow: permission_allowed(&reply),
                                        });
                                    }
                                    Ok(())
                                })
                                .map_err(|error| {
                                    anyhow!("ACP permission request was not sent: {}", error.message)
                                })
                        }
                    }
                };
                run.drive_prompt(&session, &thread, prompt, &mut emit, &mut inbox)
                    .await
            };
            let result = match result {
                Ok(stop) => to_wire(&acp::PromptResponse::new(stop)).map_err(SdkError::from),
                Err(error) => Err(SdkError::new(
                    -32603,
                    codewhale_engine::client::redact_model_bound_text(&error.to_string(), &[]),
                )),
            };
            guard.finish(responder, result);
            Ok(())
        });
    }
}

fn permission_allowed(
    reply: &std::result::Result<acp::RequestPermissionResponse, SdkError>,
) -> bool {
    matches!(
        reply,
        Ok(response) if matches!(
            &response.outcome,
            acp::RequestPermissionOutcome::Selected(selected)
                if &*selected.option_id.0 == "allow-once"
        )
    )
}

fn closes_input(line: &str) -> bool {
    line.len() <= 256
        && line.contains("daemon/input_closed")
        && serde_json::from_str::<Value>(line)
            .is_ok_and(|message| crate::is_control_input_closed(&message))
}

fn incoming_lines(
    input: Box<dyn AsyncBufRead + Send + Unpin>,
    shared: Arc<Shared>,
) -> impl Stream<Item = io::Result<String>> + Send + 'static {
    futures_util::stream::unfold(
        (BoundedLines::new(input), shared),
        |(mut lines, shared)| async move {
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) if line.trim().is_empty() => continue,
                    Ok(Some(line)) if closes_input(&line) => return None,
                    Ok(Some(line)) => return Some((Ok(line), (lines, shared))),
                    Ok(None) => return None,
                    Err(error) if crate::stdio::is_frame_refusal(&error) => {
                        return Some((Ok(error.to_string()), (lines, shared)));
                    }
                    Err(error) => {
                        shared.fail(anyhow!("ACP input failed: {error}"));
                        return None;
                    }
                }
            }
        },
    )
}

pub(super) fn bound_outgoing(line: String) -> io::Result<String> {
    if line.len() <= MAX_FRAME_BYTES {
        return Ok(line);
    }
    let refused = || io::Error::new(io::ErrorKind::InvalidData, FrameRefusal::TooLarge);
    let Ok(Value::Object(frame)) = serde_json::from_str::<Value>(&line) else {
        return Err(refused());
    };
    if frame.contains_key("method") || !frame.contains_key("id") {
        return Err(refused());
    }
    let failure = |id: Value| {
        json!({"jsonrpc":"2.0","id":id,"error":{"code":-32603,"message":OVERSIZE_RESPONSE}})
            .to_string()
    };
    let replaced = failure(frame["id"].clone());
    Ok(if replaced.len() <= MAX_FRAME_BYTES {
        replaced
    } else {
        failure(Value::Null)
    })
}

pub(super) fn outgoing_lines(
    frames: mpsc::Sender<String>,
    shared: Arc<Shared>,
) -> impl Sink<String, Error = io::Error> + Send + 'static {
    futures_util::sink::unfold(
        (frames, shared),
        |(frames, shared), line: String| async move {
            let line = match bound_outgoing(line) {
                Ok(line) => line,
                Err(error) => {
                    shared.fail(anyhow!("ACP output refused: {error}"));
                    return Ok((frames, shared));
                }
            };
            frames
                .send(line)
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "ACP output closed"))?;
            Ok::<_, io::Error>((frames, shared))
        },
    )
}

pub(super) async fn serve_connection<'a>(
    server: AcpServer,
    input: Box<dyn AsyncBufRead + Send + Unpin>,
    mut output: Box<dyn AsyncWrite + Send + Unpin + 'a>,
) -> Result<Arc<Shared>> {
    let shared = Arc::new(Shared::new(server));
    let (frames, mut queued) = mpsc::channel::<String>(OUTGOING_QUEUE_FRAMES);
    let transport = Lines::new(
        outgoing_lines(frames, Arc::clone(&shared)),
        incoming_lines(input, Arc::clone(&shared)),
    );
    let requests = Arc::clone(&shared);
    let notifications = Arc::clone(&shared);
    let finishing = Arc::clone(&shared);
    let connection = Agent
        .v1()
        .on_receive_request(
            async move |request: UntypedMessage, responder, cx| {
                let (method, params) = request.into_parts();
                requests.request(method, params, responder, cx).await;
                Ok(())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_notification(
            async move |notification: UntypedMessage, _cx| {
                let (method, params) = notification.into_parts();
                if method == "session/cancel" {
                    notifications.cancel(&params);
                }
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .connect_with(transport, async move |cx| {
            tokio::select! {
                () = cx.incoming_closed() => {}
                () = finishing.ending.notified() => {}
            }
            finishing.tell_prompt(Inbound::Eof);
            finishing.wait_idle().await;
            Ok(())
        });
    let connection = async {
        match connection.await {
            Err(error) if agent_client_protocol::is_incoming_transport_closed(&error) => Ok(()),
            other => other.map_err(|error| anyhow!("ACP connection failed: {}", error.message)),
        }
    };
    let failing = Arc::clone(&shared);
    let pump = async {
        let mut broken = false;
        while let Some(frame) = queued.recv().await {
            if broken {
                continue;
            }
            if let Err(error) =
                crate::stdio::write_control_line(&mut output, frame.as_bytes()).await
            {
                broken = true;
                failing.fail(error);
            }
        }
    };
    let (ended, ()) = tokio::join!(connection, pump);
    drop(failing);
    if let Some(error) = shared.take_failure() {
        return Err(error);
    }
    ended?;
    Ok(shared)
}
