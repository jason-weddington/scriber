//! Local adapter fakes, loopback HTTP `/api/tags` stub, and raw RFC 6455
//! WebSocket test client for `scriber-server` tests.

use std::collections::VecDeque;
use std::io::{BufRead as _, BufReader, Cursor, Read, Write as _};
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread;
use std::time::Duration;

use scriber_core::cleanup;
use scriber_core::sync::LoadOnce;
use scriber_engine::{
    Adapters, Cleaner, Clock, Downloader, Encoder, Engine, EngineConfig, Event, LexiconSource,
    LocalTime, ModelSource, Phrase, PortError, StreamRecorder, Summarizer, Transcriber, Warmer,
};
use serde_json::Value;
use tokio::io::{
    AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader as AsyncBufReader,
};
use tokio::net::TcpStream;

use crate::server::websocket_accept_key;

/// Fake transcriber capturing every exact sample slice it receives and
/// honoring `LoadOnce` semantics for `unload()`.
#[derive(Clone, Default)]
pub(crate) struct FakeTranscriber {
    /// If set, model is missing while this path is not a file.
    pub(crate) model: Option<PathBuf>,
    /// Queued answers; defaults to `"hello from server"`.
    pub(crate) answers: Arc<Mutex<VecDeque<Result<String, String>>>>,
    /// Exact sample slices passed to each `transcribe` call.
    pub(crate) heard: Arc<Mutex<Vec<Vec<f32>>>>,
    /// Number of successful warm calls.
    pub(crate) warms: Arc<AtomicU32>,
    /// Number of models freed by `unload()`.
    pub(crate) unloads: Arc<AtomicU32>,
    /// Simulated loaded model state.
    pub(crate) once: Arc<LoadOnce<()>>,
}

impl FakeTranscriber {
    pub(crate) fn answering(answers: &[Result<&str, &str>]) -> Self {
        let queue = answers
            .iter()
            .map(|r| r.map(str::to_string).map_err(str::to_string))
            .collect();
        Self {
            answers: Arc::new(Mutex::new(queue)),
            ..Self::default()
        }
    }

    pub(crate) fn heard_samples(&self) -> Vec<Vec<f32>> {
        self.heard
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Transcriber for FakeTranscriber {
    fn missing_model(&self) -> Option<String> {
        let path = self.model.as_ref()?;
        (!path.is_file()).then(|| path.display().to_string())
    }

    fn warm(&self) -> Result<(), PortError> {
        self.once.with(&mut || Ok(()), &mut |()| {
            let _prev = self.warms.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }

    fn transcribe(&self, samples: &[f32], _prompt: Option<&str>) -> Result<Vec<Phrase>, PortError> {
        self.once.with(&mut || Ok(()), &mut |()| {
            self.heard
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(samples.to_vec());
            let next = self
                .answers
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop_front();
            next.unwrap_or_else(|| Ok("hello from server".to_string()))
                .map(|text| {
                    vec![Phrase {
                        text,
                        start: 0,
                        end: samples.len(),
                    }]
                })
                .map_err(PortError)
        })
    }

    fn unload(&self) -> bool {
        let freed = self.once.close();
        if freed {
            let _prev = self.unloads.fetch_add(1, Ordering::SeqCst);
        }
        freed
    }
}

/// Fake downloader serving `body` and recording requested URLs.
#[derive(Clone, Default)]
pub(crate) struct FakeDownloader {
    pub(crate) body: Vec<u8>,
    pub(crate) opens: Arc<AtomicU32>,
}

impl Downloader for FakeDownloader {
    fn open(&self, _url: &str) -> Result<Box<dyn Read>, PortError> {
        let _prev = self.opens.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Cursor::new(self.body.clone())))
    }
}

/// Fixed local clock for deterministic library titles and IDs.
#[derive(Clone, Default)]
pub(crate) struct FakeClock;

impl Clock for FakeClock {
    fn now(&self) -> LocalTime {
        LocalTime {
            year: 2026,
            month: 10,
            day: 26,
            hour: 14,
            minute: 3,
            second: 9,
        }
    }
}

/// No-op encoder.
#[derive(Clone, Default)]
pub(crate) struct FakeEncoder;

impl Encoder for FakeEncoder {
    fn to_m4a(&self, _wav: &str, _m4a: &str) -> Result<(), PortError> {
        Ok(())
    }
}

/// Deterministic summarizer.
#[derive(Clone, Default)]
pub(crate) struct FakeSummarizer;

impl Summarizer for FakeSummarizer {
    fn summarize(&self, _model: &str, _transcript: &str) -> Result<String, PortError> {
        Ok("Server summary.".to_string())
    }
}

/// Pass-through cleaner.
#[derive(Clone, Default)]
pub(crate) struct FakeCleaner;

impl Cleaner for FakeCleaner {
    fn clean(
        &self,
        _model: &str,
        _system: &str,
        _request: &str,
    ) -> Result<cleanup::Reply, PortError> {
        Ok(cleanup::Reply::new(String::new()))
    }
}

/// No-op warmer.
#[derive(Clone, Default)]
pub(crate) struct FakeWarmer;

impl Warmer for FakeWarmer {
    fn warm(&self, _model: &str) -> Result<(), PortError> {
        Ok(())
    }

    fn release(&self, _model: &str) -> Result<(), PortError> {
        Ok(())
    }
}

/// Bundle of local adapter fakes wired to a real `StreamRecorder` and `Engine::with_adapters`.
#[derive(Clone, Default)]
pub(crate) struct FakeEngineBundle {
    pub(crate) transcriber: FakeTranscriber,
    pub(crate) downloader: FakeDownloader,
    pub(crate) model_source: Option<ModelSource>,
    pub(crate) ollama_url: Option<String>,
}

impl FakeEngineBundle {
    pub(crate) fn open(
        &self,
        data_dir: PathBuf,
        recorder: StreamRecorder,
        events: Arc<dyn Fn(Event) + Send + Sync>,
    ) -> Engine {
        let mut config = EngineConfig {
            recorder: Some(recorder.clone()),
            clipboard: None,
            library: true,
            lexicon: LexiconSource::None,
            ..EngineConfig::new(data_dir)
        };
        if let Some(source) = &self.model_source {
            config.model = source.clone();
        }
        if let Some(url) = &self.ollama_url {
            config.ollama_url = url.clone();
        }
        let transcriber = self.transcriber.clone();
        let downloader = self.downloader.clone();
        Engine::with_adapters(
            config,
            move |level| {
                recorder.with_level(level);
                Adapters {
                    recorder: Box::new(recorder),
                    transcriber: Box::new(transcriber),
                    downloader: Box::new(downloader),
                    clock: Box::new(FakeClock),
                    encoder: Box::new(FakeEncoder),
                    summarizer: Box::new(FakeSummarizer),
                    cleaner: Box::new(FakeCleaner),
                    warmer: Arc::new(FakeWarmer),
                }
            },
            events,
        )
    }
}

/// Loopback HTTP server answering one `GET /api/tags` request, optionally
/// pausing on `gate` after receiving the request so tests can prove another
/// peer stays responsive during a blocking RPC.
pub(crate) fn spawn_http_stub(
    response: &'static str,
    gate: Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let _handle = thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 && line != "\r\n" {
                line.clear();
            }
            if let Some((entered, release)) = gate {
                let _sent = entered.send(());
                let _released = release.recv();
            }
            let _written = reader.get_mut().write_all(response.as_bytes());
        }
    });
    format!("http://127.0.0.1:{port}")
}

/// Returns a loopback URL with no listener.
pub(crate) fn closed_loopback_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    format!("http://127.0.0.1:{port}")
}

/// Generates `count` samples of a loud 440 Hz sine wave as LE `i16`.
#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::as_conversions,
    reason = "synthetic test sine wave generation"
)]
pub(crate) fn loud_pcm16(count: usize) -> Vec<i16> {
    (0..count)
        .map(|i| {
            let phase = 2.0 * std::f32::consts::PI * 440.0 * (i as f32) / 16_000.0;
            let sample = 0.5 * phase.sin();
            (sample * 32_767.0).round() as i16
        })
        .collect()
}

/// Encodes `i16` samples into little-endian PCM16 bytes.
pub(crate) fn pcm16_le_bytes(samples: &[i16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len().saturating_mul(2));
    for &s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// Sends a raw HTTP/1.1 GET request over TCP and returns `(status_code, headers, body)`.
pub(crate) async fn http_get(addr: SocketAddr, path: &str) -> (u16, String, Vec<u8>) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    let _read = stream.read_to_end(&mut raw).await.unwrap();
    let sep = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&raw[..sep]).into_owned();
    let body = raw[sep.saturating_add(4)..].to_vec();
    let status = head
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse::<u16>()
        .unwrap();
    (status, head, body)
}

/// Server-to-client WebSocket frame received by [`TestWsClient`].
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ClientFrame {
    Text(String),
    Pong(Vec<u8>),
    Close(Option<u16>),
    Eof,
}

/// Minimal RFC 6455 WebSocket client over real loopback TCP for testing exact
/// wire frames, fragmented messages, control frames and close codes.
pub(crate) struct TestWsClient {
    stream: AsyncBufReader<TcpStream>,
}

impl TestWsClient {
    /// Connects to `ws://<addr>/ws` and verifies HTTP `101` + `Sec-WebSocket-Accept`.
    pub(crate) async fn connect(addr: SocketAddr) -> Self {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let key = "AAAAAAAAAAAAAAAAAAAAAA==";
        let req = format!(
            "GET /ws HTTP/1.1\r\n\
             Host: {addr}\r\n\
             Upgrade: websocket\r\n\
             Connection: keep-alive, Upgrade\r\n\
             Sec-WebSocket-Key: {key}\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n"
        );
        stream.write_all(req.as_bytes()).await.unwrap();
        let mut reader = AsyncBufReader::new(stream);
        let mut status_line = String::new();
        let _status_len = reader.read_line(&mut status_line).await.unwrap();
        assert!(
            status_line.starts_with("HTTP/1.1 101 "),
            "expected 101 Switching Protocols, got {status_line}"
        );
        let mut saw_accept = false;
        let expected_accept = websocket_accept_key(key);
        loop {
            let mut line = String::new();
            let _hdr_len = reader.read_line(&mut line).await.unwrap();
            if line == "\r\n" || line.is_empty() {
                break;
            }
            if let Some((k, v)) = line.split_once(':')
                && k.trim().eq_ignore_ascii_case("sec-websocket-accept")
            {
                assert_eq!(v.trim(), expected_accept, "Sec-WebSocket-Accept matches");
                saw_accept = true;
            }
        }
        assert!(saw_accept, "missing Sec-WebSocket-Accept");
        Self { stream: reader }
    }

    /// Sends a masked client frame with arbitrary FIN, RSV, opcode and payload.
    pub(crate) async fn send_raw_frame(
        &mut self,
        fin: bool,
        rsv: u8,
        opcode: u8,
        masked: bool,
        payload: &[u8],
    ) {
        let mut out = Vec::with_capacity(payload.len().saturating_add(14));
        out.push((u8::from(fin) << 7) | (rsv & 0x70) | (opcode & 0x0F));
        let mask_bit = if masked { 0x80 } else { 0x00 };
        if let Ok(short) = u8::try_from(payload.len())
            && short <= 125
        {
            out.push(mask_bit | short);
        } else if let Ok(med) = u16::try_from(payload.len()) {
            out.push(mask_bit | 0x7E);
            out.extend_from_slice(&med.to_be_bytes());
        } else {
            out.push(mask_bit | 0x7F);
            out.extend_from_slice(&u64::try_from(payload.len()).unwrap().to_be_bytes());
        }
        if masked {
            let mask = [0x37, 0xFA, 0x21, 0x3D];
            out.extend_from_slice(&mask);
            for (i, &b) in payload.iter().enumerate() {
                out.push(b ^ mask[i & 3]);
            }
        } else {
            out.extend_from_slice(payload);
        }
        self.stream.get_mut().write_all(&out).await.unwrap();
        self.stream.get_mut().flush().await.unwrap();
    }

    pub(crate) async fn send_text(&mut self, text: &str) {
        self.send_raw_frame(true, 0, 1, true, text.as_bytes()).await;
    }

    pub(crate) async fn send_json(&mut self, value: &Value) {
        let text = serde_json::to_string(value).unwrap();
        self.send_text(&text).await;
    }

    pub(crate) async fn send_binary(&mut self, data: &[u8]) {
        self.send_raw_frame(true, 0, 2, true, data).await;
    }

    pub(crate) async fn send_ping(&mut self, data: &[u8]) {
        self.send_raw_frame(true, 0, 9, true, data).await;
    }

    pub(crate) async fn send_pong(&mut self, data: &[u8]) {
        self.send_raw_frame(true, 0, 10, true, data).await;
    }

    pub(crate) async fn send_close(&mut self, code: u16) {
        self.send_raw_frame(true, 0, 8, true, &code.to_be_bytes())
            .await;
    }

    /// Reads the next server-to-client frame with a timeout.
    pub(crate) async fn recv(&mut self) -> ClientFrame {
        tokio::time::timeout(Duration::from_secs(5), self.recv_raw())
            .await
            .expect("timed out waiting for server frame")
    }

    async fn recv_raw(&mut self) -> ClientFrame {
        let mut head = [0u8; 2];
        match self.stream.read_exact(&mut head).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return ClientFrame::Eof,
            Err(_) => return ClientFrame::Eof,
        }
        let opcode = head[0] & 0x0F;
        let len7 = head[1] & 0x7F;
        let len: usize = match len7 {
            0..=125 => usize::from(len7),
            126 => {
                let mut ext = [0u8; 2];
                let _ext_len = self.stream.read_exact(&mut ext).await.unwrap();
                usize::from(u16::from_be_bytes(ext))
            }
            _ => {
                let mut ext = [0u8; 8];
                let _ext_len = self.stream.read_exact(&mut ext).await.unwrap();
                usize::try_from(u64::from_be_bytes(ext)).unwrap()
            }
        };
        let mut payload = vec![0u8; len];
        if self.stream.read_exact(&mut payload).await.is_err() {
            return ClientFrame::Eof;
        }
        match opcode {
            1 => ClientFrame::Text(String::from_utf8(payload).unwrap()),
            8 => {
                let code = if payload.len() >= 2 {
                    Some(u16::from_be_bytes([payload[0], payload[1]]))
                } else {
                    None
                };
                ClientFrame::Close(code)
            }
            10 => ClientFrame::Pong(payload),
            other => panic!("unexpected server opcode {other}"),
        }
    }

    /// Reads frames until a `Close` or `Eof` arrives (skipping any async
    /// post-auth broadcast events like model warmup `dictation-changed`).
    pub(crate) async fn recv_close(&mut self) -> ClientFrame {
        loop {
            match self.recv().await {
                ClientFrame::Text(_) => {}
                frame @ (ClientFrame::Pong(_) | ClientFrame::Close(_) | ClientFrame::Eof) => {
                    return frame;
                }
            }
        }
    }

    /// Reads frames until a `Pong` arrives (skipping any async post-auth
    /// broadcast events like model warmup `dictation-changed`).
    pub(crate) async fn recv_pong(&mut self) -> ClientFrame {
        self.recv_close().await
    }

    /// Reads the next JSON text frame, panicking if the server closes or sends non-text.
    pub(crate) async fn recv_json(&mut self) -> Value {
        match self.recv().await {
            ClientFrame::Text(t) => serde_json::from_str(&t).unwrap(),
            ClientFrame::Pong(p) => panic!("expected text JSON frame, got Pong({p:?})"),
            ClientFrame::Close(code) => panic!("expected text JSON frame, got Close({code:?})"),
            ClientFrame::Eof => panic!("expected text JSON frame, got Eof"),
        }
    }

    /// Completes `hello` auth and returns `(hello_reply, bootstrap_event)`.
    pub(crate) async fn hello(&mut self, token: &str) -> (Value, Value) {
        self.send_json(&serde_json::json!({ "hello": { "token": token } }))
            .await;
        let hello = self.recv_json().await;
        let bootstrap = self.recv_json().await;
        (hello, bootstrap)
    }

    /// Sends one RPC and reads frames until the reply with matching `id` arrives,
    /// returning `(reply, interleaved_events)`.
    pub(crate) async fn call(&mut self, id: u32, call: &str, args: Value) -> (Value, Vec<Value>) {
        self.send_json(&serde_json::json!({ "id": id, "call": call, "args": args }))
            .await;
        let mut events = Vec::new();
        loop {
            let msg = self.recv_json().await;
            if msg.get("id").and_then(Value::as_u64) == Some(u64::from(id)) {
                return (msg, events);
            }
            events.push(msg);
        }
    }
}
