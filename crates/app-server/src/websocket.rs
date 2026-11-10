use crate::stdio::serialize_control_frame;
use codewhale_app_server_protocol::runtime::MAX_RUNTIME_IMAGE_BODY_BYTES;
use futures_util::{Sink, Stream};
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll, ready};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, Join, ReadBuf, Take};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::protocol::{Message, WebSocketConfig};

pub(crate) type LocalWebSocket<R, W> = WebSocketLines<Join<Take<R>, W>>;

fn configuration() -> WebSocketConfig {
    WebSocketConfig::default()
        .write_buffer_size(0)
        .max_write_buffer_size(MAX_RUNTIME_IMAGE_BODY_BYTES + 128)
        .max_message_size(Some(MAX_RUNTIME_IMAGE_BODY_BYTES))
        .max_frame_size(Some(MAX_RUNTIME_IMAGE_BODY_BYTES))
}

pub(crate) async fn accept<R, W>(read: R, write: W) -> anyhow::Result<LocalWebSocket<R, W>>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let stream = tokio::io::join(read.take(16384), write);
    let mut socket = tokio::time::timeout(
        Duration::from_secs(5),
        tokio_tungstenite::accept_hdr_async_with_config(
            stream,
            refuse_browser_origin,
            Some(configuration()),
        ),
    )
    .await??;
    socket.get_mut().reader_mut().set_limit(u64::MAX);
    Ok(WebSocketLines::new(socket))
}

#[allow(clippy::result_large_err)]
fn refuse_browser_origin(request: &Request, response: Response) -> Result<Response, ErrorResponse> {
    if request.headers().contains_key("origin") {
        let mut refusal = tokio_tungstenite::tungstenite::http::Response::new(Some(
            "Origin is not accepted on the private owner channel".to_string(),
        ));
        *refusal.status_mut() = tokio_tungstenite::tungstenite::http::StatusCode::FORBIDDEN;
        return Err(refusal);
    }
    Ok(response)
}

pub(crate) async fn connect<R, W>(read: R, write: W) -> anyhow::Result<LocalWebSocket<R, W>>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let stream = tokio::io::join(read.take(16384), write);
    let (mut socket, _) = tokio::time::timeout(
        Duration::from_secs(5),
        tokio_tungstenite::client_async_with_config(
            "ws://localhost/",
            stream,
            Some(configuration()),
        ),
    )
    .await??;
    socket.get_mut().reader_mut().set_limit(u64::MAX);
    Ok(WebSocketLines::new(socket))
}

fn peer_gone(error: &tokio_tungstenite::tungstenite::Error) -> bool {
    use tokio_tungstenite::tungstenite::Error;
    match error {
        Error::ConnectionClosed | Error::AlreadyClosed => true,
        Error::Io(error) => matches!(
            error.kind(),
            io::ErrorKind::BrokenPipe
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::ConnectionAborted
                | io::ErrorKind::UnexpectedEof
        ),
        _ => false,
    }
}

pub(crate) struct WebSocketLines<S> {
    socket: WebSocketStream<S>,
    incoming: Vec<u8>,
    incoming_offset: usize,
    outgoing: Vec<u8>,
    flush_control: bool,
    closed: bool,
}

impl<S> WebSocketLines<S> {
    fn new(socket: WebSocketStream<S>) -> Self {
        Self {
            socket,
            incoming: Vec::new(),
            incoming_offset: 0,
            outgoing: Vec::new(),
            flush_control: false,
            closed: false,
        }
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncRead for WebSocketLines<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if buffer.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        for _ in 0..32 {
            if this.incoming_offset < this.incoming.len() {
                let count = buffer
                    .remaining()
                    .min(this.incoming.len() - this.incoming_offset);
                buffer
                    .put_slice(&this.incoming[this.incoming_offset..this.incoming_offset + count]);
                this.incoming_offset += count;
                if this.incoming_offset == this.incoming.len() {
                    this.incoming.clear();
                    this.incoming_offset = 0;
                }
                return Poll::Ready(Ok(()));
            }
            if this.flush_control {
                match ready!(Pin::new(&mut this.socket).poll_flush(cx)) {
                    Ok(()) => this.flush_control = false,
                    Err(error) if peer_gone(&error) => {
                        this.closed = true;
                        this.flush_control = false;
                        return Poll::Ready(Ok(()));
                    }
                    Err(error) => return Poll::Ready(Err(io::Error::other(error))),
                }
            }
            if this.closed {
                return Poll::Ready(Ok(()));
            }
            match ready!(Pin::new(&mut this.socket).poll_next(cx)) {
                Some(Ok(Message::Text(text))) => {
                    this.incoming = match serde_json::from_str::<serde_json::Value>(&text) {
                        Ok(value) => serialize_control_frame(&value)?,
                        Err(_) if !text.contains(['\r', '\n']) => text.as_bytes().to_vec(),
                        Err(_) => b"{invalid WebSocket JSON".to_vec(),
                    };
                    this.incoming.reserve_exact(1);
                    this.incoming.push(b'\n');
                }
                Some(Ok(Message::Ping(_))) => this.flush_control = true,
                Some(Ok(Message::Close(_))) => {
                    this.closed = true;
                    this.flush_control = true;
                }
                Some(Ok(Message::Binary(_) | Message::Pong(_) | Message::Frame(_))) => {}
                Some(Err(tokio_tungstenite::tungstenite::Error::Protocol(
                    tokio_tungstenite::tungstenite::error::ProtocolError::ResetWithoutClosingHandshake,
                ))) => {
                    this.closed = true;
                    return Poll::Ready(Ok(()));
                }
                Some(Err(error)) if peer_gone(&error) => {
                    this.closed = true;
                    return Poll::Ready(Ok(()));
                }
                Some(Err(error)) => return Poll::Ready(Err(io::Error::other(error))),
                None => {
                    this.closed = true;
                    return Poll::Ready(Ok(()));
                }
            }
        }
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncWrite for WebSocketLines<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if this.closed {
            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
        }
        let newline = bytes.iter().position(|byte| *byte == b'\n');
        let count = newline.unwrap_or(bytes.len());
        if count > MAX_RUNTIME_IMAGE_BODY_BYTES.saturating_sub(this.outgoing.len()) {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::InvalidData,
                crate::stdio::FrameRefusal::TooLarge,
            )));
        }
        if newline.is_some() {
            ready!(Pin::new(&mut this.socket).poll_ready(cx)).map_err(io::Error::other)?;
        }
        this.outgoing.reserve_exact(count);
        this.outgoing.extend_from_slice(&bytes[..count]);
        if newline.is_some() {
            let text = String::from_utf8(std::mem::take(&mut this.outgoing))
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            Pin::new(&mut this.socket)
                .start_send(Message::Text(text.into()))
                .map_err(io::Error::other)?;
        }
        Poll::Ready(Ok(count + usize::from(newline.is_some())))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().socket)
            .poll_flush(cx)
            .map_err(io::Error::other)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if !this.outgoing.is_empty() {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "private WebSocket output ended inside a JSON frame",
            )));
        }
        Pin::new(&mut this.socket)
            .poll_close(cx)
            .map_err(io::Error::other)
    }
}
