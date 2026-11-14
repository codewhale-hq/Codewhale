use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use codewhale_app_server_protocol::rpc::RequestId;
use codewhale_app_server_protocol::runtime::{
    MAX_RUNTIME_IMAGE_BODY_BYTES, NATIVE_RUNTIME_CAPABILITY, NATIVE_RUNTIME_METHODS,
    NativeRuntimeRequest,
};
use futures_util::StreamExt as _;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt as _, AsyncWrite};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};
use tokio_util::sync::CancellationToken;

use crate::{RuntimeOwnerFrontend, RuntimeOwnerRequest};

pub fn capabilities() -> Value {
    json!({"capabilities":[NATIVE_RUNTIME_CAPABILITY],"methods":NATIVE_RUNTIME_METHODS,"max_in_flight":64,"max_subscriptions":16,"max_frame_bytes":MAX_RUNTIME_IMAGE_BODY_BYTES,"max_output_frames":64,"max_output_bytes":MAX_RUNTIME_IMAGE_BODY_BYTES})
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct NativeSelection {
    pub capability: String,
    pub workspace: PathBuf,
}

impl NativeSelection {
    pub(crate) fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.capability == NATIVE_RUNTIME_CAPABILITY
                && self.workspace.is_absolute()
                && self.workspace.as_os_str().len() <= 4096,
            "unsupported native Runtime selection"
        );
        Ok(())
    }
}

type SentSequence = Arc<parking_lot::Mutex<Option<u64>>>;

struct Queued {
    bytes: Vec<u8>,
    _bytes: OwnedSemaphorePermit,
    sent_sequence: Option<(SentSequence, u64)>,
    closed_sequence: Option<SentSequence>,
}

impl Queued {
    async fn write<W: AsyncWrite + Unpin>(mut self, writer: &mut W) -> Result<()> {
        if let Some(sequence) = self.closed_sequence.as_ref() {
            let mut terminal: Value = serde_json::from_slice(&self.bytes)?;
            terminal["params"]["last_sent_sequence"] = json!(*sequence.lock());
            self.bytes = crate::stdio::serialize_control_frame(&terminal)?;
        }
        tokio::time::timeout(
            Duration::from_secs(5),
            crate::stdio::write_control_line(writer, &self.bytes),
        )
        .await
        .context("native write deadline expired")??;
        if let Some((sequence, sent)) = self.sent_sequence {
            *sequence.lock() = Some(sent);
        }
        Ok(())
    }
}

#[derive(Clone)]
struct Output {
    sender: mpsc::Sender<Queued>,
    bytes: Arc<Semaphore>,
    closed: CancellationToken,
}

impl Output {
    fn try_send(&self, value: Value) -> Result<()> {
        self.try_send_tracked(value, None, None)
    }

    fn try_send_tracked(
        &self,
        value: Value,
        sent_sequence: Option<(SentSequence, u64)>,
        closed_sequence: Option<SentSequence>,
    ) -> Result<()> {
        let bytes = crate::stdio::serialize_control_frame(&value)?;
        let permit = self
            .bytes
            .clone()
            .try_acquire_many_owned(u32::try_from(bytes.len())?)?;
        self.sender
            .try_send(Queued {
                bytes,
                _bytes: permit,
                sent_sequence,
                closed_sequence,
            })
            .map_err(|_| anyhow::anyhow!("output queue is full"))?;
        Ok(())
    }

    async fn send(&self, value: Value) -> Result<()> {
        let bytes = crate::stdio::serialize_control_frame(&value)?;
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            let permit = self
                .bytes
                .clone()
                .acquire_many_owned(u32::try_from(bytes.len())?)
                .await?;
            self.sender
                .send(Queued {
                    bytes,
                    _bytes: permit,
                    sent_sequence: None,
                    closed_sequence: None,
                })
                .await
                .map_err(|_| anyhow::anyhow!("output closed"))?;
            Ok::<_, anyhow::Error>(())
        })
        .await;
        match result {
            Ok(Ok(())) => Ok(()),
            _ => {
                self.closed.cancel();
                anyhow::bail!("native output unavailable")
            }
        }
    }
}

pub(crate) fn error(id: Value, code: i64, message: &str, status: u16, outcome: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message.chars().take(512).collect::<String>(),"data":{"status":status,"code":if status==429 {"busy"} else {"native_runtime_refused"},"outcome":outcome}}})
}

fn result(id: Value, value: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":value})
}

fn failed(id: Value, failure: anyhow::Error) -> Value {
    if let Some(failure) = failure.downcast_ref::<crate::RuntimeOwnerFailure>() {
        let mut response = error(
            id,
            if failure.status == 400 {
                -32602
            } else {
                -32000
            },
            &failure.message,
            failure.status,
            "known",
        );
        if let Some(code) = failure.code.as_deref() {
            response["error"]["data"]["code"] = json!(code);
        }
        response
    } else {
        error(id, -32000, "Runtime operation failed", 500, "unknown")
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Envelope {
    pub(crate) jsonrpc: String,
    #[serde(default, deserialize_with = "present_request_id")]
    pub(crate) id: Option<RequestId>,
    pub(crate) method: String,
    #[serde(default)]
    pub(crate) params: Value,
}

fn present_request_id<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<RequestId>, D::Error> {
    RequestId::deserialize(deserializer).map(Some)
}

pub(crate) fn decode_envelope(line: &str) -> Result<Envelope> {
    let request: Envelope = serde_json::from_str(line)?;
    anyhow::ensure!(request.jsonrpc == "2.0", "invalid JSON-RPC version");
    if request.id.is_none() {
        anyhow::ensure!(
            matches!(
                request.method.as_str(),
                "initialized" | "daemon/input_closed"
            ) && (request.params.is_null() || request.params == json!({})),
            "invalid native notification"
        );
    } else {
        anyhow::ensure!(
            !matches!(
                request.method.as_str(),
                "initialized" | "daemon/input_closed"
            ),
            "notification requires no ID"
        );
    }
    Ok(request)
}

pub(crate) async fn serve<R, W>(
    frontend: Arc<dyn RuntimeOwnerFrontend>,
    workspace: PathBuf,
    lines: crate::stdio::BoundedLines<R>,
    mut writer: W,
    peer: Arc<crate::daemon_socket::AuthorizedPeer>,
) -> Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let closed = CancellationToken::new();
    let _cancel = closed.clone().drop_guard();
    let (sender, mut receiver) = mpsc::channel::<Queued>(64);
    let output = Output {
        sender,
        bytes: Arc::new(Semaphore::new(MAX_RUNTIME_IMAGE_BODY_BYTES)),
        closed: closed.clone(),
    };
    let pending = Arc::new(Semaphore::new(64));
    let pending_bytes = Arc::new(Semaphore::new(MAX_RUNTIME_IMAGE_BODY_BYTES));
    let mut lines =
        crate::stdio::BoundedLines::with_budget(lines.into_inner(), pending_bytes.clone());
    let mut subscriptions = HashMap::<String, CancellationToken>::new();
    let mut session = crate::connection::ConnectionSession::default();
    let mut initialized = false;
    let initialize_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        tokio::select! {
            _=closed.cancelled()=>return Ok(()),
            _=tokio::time::sleep_until(initialize_deadline),if !initialized=>anyhow::bail!("native initialize deadline expired"),
            queued=receiver.recv()=>{
                let queued=queued.context("native output closed")?;
                let check=peer.clone();
                codewhale_runtime::runtime_control::owner_work(move||check.check()).await?;
                queued.write(&mut writer).await?;
            }
            line=lines.next_line()=>{
                let line = match line {
                    Ok(line) => line,
                    Err(refusal) if refusal.get_ref().and_then(|source|source.downcast_ref::<crate::stdio::FrameRefusal>()).is_some_and(|failure|matches!(failure,crate::stdio::FrameRefusal::Busy)) => {
                        output.try_send(error(Value::Null,-32000,"Connection input is full",429,"not_started"))?;
                        continue;
                    }
                    Err(error) => return Err(error.into()),
                };
                let Some(line)=line else {return Ok(())};
                let bytes=lines.take_frame_budget();
                let check=peer.clone();
                codewhale_runtime::runtime_control::owner_work(move||check.check()).await?;
                let request=match decode_envelope(&line) {
                    Ok(value)=>value,
                    Err(_)=>{output.try_send(error(Value::Null,-32600,"Invalid request",400,"not_started"))?;continue;}
                };
                let id=json!(request.id);
                if request.jsonrpc!="2.0" {output.try_send(error(id,-32600,"Invalid JSON-RPC version",400,"not_started"))?;continue;}
                if request.method=="daemon/input_closed" && request.id.is_none() {return Ok(());}
                if request.method=="initialized" && request.id.is_none() && session.initialized() && !initialized {initialized=true;continue;}
                if request.id.is_none() {continue;}
                if request.method=="initialize" {
                    let response=match session.initialize(request.params) {
                        Ok(mut response)=>{response["capabilities"]=json!([NATIVE_RUNTIME_CAPABILITY]);result(id,response)},
                        Err(_)=>error(id,-32600,"Initialization refused",400,"not_started"),
                    };
                    output.try_send(response)?;continue;
                }
                if !initialized {output.try_send(error(id,-32600,"Not initialized",400,"not_started"))?;continue;}
                if request.method=="capabilities" || request.method=="healthz" {
                    if request.params!=json!({}) && !request.params.is_null() {output.try_send(error(id,-32602,"Invalid params",400,"not_started"))?;} else {output.try_send(result(id,capabilities()))?;}
                    continue;
                }
                let operation:NativeRuntimeRequest=match serde_json::from_value(json!({"method":request.method,"params":request.params})) {
                    Ok(operation)=>operation,
                    Err(_)=>{output.try_send(error(id,if NATIVE_RUNTIME_METHODS.contains(&request.method.as_str()) {-32602}else{-32601},"Unsupported method or invalid params",400,"not_started"))?;continue;}
                };
                if let NativeRuntimeRequest::Unsubscribe(params)=operation {
                    let removed=subscriptions.remove(&params.subscription_id).map(|token|token.cancel()).is_some();
                    output.try_send(result(id,json!({"subscription_id":params.subscription_id,"removed":removed})))?;continue;
                }
                let permit=match pending.clone().try_acquire_owned() {Ok(permit)=>permit,Err(_)=>{output.try_send(error(id,-32000,"Connection is busy",429,"not_started"))?;continue;}};

                let subscription=if let NativeRuntimeRequest::Subscribe(params)=&operation {
                    subscriptions.retain(|_,token|!token.is_cancelled());
                    if subscriptions.len()>=16 {output.try_send(error(id,-32000,"Subscription limit reached",429,"not_started"))?;continue;}
                    let subscription_id=uuid::Uuid::new_v4().to_string();
                    let token=closed.child_token();
                    subscriptions.insert(subscription_id.clone(),token.clone());
                    Some((subscription_id,params.clone(),token))
                }else{None};
                let frontend=frontend.clone();let workspace=workspace.clone();let output=output.clone();
                tokio::spawn(async move {
                    let _permit=permit;let _bytes=bytes;
                    let response=frontend.execute(RuntimeOwnerRequest::Native {workspace:workspace.clone(),request:operation}).await;
                    if let Some((subscription_id,params,token))=subscription {
                        let _cancel=token.clone().drop_guard();
                        let stream=match response {
                            Ok(_)=>frontend.events_in_workspace(workspace.clone(),params.thread_id.clone(),params.since_seq).await,
                            Err(error)=>Err(error),
                        };
                        let mut stream=match stream {Ok(stream)=>stream,Err(error)=>{let _=output.send(failed(id,error)).await;return;}};
                        if output.send(result(id,json!({"subscription_id":subscription_id,"thread_id":params.thread_id,"since_seq":params.since_seq}))).await.is_err(){return;}
                        drop(_permit);drop(_bytes);
                        let last:SentSequence=Arc::new(parking_lot::Mutex::new(None));
                        loop {
                            let frame=tokio::select! {_=token.cancelled()=>return,frame=stream.next()=>match frame {Some(frame)=>frame,None=>return}};
                            let valid=frontend.execute(RuntimeOwnerRequest::Native {workspace:workspace.clone(),request:NativeRuntimeRequest::Subscribe(params.clone())}).await;
                            if valid.is_err() {token.cancel();output.closed.cancel();return;}
                            let sequence=frame.sequence;
                            let end=frame.event=="stream.end";
                            if output.try_send_tracked(json!({"jsonrpc":"2.0","method":"runtime/event","params":{"subscription_id":subscription_id,"thread_id":params.thread_id,"frame":{"event":frame.event,"data":frame.data,"sequence":sequence}}}),sequence.map(|sequence|(last.clone(),sequence)),None).is_err() {
                                if output.try_send_tracked(json!({"jsonrpc":"2.0","method":"runtime/events/closed","params":{"subscription_id":subscription_id,"reason":"backpressure","last_sent_sequence":u64::MAX}}),None,Some(last.clone())).is_err(){output.closed.cancel();}
                                return;
                            }
                            if end{return;}
                        }
                    } else {
                        let response=match response {Ok(value)=>result(id,value),Err(error)=>failed(id,error)};
                        let _=output.send(response).await;
                    }
                });
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AttachPrelude {
    contract_version: u32,
    target_id: String,
    selection_revision: u64,
    workspace: PathBuf,
    expect_owner: codewhale_protocol::RuntimeOwnerReceipt,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DescribePrelude {
    contract_version: u32,
    workspace: PathBuf,
    expected_pid: u32,
    expected_data_dir: PathBuf,
    config_path: Option<PathBuf>,
}

#[cfg(any(unix, windows))]
pub async fn run_attach_only(describe: bool) -> Result<()> {
    attach_io(
        describe,
        tokio::io::BufReader::new(tokio::io::stdin()),
        tokio::io::stdout(),
    )
    .await
}

#[cfg(not(any(unix, windows)))]
pub async fn run_attach_only(_describe: bool) -> Result<()> {
    anyhow::bail!("native attachment is unavailable on this platform")
}

#[cfg(any(unix, windows))]
pub async fn attach_io<R, W>(describe: bool, mut input: R, mut output: W) -> Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    use tokio::io::AsyncReadExt as _;
    let mut bytes = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(5),
        (&mut input).take(16385).read_until(b'\n', &mut bytes),
    )
    .await
    .context("native attachment prelude deadline expired")??;
    anyhow::ensure!(
        bytes.len() <= 16384 && bytes.last() == Some(&b'\n'),
        "invalid attachment prelude framing"
    );
    let envelope: Envelope = serde_json::from_slice(&bytes)?;
    anyhow::ensure!(
        envelope.jsonrpc == "2.0" && envelope.id.is_some(),
        "invalid attachment envelope"
    );
    let id = json!(envelope.id);
    if describe {
        anyhow::ensure!(
            envelope.method == "runtime/describe-target",
            "target probe prelude required"
        );
        let params: DescribePrelude = serde_json::from_value(envelope.params)?;
        anyhow::ensure!(
            params.contract_version == 1
                && params.expected_pid > 0
                && params.expected_data_dir.is_absolute(),
            "invalid target probe selection"
        );
        #[cfg(unix)]
        let endpoint =
            crate::runtime_api::store_selected_control_socket(None, &params.expected_data_dir)
                .await?;
        #[cfg(windows)]
        let endpoint = codewhale_runtime::runtime_control::owner_work(
            crate::daemon_socket::default_socket_path,
        )
        .await?;
        let observed = crate::daemon_client::connect(params.config_path, Some(endpoint)).await?;
        anyhow::ensure!(
            observed.receipt().pid == params.expected_pid,
            "target probe process mismatch"
        );
        crate::runtime_api::validate_selected_owner(&observed, params.expected_data_dir).await?;
        let receipt = observed.receipt().clone();
        drop(observed);
        let mut client =
            crate::daemon_client::connect_native(receipt, params.workspace.clone()).await?;
        client
            .send(json!("native-probe"), "runtime/capabilities", json!({}))
            .await?;
        let response = tokio::time::timeout(Duration::from_secs(5), client.recv())
            .await
            .context("native probe response deadline expired")??
            .context("native probe closed")?;
        anyhow::ensure!(
            response["id"] == "native-probe" && response.get("error").is_none(),
            "native probe refused"
        );
        crate::stdio::write_stdio_line(&mut output,&result(id,json!({"contract_version":1,"workspace":params.workspace,"expect_owner":client.receipt(),"capabilities":[NATIVE_RUNTIME_CAPABILITY]}))).await?;
        return Ok(());
    }
    anyhow::ensure!(
        envelope.method == "runtime/attach",
        "native attachment prelude required"
    );
    let params: AttachPrelude = serde_json::from_value(envelope.params)?;
    anyhow::ensure!(
        params.contract_version == 1
            && !params.target_id.is_empty()
            && params.target_id.len() <= 256
            && params.selection_revision > 0,
        "invalid native target binding"
    );
    let client =
        crate::daemon_client::connect_native(params.expect_owner, params.workspace.clone()).await?;
    tokio::time::timeout(Duration::from_secs(5),crate::stdio::write_stdio_line(&mut output,&result(id,json!({"contract_version":1,"target_id":params.target_id,"selection_revision":params.selection_revision,"workspace":params.workspace,"expect_owner":client.receipt(),"capabilities":[NATIVE_RUNTIME_CAPABILITY]})))).await.context("native attach acknowledgement deadline expired")??;
    client.forward_native(input, output).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(frames: usize, bytes: usize) -> (Output, mpsc::Receiver<Queued>) {
        let (sender, receiver) = mpsc::channel(frames);
        (
            Output {
                sender,
                bytes: Arc::new(Semaphore::new(bytes)),
                closed: CancellationToken::new(),
            },
            receiver,
        )
    }

    #[tokio::test]
    async fn native_cursor_advances_only_after_successful_write_and_closed_frame_is_current() {
        let (output, mut receiver) = output(4, 4096);
        let sequence: SentSequence = Arc::new(parking_lot::Mutex::new(None));
        output
            .try_send_tracked(json!({"event":"first"}), Some((sequence.clone(), 42)), None)
            .unwrap();
        output
            .try_send_tracked(
                json!({"params":{"last_sent_sequence":u64::MAX}}),
                None,
                Some(sequence.clone()),
            )
            .unwrap();
        assert_eq!(*sequence.lock(), None);
        receiver
            .recv()
            .await
            .unwrap()
            .write(&mut tokio::io::sink())
            .await
            .unwrap();
        assert_eq!(*sequence.lock(), Some(42));
        let mut terminal = Vec::new();
        receiver
            .recv()
            .await
            .unwrap()
            .write(&mut terminal)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&terminal).unwrap()["params"]["last_sent_sequence"],
            42
        );
        output
            .try_send_tracked(
                json!({"event":"second"}),
                Some((sequence.clone(), 43)),
                None,
            )
            .unwrap();
        let (mut failed, read) = tokio::io::duplex(64);
        drop(read);
        assert!(
            receiver
                .recv()
                .await
                .unwrap()
                .write(&mut failed)
                .await
                .is_err()
        );
        assert_eq!(*sequence.lock(), Some(42));
        assert_eq!(output.bytes.available_permits(), 4096);
    }

    #[tokio::test]
    async fn native_output_frames_and_bytes_have_independent_bounds() {
        let (frames, mut receiver) = output(2, 4096);
        frames.try_send(json!({})).unwrap();
        frames.try_send(json!({})).unwrap();
        assert!(frames.try_send(json!({})).is_err());
        drop(receiver.recv().await);
        drop(receiver.recv().await);
        assert_eq!(frames.bytes.available_permits(), 4096);
        let (bytes, mut receiver) = output(64, 2);
        bytes.try_send(json!({})).unwrap();
        assert!(bytes.try_send(json!({})).is_err());
        drop(receiver.recv().await);
        assert_eq!(bytes.bytes.available_permits(), 2);
    }
}
