//! HTTP routes, RFC 6455 WebSocket transport, protocol dispatch, event fan-out
//! and cross-socket recording ownership coordination for `scriber-server`.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, PoisonError};
use std::task::Poll;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::header::{
    CONNECTION, CONTENT_TYPE, SEC_WEBSOCKET_ACCEPT, SEC_WEBSOCKET_KEY, SEC_WEBSOCKET_VERSION,
    UPGRADE,
};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use hyper_util::rt::TokioIo;
use scriber_engine::{Action, Engine, Event, Phase, Settings, Snapshot, StreamRecorder};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha1::{Digest as _, Sha1};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::sync::{Mutex as AsyncMutex, mpsc, watch};
use tokio::task::JoinHandle;

use crate::AuthMode;
use crate::diagnostics::{AudioCounts, DiagnosticEvent, Diagnostics};

/// Server version reported in `hello` and `app_version`.
pub(crate) const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Maximum incoming WebSocket frame and reassembled message length (1 MiB inclusive).
pub(crate) const MAX_WS_PAYLOAD: usize = 1_048_576;

/// Bounded per-subscriber event queue length before lag disconnect.
pub(crate) const SUBSCRIBER_QUEUE_CAPACITY: usize = 64;

/// Fixed error message for desktop-only commands in the web app.
pub(crate) const NOT_AVAILABLE_ERR: &str = "Not available in the web app.";

/// Event name for session snapshots.
pub(crate) const DICTATION_EVENT: &str = "dictation-changed";

/// Event name for live microphone levels.
pub(crate) const LEVEL_EVENT: &str = "dictation-level";

/// RFC 6455 WebSocket handshake GUID.
const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// RFC 6455 frame opcodes.
const OP_CONTINUATION: u8 = 0x0;
const OP_TEXT: u8 = 0x1;
const OP_BINARY: u8 = 0x2;
const OP_CLOSE: u8 = 0x8;
const OP_PING: u8 = 0x9;
const OP_PONG: u8 = 0xA;

/// WebSocket close status codes.
pub(crate) const CLOSE_NORMAL: u16 = 1000;
pub(crate) const CLOSE_GOING_AWAY: u16 = 1001;
pub(crate) const CLOSE_PROTOCOL_ERROR: u16 = 1002;
pub(crate) const CLOSE_INVALID_PAYLOAD: u16 = 1007;
pub(crate) const CLOSE_MESSAGE_TOO_BIG: u16 = 1009;
pub(crate) const CLOSE_UNAUTHORIZED: u16 = 4401;

/// Outcome of racing two futures with [`race2`].
pub(crate) enum Either<A, B> {
    /// First future completed first (polled first when both are ready).
    Left(A),
    /// Second future completed first.
    Right(B),
}

/// Biased two-way future race without macro expansion overhead.
pub(crate) async fn race2<A: Future, B: Future>(a: A, b: B) -> Either<A::Output, B::Output> {
    let mut a = std::pin::pin!(a);
    let mut b = std::pin::pin!(b);
    std::future::poll_fn(|cx| {
        if let Poll::Ready(val) = a.as_mut().poll(cx) {
            return Poll::Ready(Either::Left(val));
        }
        if let Poll::Ready(val) = b.as_mut().poll(cx) {
            return Poll::Ready(Either::Right(val));
        }
        Poll::Pending
    })
    .await
}

/// Deterministic synchronization hooks used by tests to exercise concurrent
/// bootstrap publishes and stalled subscribers without timing sleeps.
pub(crate) trait ServerHooks: Send + Sync {
    /// Invoked after subscribing to events and before sampling the bootstrap snapshot.
    fn before_sample(&self, conn_id: u64) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;

    /// Invoked in the writer task immediately before writing a live event frame.
    fn before_event_write(&self, conn_id: u64) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

/// Production no-op hooks.
#[derive(Default)]
pub(crate) struct NoopHooks;

impl ServerHooks for NoopHooks {
    fn before_sample(&self, _conn_id: u64) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::ready(()))
    }

    fn before_event_write(&self, _conn_id: u64) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::ready(()))
    }
}

/// One sequenced event published by the engine callback.
#[derive(Clone)]
pub(crate) struct HubEvent {
    /// Monotonically increasing sequence number assigned on publish.
    pub(crate) seq: u64,
    /// Engine event payload.
    pub(crate) event: Event,
}

struct Subscriber {
    conn_id: u64,
    tx: mpsc::Sender<HubEvent>,
    overflow_tx: watch::Sender<bool>,
}

struct HubInner {
    next_seq: u64,
    subscribers: Vec<Subscriber>,
}

/// Nonblocking fan-out hub for [`Event`]s.
///
/// The engine invokes [`Self::publish`] from its event callback while holding
/// the session lock or on the audio pusher thread. This hub holds only its own
/// short leaf mutex and never calls `Engine` or session coordination locks.
#[derive(Clone)]
pub(crate) struct EventHub {
    inner: Arc<StdMutex<HubInner>>,
    diagnostics: Diagnostics,
}

impl EventHub {
    /// Creates an empty event hub.
    #[must_use]
    pub(crate) fn new(diagnostics: Diagnostics) -> Self {
        Self {
            inner: Arc::new(StdMutex::new(HubInner {
                next_seq: 0,
                subscribers: Vec::new(),
            })),
            diagnostics,
        }
    }

    /// Publishes `event` to all currently subscribed authenticated connections.
    /// Subscribers whose 64-event queue is full are removed and signaled to disconnect.
    pub(crate) fn publish(&self, event: Event) {
        let mut lagged = Vec::new();
        {
            let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
            inner.next_seq = inner.next_seq.saturating_add(1);
            let item = HubEvent {
                seq: inner.next_seq,
                event,
            };
            inner
                .subscribers
                .retain(|sub| match sub.tx.try_send(item.clone()) {
                    Ok(()) => true,
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        let _sent = sub.overflow_tx.send(true);
                        lagged.push(sub.conn_id);
                        false
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => false,
                });
        }
        for conn_id in lagged {
            self.diagnostics.emit(&DiagnosticEvent::SubscriberLag {
                conn_id,
                reason: "subscriber_lag",
                queue_capacity: SUBSCRIBER_QUEUE_CAPACITY,
            });
        }
    }

    /// Registers a new bounded subscriber and returns its handle.
    #[must_use]
    pub(crate) fn subscribe(&self, conn_id: u64) -> Subscription {
        let (tx, rx) = mpsc::channel(SUBSCRIBER_QUEUE_CAPACITY);
        let (overflow_tx, overflow_rx) = watch::channel(false);
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .subscribers
            .push(Subscriber {
                conn_id,
                tx,
                overflow_tx,
            });
        Subscription {
            conn_id,
            rx,
            overflow_rx,
            hub: self.clone(),
        }
    }

    /// Sequence number of the most recently published event.
    #[must_use]
    pub(crate) fn current_seq(&self) -> u64 {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .next_seq
    }

    fn unsubscribe(&self, conn_id: u64) {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .subscribers
            .retain(|sub| sub.conn_id != conn_id);
    }

    #[cfg(test)]
    pub(crate) fn subscriber_count(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .subscribers
            .len()
    }
}

/// Active event subscription for one authenticated connection.
pub(crate) struct Subscription {
    conn_id: u64,
    rx: mpsc::Receiver<HubEvent>,
    overflow_rx: watch::Receiver<bool>,
    hub: EventHub,
}

#[cfg(test)]
impl Subscription {
    pub(crate) fn close_rx(&mut self) {
        self.rx.close();
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.hub.unsubscribe(self.conn_id);
    }
}

/// Cross-socket admission and recording ownership state.
struct OwnershipState {
    accepting: bool,
    owner: Option<u64>,
}

/// Shared state across HTTP and WebSocket handlers.
#[derive(Clone)]
pub(crate) struct ServerState {
    engine: Engine,
    recorder: StreamRecorder,
    auth: AuthMode,
    web_dir: PathBuf,
    hub: EventHub,
    diagnostics: Diagnostics,
    hooks: Arc<dyn ServerHooks>,
    next_conn_id: Arc<AtomicU64>,
    accepting: Arc<AtomicBool>,
    session_gate: Arc<AsyncMutex<OwnershipState>>,
    connections: Arc<AsyncMutex<HashMap<u64, JoinHandle<()>>>>,
    shutdown_tx: watch::Sender<bool>,
}

impl ServerState {
    /// Creates server state wired to one engine, recorder, event hub and hooks.
    #[must_use]
    pub(crate) fn new(
        engine: Engine,
        recorder: StreamRecorder,
        auth: AuthMode,
        web_dir: PathBuf,
        hub: EventHub,
        diagnostics: Diagnostics,
        hooks: Arc<dyn ServerHooks>,
    ) -> Self {
        let (shutdown_tx, _rx) = watch::channel(false);
        Self {
            engine,
            recorder,
            auth,
            web_dir,
            hub,
            diagnostics,
            hooks,
            next_conn_id: Arc::new(AtomicU64::new(1)),
            accepting: Arc::new(AtomicBool::new(true)),
            session_gate: Arc::new(AsyncMutex::new(OwnershipState {
                accepting: true,
                owner: None,
            })),
            connections: Arc::new(AsyncMutex::new(HashMap::new())),
            shutdown_tx,
        }
    }

    /// Stops new admissions, signals all tracked sockets to close, and joins
    /// every socket task so all in-flight RPCs and PCM pushes have settled.
    pub(crate) async fn drain_and_close_connections(&self) {
        self.accepting.store(false, Ordering::SeqCst);
        {
            let mut gate = self.session_gate.lock().await;
            gate.accepting = false;
        }
        let _sent = self.shutdown_tx.send(true);
        loop {
            let handles: Vec<JoinHandle<()>> = {
                let mut map = self.connections.lock().await;
                map.drain().map(|(_id, handle)| handle).collect()
            };
            if handles.is_empty() {
                break;
            }
            for handle in handles {
                let _joined = handle.await;
            }
        }
    }

    fn is_accepting(&self) -> bool {
        self.accepting.load(Ordering::SeqCst)
    }

    fn shutdown_rx(&self) -> watch::Receiver<bool> {
        self.shutdown_tx.subscribe()
    }
}

/// Builds the axum [`Router`] with `/healthz`, `/ws`, and static file serving.
pub(crate) fn router(state: ServerState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/ws", get(ws_upgrade))
        .fallback(get(serve_static))
        .with_state(state)
}

async fn healthz() -> &'static str {
    "ok"
}

/// Serves static files strictly inside `SCRIBER_WEB_DIR`, refusing path
/// traversal and symlinks that resolve outside the configured directory.
async fn serve_static(State(state): State<ServerState>, req: Request) -> Response {
    let raw_path = req.uri().path().to_string();
    let web_dir = state.web_dir.clone();
    let loaded = tokio::task::spawn_blocking(move || read_static_asset(&web_dir, &raw_path))
        .await
        .ok()
        .flatten();
    match loaded {
        Some((mime, bytes)) => {
            let mut res = (StatusCode::OK, bytes).into_response();
            let _prev = res
                .headers_mut()
                .insert(CONTENT_TYPE, HeaderValue::from_static(mime));
            res
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Validates `uri_path`, resolves canonical paths under `web_dir` and reads the
/// target file if it stays strictly inside `web_dir`.
pub(crate) fn read_static_asset(web_dir: &Path, uri_path: &str) -> Option<(&'static str, Vec<u8>)> {
    let rel = sanitize_static_path(uri_path)?;
    let root = std::fs::canonicalize(web_dir).ok()?;
    let resolved = std::fs::canonicalize(root.join(&rel)).ok()?;
    if !resolved.starts_with(&root) || !resolved.is_file() {
        return None;
    }
    let bytes = std::fs::read(&resolved).ok()?;
    Some((content_type_for(&resolved), bytes))
}

pub(crate) fn sanitize_static_path(uri_path: &str) -> Option<PathBuf> {
    let decoded = percent_decode(uri_path)?;
    let trimmed = decoded.trim_start_matches('/');
    let rel_str = if trimmed.is_empty() {
        "index.html"
    } else {
        trimmed
    };
    if rel_str.contains('\0') || rel_str.contains('\\') {
        return None;
    }
    let mut clean = PathBuf::new();
    for component in Path::new(rel_str).components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => return None,
        }
    }
    Some(clean)
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if b == b'%' {
            let hi = hex_nibble(*bytes.get(i.checked_add(1)?)?)?;
            let lo = hex_nibble(*bytes.get(i.checked_add(2)?)?)?;
            out.push((hi << 4).saturating_add(lo));
            i = i.checked_add(3)?;
        } else {
            out.push(b);
            i = i.checked_add(1)?;
        }
    }
    String::from_utf8(out).ok()
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b.saturating_sub(b'0')),
        b'a'..=b'f' => Some(b.saturating_sub(b'a').saturating_add(10)),
        b'A'..=b'F' => Some(b.saturating_sub(b'A').saturating_add(10)),
        _ => None,
    }
}

fn content_type_for(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("webmanifest") => "application/manifest+json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Computes RFC 6455 `Sec-WebSocket-Accept` from `Sec-WebSocket-Key`.
pub(crate) fn websocket_accept_key(key: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(key.as_bytes());
    hasher.update(WS_GUID.as_bytes());
    BASE64.encode(hasher.finalize())
}

fn header_contains_token(req: &Request, name: axum::http::HeaderName, token: &str) -> bool {
    req.headers()
        .get_all(name)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|s| s.split(','))
        .any(|part| part.trim().eq_ignore_ascii_case(token))
}

/// Boxed async byte reader used by the WebSocket framing layer.
pub(crate) type BoxRead = Box<dyn AsyncRead + Unpin + Send>;

/// Boxed async byte writer used by the WebSocket framing layer.
pub(crate) type BoxWrite = Box<dyn AsyncWrite + Unpin + Send>;

/// Upgrades `GET /ws` to an RFC 6455 WebSocket connection tracked by `state`.
pub(crate) async fn ws_upgrade(State(state): State<ServerState>, req: Request) -> Response {
    let valid_upgrade = header_contains_token(&req, UPGRADE, "websocket")
        && header_contains_token(&req, CONNECTION, "upgrade")
        && req
            .headers()
            .get(SEC_WEBSOCKET_VERSION)
            .and_then(|v| v.to_str().ok())
            == Some("13");
    let key = req
        .headers()
        .get(SEC_WEBSOCKET_KEY)
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let Some(key) = key.filter(|_| valid_upgrade) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let accept = websocket_accept_key(&key);
    let accept_header = HeaderValue::from_str(&accept).unwrap_or(HeaderValue::from_static(""));

    let on_upgrade = hyper::upgrade::on(req);
    let conn_id = state.next_conn_id.fetch_add(1, Ordering::SeqCst);
    let mut map = state.connections.lock().await;
    if !state.is_accepting() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let task_state = state.clone();
    let handle = tokio::spawn(async move {
        if let Ok(upgraded) = on_upgrade.await {
            let (read_half, write_half) = tokio::io::split(TokioIo::new(upgraded));
            handle_connection(
                task_state.clone(),
                conn_id,
                Box::new(read_half),
                Box::new(write_half),
            )
            .await;
        }
        let _removed = task_state.connections.lock().await.remove(&conn_id);
    });
    let _prev = map.insert(conn_id, handle);
    drop(map);

    let mut res = StatusCode::SWITCHING_PROTOCOLS.into_response();
    let headers = res.headers_mut();
    let _u = headers.insert(UPGRADE, HeaderValue::from_static("websocket"));
    let _c = headers.insert(CONNECTION, HeaderValue::from_static("Upgrade"));
    let _a = headers.insert(SEC_WEBSOCKET_ACCEPT, accept_header);
    res
}

// ---------------------------------------------------------------------------
// RFC 6455 framing with bounded buffering
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum WsIncoming {
    Text(String),
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Close,
    Eof,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum WsReadError {
    TooBig,
    ProtocolError,
    InvalidUtf8,
    Io,
}

pub(crate) struct WsReader {
    reader: BoxRead,
    frag_opcode: Option<u8>,
    frag_buf: Vec<u8>,
}

impl WsReader {
    pub(crate) fn new(reader: BoxRead) -> Self {
        Self {
            reader,
            frag_opcode: None,
            frag_buf: Vec::new(),
        }
    }

    pub(crate) async fn read_message(&mut self) -> Result<WsIncoming, WsReadError> {
        loop {
            let mut head = [0u8; 2];
            match self.reader.read_exact(&mut head).await {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return if self.frag_opcode.is_none() {
                        Ok(WsIncoming::Eof)
                    } else {
                        Err(WsReadError::ProtocolError)
                    };
                }
                Err(_) => return Err(WsReadError::Io),
            }
            let fin = (head[0] & 0x80) != 0;
            let rsv = head[0] & 0x70;
            let opcode = head[0] & 0x0F;
            let masked = (head[1] & 0x80) != 0;
            let len7 = head[1] & 0x7F;
            if rsv != 0 || !masked {
                return Err(WsReadError::ProtocolError);
            }
            let payload_len = self.read_extended_len(len7).await?;

            // Control frames (Close, Ping, Pong).
            if opcode >= 0x8 {
                if !fin || payload_len > 125 {
                    return Err(WsReadError::ProtocolError);
                }
                let len = usize::try_from(payload_len).map_err(|_| WsReadError::ProtocolError)?;
                let payload = self.read_masked_payload(len).await?;
                match opcode {
                    OP_CLOSE => {
                        if payload.len() == 1 {
                            return Err(WsReadError::ProtocolError);
                        }
                        return Ok(WsIncoming::Close);
                    }
                    OP_PING => return Ok(WsIncoming::Ping(payload)),
                    OP_PONG => continue,
                    _ => return Err(WsReadError::ProtocolError),
                }
            }

            // Data frames (Continuation, Text, Binary).
            match opcode {
                OP_CONTINUATION if self.frag_opcode.is_some() => {}
                OP_TEXT | OP_BINARY if self.frag_opcode.is_none() => {}
                _ => return Err(WsReadError::ProtocolError),
            }

            // Enforce the 1 MiB inclusive limit BEFORE allocating or reading bytes.
            let frame_len = usize::try_from(payload_len).map_err(|_| WsReadError::TooBig)?;
            if frame_len > MAX_WS_PAYLOAD {
                return Err(WsReadError::TooBig);
            }
            let total_len = self
                .frag_buf
                .len()
                .checked_add(frame_len)
                .ok_or(WsReadError::TooBig)?;
            if total_len > MAX_WS_PAYLOAD {
                return Err(WsReadError::TooBig);
            }

            let payload = self.read_masked_payload(frame_len).await?;
            if opcode != OP_CONTINUATION && fin {
                return decode_data_message(opcode, payload);
            }
            if opcode != OP_CONTINUATION {
                self.frag_opcode = Some(opcode);
            }
            self.frag_buf.extend_from_slice(&payload);
            if fin {
                let op = self.frag_opcode.take().unwrap_or(OP_BINARY);
                let data = std::mem::take(&mut self.frag_buf);
                return decode_data_message(op, data);
            }
        }
    }

    async fn read_extended_len(&mut self, len7: u8) -> Result<u64, WsReadError> {
        match len7 {
            0..=125 => Ok(u64::from(len7)),
            126 => {
                let mut ext = [0u8; 2];
                let _n = self
                    .reader
                    .read_exact(&mut ext)
                    .await
                    .map_err(|_| WsReadError::Io)?;
                Ok(u64::from(u16::from_be_bytes(ext)))
            }
            _ => {
                let mut ext = [0u8; 8];
                let _n = self
                    .reader
                    .read_exact(&mut ext)
                    .await
                    .map_err(|_| WsReadError::Io)?;
                let len = u64::from_be_bytes(ext);
                if (len & (1u64 << 63)) != 0 {
                    return Err(WsReadError::ProtocolError);
                }
                Ok(len)
            }
        }
    }

    async fn read_masked_payload(&mut self, len: usize) -> Result<Vec<u8>, WsReadError> {
        let mut mask = [0u8; 4];
        let _mask_n = self
            .reader
            .read_exact(&mut mask)
            .await
            .map_err(|_| WsReadError::Io)?;
        let mut payload = vec![0u8; len];
        let _body_n = self
            .reader
            .read_exact(&mut payload)
            .await
            .map_err(|_| WsReadError::Io)?;
        for (byte, &m) in payload.iter_mut().zip(mask.iter().cycle()) {
            *byte ^= m;
        }
        Ok(payload)
    }
}

fn decode_data_message(opcode: u8, payload: Vec<u8>) -> Result<WsIncoming, WsReadError> {
    if opcode == OP_TEXT {
        String::from_utf8(payload)
            .map(WsIncoming::Text)
            .map_err(|_| WsReadError::InvalidUtf8)
    } else {
        Ok(WsIncoming::Binary(payload))
    }
}

pub(crate) async fn ws_write_frame(
    writer: &mut BoxWrite,
    opcode: u8,
    payload: &[u8],
) -> std::io::Result<()> {
    let mut head = Vec::with_capacity(10);
    head.push(0x80_u8.saturating_add(opcode & 0x0F));
    let len = payload.len();
    if let Ok(short) = u8::try_from(len)
        && short <= 125
    {
        head.push(short);
    } else if let Ok(med) = u16::try_from(len) {
        head.push(126);
        head.extend_from_slice(&med.to_be_bytes());
    } else {
        head.push(127);
        let wide = u64::try_from(len).unwrap_or(u64::MAX);
        head.extend_from_slice(&wide.to_be_bytes());
    }
    writer.write_all(&head).await?;
    writer.write_all(payload).await?;
    writer.flush().await
}

async fn ws_write_close(writer: &mut BoxWrite, code: u16, reason: &str) -> std::io::Result<()> {
    let mut body = Vec::with_capacity(reason.len().saturating_add(2));
    body.extend_from_slice(&code.to_be_bytes());
    body.extend_from_slice(reason.as_bytes());
    ws_write_frame(writer, OP_CLOSE, &body).await?;
    writer.shutdown().await
}

/// Drains unread peer bytes into a tiny fixed buffer for a brief window after
/// sending a Close frame so closing the socket does not emit a TCP RST that
/// discards the Close frame before the peer reads it.
pub(crate) async fn drain_peer_after_close(reader: &mut BoxRead) {
    let mut scratch = [0u8; 4096];
    let _done = tokio::time::timeout(std::time::Duration::from_millis(50), async {
        while matches!(reader.read(&mut scratch).await, Ok(n) if n > 0) {}
    })
    .await;
}

// ---------------------------------------------------------------------------
// Wire protocol types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HelloRequest {
    hello: HelloToken,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HelloToken {
    token: String,
}

#[derive(Serialize)]
struct HelloResponse<'a> {
    hello: HelloInfo<'a>,
}

#[derive(Serialize)]
struct HelloInfo<'a> {
    version: &'a str,
    platform: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RpcRequest {
    id: u32,
    call: String,
    args: Map<String, Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArgs {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DictationActArgs {
    action: Action,
    text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LibraryIdArgs {
    id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsSetArgs {
    settings: Settings,
}

#[derive(Serialize)]
struct RpcOk<T: Serialize> {
    id: u32,
    ok: T,
}

#[derive(Serialize)]
struct RpcErr<'a> {
    id: Option<u32>,
    err: &'a str,
}

#[derive(Serialize)]
struct EventEnvelope<'a, T: Serialize> {
    event: &'a str,
    payload: T,
}

#[derive(Serialize)]
struct WebInputDevices {
    default: &'static str,
    others: &'static [&'static str],
}

enum OutgoingFrame {
    Text(String),
    Pong(Vec<u8>),
    Close(u16, &'static str),
}

/// Constant-time comparison for shared-token verification.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= usize::from(*x ^ *y);
    }
    diff == 0
}

fn serialize_ok<T: Serialize>(id: u32, value: T) -> String {
    serde_json::to_string(&RpcOk { id, ok: value }).unwrap_or_default()
}

fn serialize_err(id: Option<u32>, message: &str) -> String {
    serde_json::to_string(&RpcErr { id, err: message }).unwrap_or_default()
}

fn serialize_event<T: Serialize>(event: &str, payload: T) -> String {
    serde_json::to_string(&EventEnvelope { event, payload }).unwrap_or_default()
}

/// Formats `hub_event` if it passes the bootstrap filter: discards levels at or
/// before `boundary_seq` and snapshots strictly older than `bootstrap_revision`,
/// preserving equal revisions and newer snapshots.
pub(crate) fn format_filtered_event(
    hub_event: &HubEvent,
    boundary_seq: u64,
    bootstrap_revision: u64,
) -> Option<String> {
    match &hub_event.event {
        Event::Level(level) => {
            if hub_event.seq <= boundary_seq {
                None
            } else {
                Some(serialize_event(LEVEL_EVENT, level))
            }
        }
        Event::State(snapshot) => {
            if snapshot.revision < bootstrap_revision {
                None
            } else {
                Some(serialize_event(DICTATION_EVENT, snapshot))
            }
        }
    }
}

pub(crate) fn phase_name(phase: Phase) -> &'static str {
    match phase {
        Phase::Idle => "idle",
        Phase::Ready => "ready",
        Phase::Recording => "recording",
        Phase::Paused => "paused",
        Phase::Transcribing => "transcribing",
        Phase::Finished => "finished",
        Phase::Failed => "failed",
    }
}

pub(crate) fn action_name(action: Action) -> &'static str {
    match action {
        Action::New => "new",
        Action::Record => "record",
        Action::Pause => "pause",
        Action::Resume => "resume",
        Action::Done => "done",
        Action::Discard => "discard",
        Action::Retry => "retry",
        Action::Copy => "copy",
        Action::Accept => "accept",
        Action::DownloadModel => "downloadModel",
        Action::UndoLearn => "undoLearn",
    }
}

/// Runs a blocking engine call off Tokio worker threads.
pub(crate) async fn run_blocking<T: Send + 'static>(
    f: Box<dyn FnOnce() -> T + Send>,
) -> Result<T, &'static str> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| "The engine has shut down.")
}

// ---------------------------------------------------------------------------
// Connection lifecycle
// ---------------------------------------------------------------------------

async fn authenticate_connection(
    state: &ServerState,
    conn_id: u64,
    reader: &mut WsReader,
    write_half: &mut BoxWrite,
    shutdown_rx: &mut watch::Receiver<bool>,
) -> bool {
    let zero_audio = AudioCounts::default();
    loop {
        let next = match race2(wait_true(shutdown_rx), reader.read_message()).await {
            Either::Left(()) => {
                let _closed =
                    ws_write_close(write_half, CLOSE_GOING_AWAY, "server shutting down").await;
                state.diagnostics.emit(&DiagnosticEvent::ConnectionClosed {
                    conn_id,
                    reason: "server_shutdown",
                    audio: zero_audio,
                });
                return false;
            }
            Either::Right(msg) => msg,
        };
        match next {
            Ok(WsIncoming::Ping(payload)) => {
                if ws_write_frame(write_half, OP_PONG, &payload).await.is_err() {
                    state.diagnostics.emit(&DiagnosticEvent::ConnectionClosed {
                        conn_id,
                        reason: "writer_error",
                        audio: zero_audio,
                    });
                    return false;
                }
            }
            Ok(WsIncoming::Text(text)) => match validate_hello(&text, &state.auth) {
                Ok(()) => return true,
                Err(reason) => {
                    let _closed =
                        ws_write_close(write_half, CLOSE_UNAUTHORIZED, "unauthorized").await;
                    emit_auth_reject(state, conn_id, reason, "auth_rejected");
                    return false;
                }
            },
            Ok(WsIncoming::Binary(_)) => {
                let _closed =
                    ws_write_close(write_half, CLOSE_UNAUTHORIZED, "expected hello").await;
                emit_auth_reject(state, conn_id, "unexpected_binary", "auth_rejected");
                return false;
            }
            Ok(WsIncoming::Close) => {
                let _closed = ws_write_close(write_half, CLOSE_NORMAL, "").await;
                emit_auth_reject(state, conn_id, "closed_before_hello", "client_close");
                return false;
            }
            Ok(WsIncoming::Eof) => {
                emit_auth_reject(state, conn_id, "closed_before_hello", "eof");
                return false;
            }
            Err(err) => {
                let (close_code, reason) = close_for_read_error(&err);
                if let Some(code) = close_code {
                    let _closed = ws_write_close(write_half, code, reason).await;
                    drain_peer_after_close(&mut reader.reader).await;
                }
                emit_auth_reject(state, conn_id, reason, reason);
                return false;
            }
        }
    }
}

fn emit_auth_reject(
    state: &ServerState,
    conn_id: u64,
    auth_reason: &'static str,
    close: &'static str,
) {
    state.diagnostics.emit(&DiagnosticEvent::Auth {
        conn_id,
        outcome: "rejected",
        reason: auth_reason,
    });
    state.diagnostics.emit(&DiagnosticEvent::ConnectionClosed {
        conn_id,
        reason: close,
        audio: AudioCounts::default(),
    });
}

pub(crate) async fn handle_connection(
    state: ServerState,
    conn_id: u64,
    read_half: BoxRead,
    mut write_half: BoxWrite,
) {
    let mut reader = WsReader::new(read_half);
    let mut shutdown_rx = state.shutdown_rx();
    let mut audio_counts = AudioCounts::default();

    if !authenticate_connection(
        &state,
        conn_id,
        &mut reader,
        &mut write_half,
        &mut shutdown_rx,
    )
    .await
    {
        return;
    }

    state.diagnostics.emit(&DiagnosticEvent::Auth {
        conn_id,
        outcome: "accepted",
        reason: "ok",
    });

    // Bootstrap: subscribe BEFORE sampling state, then send hello + Snapshot.
    let sub = state.hub.subscribe(conn_id);
    state.hooks.before_sample(conn_id).await;
    let boundary_seq = state.hub.current_seq();
    let engine_for_bootstrap = state.engine.clone();
    let bootstrap = run_blocking(Box::new(move || engine_for_bootstrap.state())).await;

    let Ok(bootstrap) = send_bootstrap_result(&mut write_half, bootstrap).await else {
        state.diagnostics.emit(&DiagnosticEvent::WriterOutcome {
            conn_id,
            reason: "writer_error",
        });
        state.diagnostics.emit(&DiagnosticEvent::ConnectionClosed {
            conn_id,
            reason: "writer_error",
            audio: audio_counts,
        });
        return;
    };

    let (reply_tx, reply_rx) = mpsc::channel::<OutgoingFrame>(SUBSCRIBER_QUEUE_CAPACITY);
    let (writer_done_tx, mut writer_done_rx) = watch::channel(false);
    let writer_handle = tokio::spawn(run_writer(WriterContext {
        conn_id,
        write_half,
        reply_rx,
        sub,
        boundary_seq,
        bootstrap_revision: bootstrap.revision,
        shutdown_rx: state.shutdown_rx(),
        writer_done_tx,
        diagnostics: state.diagnostics.clone(),
        hooks: Arc::clone(&state.hooks),
    }));

    let reader_reason = run_reader_loop(
        &state,
        conn_id,
        &mut reader,
        &reply_tx,
        &mut writer_done_rx,
        &mut shutdown_rx,
        &mut audio_counts,
    )
    .await;

    drop(reply_tx);
    let writer_reason = writer_handle.await.unwrap_or("writer_error");
    if matches!(reader_reason, Some("frame_too_large" | "transport_error")) {
        drain_peer_after_close(&mut reader.reader).await;
    }
    release_owner_on_disconnect(&state, conn_id).await;
    let final_reason = reader_reason.unwrap_or(writer_reason);
    state.diagnostics.emit(&DiagnosticEvent::ConnectionClosed {
        conn_id,
        reason: final_reason,
        audio: audio_counts,
    });
}

pub(crate) async fn send_bootstrap_result(
    write_half: &mut BoxWrite,
    bootstrap: Result<Snapshot, &'static str>,
) -> Result<Snapshot, ()> {
    let snap = bootstrap.map_err(drop)?;
    send_bootstrap_frames(write_half, &snap)
        .await
        .map_err(drop)?;
    Ok(snap)
}

async fn send_bootstrap_frames(
    write_half: &mut BoxWrite,
    bootstrap: &Snapshot,
) -> std::io::Result<()> {
    let hello_json = serde_json::to_string(&HelloResponse {
        hello: HelloInfo {
            version: SERVER_VERSION,
            platform: "web",
        },
    })
    .unwrap_or_default();
    let bootstrap_json = serialize_event(DICTATION_EVENT, bootstrap);
    ws_write_frame(write_half, OP_TEXT, hello_json.as_bytes()).await?;
    ws_write_frame(write_half, OP_TEXT, bootstrap_json.as_bytes()).await
}

async fn run_reader_loop(
    state: &ServerState,
    conn_id: u64,
    reader: &mut WsReader,
    reply_tx: &mpsc::Sender<OutgoingFrame>,
    writer_done_rx: &mut watch::Receiver<bool>,
    shutdown_rx: &mut watch::Receiver<bool>,
    audio_counts: &mut AudioCounts,
) -> Option<&'static str> {
    loop {
        let next = match race2(
            wait_true(writer_done_rx),
            race2(wait_true(shutdown_rx), reader.read_message()),
        )
        .await
        {
            Either::Left(()) => return None,
            Either::Right(Either::Left(())) => return Some("server_shutdown"),
            Either::Right(Either::Right(msg)) => msg,
        };
        match next {
            Ok(WsIncoming::Ping(payload)) => {
                let _sent = reply_tx.send(OutgoingFrame::Pong(payload)).await;
            }
            Ok(WsIncoming::Text(text)) => {
                let reply = dispatch_text_rpc(state, conn_id, &text).await;
                let _sent = reply_tx.send(OutgoingFrame::Text(reply)).await;
            }
            Ok(WsIncoming::Binary(bytes)) => {
                if let Some(err_json) = handle_pcm(state, conn_id, &bytes, audio_counts).await {
                    let _sent = reply_tx.send(OutgoingFrame::Text(err_json)).await;
                }
            }
            Ok(WsIncoming::Close) => {
                let _sent = reply_tx.send(OutgoingFrame::Close(CLOSE_NORMAL, "")).await;
                return Some("client_close");
            }
            Ok(WsIncoming::Eof) => return Some("eof"),
            Err(err) => {
                let (close_code, reason) = close_for_read_error(&err);
                if let Some(code) = close_code {
                    let _sent = reply_tx.send(OutgoingFrame::Close(code, reason)).await;
                }
                return Some(reason);
            }
        }
    }
}

struct WriterContext {
    conn_id: u64,
    write_half: BoxWrite,
    reply_rx: mpsc::Receiver<OutgoingFrame>,
    sub: Subscription,
    boundary_seq: u64,
    bootstrap_revision: u64,
    shutdown_rx: watch::Receiver<bool>,
    writer_done_tx: watch::Sender<bool>,
    diagnostics: Diagnostics,
    hooks: Arc<dyn ServerHooks>,
}

enum WriterStep {
    Overflow,
    Shutdown,
    Reply(Option<OutgoingFrame>),
    Event(Option<Box<HubEvent>>),
}

async fn wait_true(rx: &mut watch::Receiver<bool>) {
    let _done = rx.wait_for(|&v| v).await;
}

async fn run_writer(mut ctx: WriterContext) -> &'static str {
    let reason = loop {
        let step = match race2(
            race2(
                wait_true(&mut ctx.sub.overflow_rx),
                wait_true(&mut ctx.shutdown_rx),
            ),
            race2(ctx.reply_rx.recv(), ctx.sub.rx.recv()),
        )
        .await
        {
            Either::Left(Either::Left(())) => WriterStep::Overflow,
            Either::Left(Either::Right(())) => WriterStep::Shutdown,
            Either::Right(Either::Left(maybe_reply)) => WriterStep::Reply(maybe_reply),
            Either::Right(Either::Right(maybe_ev)) => WriterStep::Event(maybe_ev.map(Box::new)),
        };
        match step {
            WriterStep::Overflow => {
                let _closed =
                    ws_write_close(&mut ctx.write_half, CLOSE_GOING_AWAY, "subscriber lagged")
                        .await;
                break "subscriber_lag";
            }
            WriterStep::Shutdown => {
                let _closed = ws_write_close(
                    &mut ctx.write_half,
                    CLOSE_GOING_AWAY,
                    "server shutting down",
                )
                .await;
                break "server_shutdown";
            }
            WriterStep::Reply(None) | WriterStep::Event(None) => break "writer_closed",
            WriterStep::Reply(Some(frame)) => {
                let write_res = match frame {
                    OutgoingFrame::Text(t) => {
                        ws_write_frame(&mut ctx.write_half, OP_TEXT, t.as_bytes()).await
                    }
                    OutgoingFrame::Pong(p) => {
                        ws_write_frame(&mut ctx.write_half, OP_PONG, &p).await
                    }
                    OutgoingFrame::Close(code, msg) => {
                        let _closed = ws_write_close(&mut ctx.write_half, code, msg).await;
                        break "writer_closed";
                    }
                };
                if write_res.is_err() {
                    break "writer_error";
                }
            }
            WriterStep::Event(Some(ev)) => {
                let Some(json) =
                    format_filtered_event(&ev, ctx.boundary_seq, ctx.bootstrap_revision)
                else {
                    continue;
                };
                let write_fut = async {
                    ctx.hooks.before_event_write(ctx.conn_id).await;
                    ws_write_frame(&mut ctx.write_half, OP_TEXT, json.as_bytes()).await
                };
                let outcome = match race2(
                    race2(
                        wait_true(&mut ctx.sub.overflow_rx),
                        wait_true(&mut ctx.shutdown_rx),
                    ),
                    write_fut,
                )
                .await
                {
                    Either::Left(Either::Left(())) => Some("subscriber_lag"),
                    Either::Left(Either::Right(())) => Some("server_shutdown"),
                    Either::Right(res) => res.err().map(|_| "writer_error"),
                };
                if let Some(stop_reason) = outcome {
                    break stop_reason;
                }
            }
        }
    };
    ctx.diagnostics.emit(&DiagnosticEvent::WriterOutcome {
        conn_id: ctx.conn_id,
        reason,
    });
    let _sent = ctx.writer_done_tx.send(true);
    reason
}

fn validate_hello(text: &str, auth: &AuthMode) -> Result<(), &'static str> {
    let Ok(hello) = serde_json::from_str::<HelloRequest>(text) else {
        return Err("invalid_hello");
    };
    if constant_time_eq(
        hello.hello.token.as_bytes(),
        auth.expected_token().as_bytes(),
    ) {
        Ok(())
    } else {
        Err("bad_token")
    }
}

fn close_for_read_error(err: &WsReadError) -> (Option<u16>, &'static str) {
    match err {
        WsReadError::TooBig => (Some(CLOSE_MESSAGE_TOO_BIG), "frame_too_large"),
        WsReadError::ProtocolError => (Some(CLOSE_PROTOCOL_ERROR), "transport_error"),
        WsReadError::InvalidUtf8 => (Some(CLOSE_INVALID_PAYLOAD), "transport_error"),
        WsReadError::Io => (None, "transport_error"),
    }
}

// ---------------------------------------------------------------------------
// RPC dispatch & PCM handling
// ---------------------------------------------------------------------------

pub(crate) async fn dispatch_text_rpc(state: &ServerState, conn_id: u64, text: &str) -> String {
    let Ok(raw) = serde_json::from_str::<Value>(text) else {
        return serialize_err(None, "Invalid JSON frame.");
    };
    let recovered_id = raw
        .as_object()
        .and_then(|obj| obj.get("id"))
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok());
    let Ok(req) = serde_json::from_value::<RpcRequest>(raw) else {
        return serialize_err(recovered_id, "Invalid request envelope.");
    };
    let id = req.id;
    let args = Value::Object(req.args);
    match req.call.as_str() {
        "app_version" => {
            if serde_json::from_value::<EmptyArgs>(args).is_err() {
                return serialize_err(Some(id), "Invalid arguments for app_version.");
            }
            serialize_ok(id, SERVER_VERSION)
        }
        "input_devices" => {
            if serde_json::from_value::<EmptyArgs>(args).is_err() {
                return serialize_err(Some(id), "Invalid arguments for input_devices.");
            }
            serialize_ok(
                id,
                WebInputDevices {
                    default: "Browser microphone",
                    others: &[],
                },
            )
        }
        "window_fullscreen" => {
            if serde_json::from_value::<EmptyArgs>(args).is_err() {
                return serialize_err(Some(id), "Invalid arguments for window_fullscreen.");
            }
            serialize_ok(id, true)
        }
        "vocabulary_open"
        | "learned_vocabulary_open"
        | "pick_vocabulary_file"
        | "pick_model_folder" => {
            if serde_json::from_value::<EmptyArgs>(args).is_err() {
                return serialize_err(Some(id), "Invalid arguments.");
            }
            serialize_err(Some(id), NOT_AVAILABLE_ERR)
        }
        other => dispatch_engine_call(state, conn_id, id, other, args).await,
    }
}

async fn dispatch_engine_call(
    state: &ServerState,
    conn_id: u64,
    id: u32,
    call: &str,
    args: Value,
) -> String {
    let result = match call {
        "dictation_state" => {
            if serde_json::from_value::<EmptyArgs>(args).is_err() {
                Err("Invalid arguments for dictation_state.".to_string())
            } else {
                let engine = state.engine.clone();
                run_blocking(Box::new(move || engine.state()))
                    .await
                    .map(|s| serialize_ok(id, s))
                    .map_err(str::to_string)
            }
        }
        "dictation_act" => match serde_json::from_value::<DictationActArgs>(args) {
            Ok(parsed) => run_dictation_act(state, conn_id, parsed.action, parsed.text)
                .await
                .map(|s| serialize_ok(id, s)),
            Err(_) => Err("Invalid arguments for dictation_act.".to_string()),
        },
        "library_list" => {
            if serde_json::from_value::<EmptyArgs>(args).is_err() {
                Err("Invalid arguments for library_list.".to_string())
            } else {
                let engine = state.engine.clone();
                run_blocking(Box::new(move || engine.library()))
                    .await
                    .map(|entries| serialize_ok(id, entries))
                    .map_err(str::to_string)
            }
        }
        "library_get" => match serde_json::from_value::<LibraryIdArgs>(args) {
            Ok(parsed) => {
                let engine = state.engine.clone();
                run_blocking(Box::new(move || engine.library_transcript(&parsed.id)))
                    .await
                    .map_err(str::to_string)
                    .and_then(|res| res.map(|t| serialize_ok(id, t)).map_err(|e| e.to_string()))
            }
            Err(_) => Err("Invalid arguments for library_get.".to_string()),
        },
        "library_copy" => match serde_json::from_value::<LibraryIdArgs>(args) {
            Ok(parsed) => {
                let engine = state.engine.clone();
                run_blocking(Box::new(move || engine.library_copy(&parsed.id)))
                    .await
                    .map_err(str::to_string)
                    .and_then(|res| {
                        res.map(|()| serialize_ok(id, Value::Null))
                            .map_err(|e| e.to_string())
                    })
            }
            Err(_) => Err("Invalid arguments for library_copy.".to_string()),
        },
        "settings_get" => {
            if serde_json::from_value::<EmptyArgs>(args).is_err() {
                Err("Invalid arguments for settings_get.".to_string())
            } else {
                let engine = state.engine.clone();
                run_blocking(Box::new(move || engine.settings()))
                    .await
                    .map(|s| serialize_ok(id, s))
                    .map_err(str::to_string)
            }
        }
        "settings_set" => match serde_json::from_value::<SettingsSetArgs>(args) {
            Ok(parsed) => {
                let engine = state.engine.clone();
                run_blocking(Box::new(move || engine.set_settings(parsed.settings)))
                    .await
                    .map_err(str::to_string)
                    .and_then(|res| res.map(|s| serialize_ok(id, s)).map_err(|e| e.to_string()))
            }
            Err(_) => Err("Invalid arguments for settings_set.".to_string()),
        },
        "ollama_models" => {
            if serde_json::from_value::<EmptyArgs>(args).is_err() {
                Err("Invalid arguments for ollama_models.".to_string())
            } else {
                let engine = state.engine.clone();
                run_blocking(Box::new(move || engine.ollama_models()))
                    .await
                    .map_err(str::to_string)
                    .and_then(|res| res.map(|m| serialize_ok(id, m)).map_err(|e| e.to_string()))
            }
        }
        _ => Err("Unknown call.".to_string()),
    };
    match result {
        Ok(ok_json) => ok_json,
        Err(msg) => serialize_err(Some(id), &msg),
    }
}

pub(crate) fn apply_ownership_transition(
    owner: &mut Option<u64>,
    conn_id: u64,
    action: Action,
    phase: Phase,
) -> Option<&'static str> {
    if matches!(action, Action::Record | Action::Resume) && phase == Phase::Recording {
        *owner = Some(conn_id);
        Some("granted")
    } else if phase != Phase::Recording && owner.is_some() {
        *owner = None;
        Some("cleared")
    } else {
        None
    }
}

async fn run_dictation_act(
    state: &ServerState,
    conn_id: u64,
    action: Action,
    text: String,
) -> Result<Snapshot, String> {
    let mut gate = state.session_gate.lock().await;
    if !gate.accepting {
        return Err("The engine has shut down.".to_string());
    }
    let engine = state.engine.clone();
    let outcome = run_blocking(Box::new(move || engine.act(action, &text))).await?;
    match outcome {
        Ok(snapshot) => {
            if let Some(outcome_label) =
                apply_ownership_transition(&mut gate.owner, conn_id, action, snapshot.phase)
            {
                state.diagnostics.emit(&DiagnosticEvent::Ownership {
                    conn_id,
                    outcome: outcome_label,
                    action: action_name(action),
                    phase: phase_name(snapshot.phase),
                });
            }
            Ok(snapshot)
        }
        Err(e) => Err(e.to_string()),
    }
}

enum PcmOutcome {
    Accepted(u64),
    OutOfPhase,
    NonOwner,
}

pub(crate) async fn handle_pcm(
    state: &ServerState,
    conn_id: u64,
    bytes: &[u8],
    audio_counts: &mut AudioCounts,
) -> Option<String> {
    if !bytes.len().is_multiple_of(2) {
        audio_counts.invalid_odd_frames = audio_counts.invalid_odd_frames.saturating_add(1);
        return Some(serialize_err(
            None,
            "Invalid PCM16 frame: byte length must be even.",
        ));
    }
    let gate = state.session_gate.lock().await;
    if !gate.accepting {
        return None;
    }
    let is_owner = gate.owner == Some(conn_id);
    let engine = state.engine.clone();
    let recorder = state.recorder.clone();
    let payload = bytes.to_vec();
    let outcome = run_blocking(Box::new(move || {
        if engine.state().phase != Phase::Recording {
            return PcmOutcome::OutOfPhase;
        }
        if !is_owner {
            return PcmOutcome::NonOwner;
        }
        let (pairs, _) = payload.as_chunks::<2>();
        let samples: Vec<i16> = pairs.iter().map(|&pair| i16::from_le_bytes(pair)).collect();
        let count = u64::try_from(samples.len()).unwrap_or(u64::MAX);
        recorder.push_pcm16(&samples);
        PcmOutcome::Accepted(count)
    }))
    .await
    .unwrap_or(PcmOutcome::OutOfPhase);
    drop(gate);
    match outcome {
        PcmOutcome::Accepted(samples) => {
            audio_counts.accepted_frames = audio_counts.accepted_frames.saturating_add(1);
            audio_counts.accepted_samples = audio_counts.accepted_samples.saturating_add(samples);
        }
        PcmOutcome::OutOfPhase => {
            audio_counts.dropped_out_of_phase_frames =
                audio_counts.dropped_out_of_phase_frames.saturating_add(1);
        }
        PcmOutcome::NonOwner => {
            audio_counts.dropped_nonowner_frames =
                audio_counts.dropped_nonowner_frames.saturating_add(1);
        }
    }
    None
}

async fn release_owner_on_disconnect(state: &ServerState, conn_id: u64) {
    let mut gate = state.session_gate.lock().await;
    if gate.owner != Some(conn_id) {
        return;
    }
    gate.owner = None;
    let engine = state.engine.clone();
    let phase = tokio::task::spawn_blocking(move || engine.state().phase)
        .await
        .unwrap_or(Phase::Recording);
    state.diagnostics.emit(&DiagnosticEvent::Ownership {
        conn_id,
        outcome: "released",
        action: "disconnect",
        phase: phase_name(phase),
    });
}
