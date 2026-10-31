//! Fakes for the engine's adapters. Each is `Clone` over shared state, so a
//! test keeps a copy to look at while the engine owns another.

use std::collections::VecDeque;
use std::io::{BufRead as _, BufReader, Cursor, Read, Write as _};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use scriber_core::sync::LoadOnce;
use scriber_engine::{
    Adapters, Captured, Cleaner, Clipboard, Clock, Downloader, Encoder, Engine, EngineConfig,
    Event, InputDevices, LevelSink, LocalTime, Phrase, PortError, Recorder, Snapshot, Summarizer,
    Transcriber, Warmer,
};

/// How long a test waits for anything.
pub(crate) const PATIENCE: Duration = Duration::from_secs(5);

/// The recorder's split notifier.
type Notify = Arc<dyn Fn() + Send + Sync>;

/// A microphone that captures one second of loud sound per segment and
/// reports a level of 0.25 when it starts. It logs each device asked for in
/// `opened` and opens it ("Fake Mic" for the default); it lists "Fake Mic"
/// as the default and "Other Mic", or fails with `inputs_error`. It logs
/// each start's silence split in `splits`; [`FakeRecorder::close_at_silence`]
/// plays a split.
#[derive(Clone, Default)]
pub(crate) struct FakeRecorder {
    level: Arc<Mutex<Option<LevelSink>>>,
    pub(crate) starts: Arc<AtomicU32>,
    pub(crate) stops: Arc<AtomicU32>,
    pub(crate) opened: Arc<Mutex<Vec<String>>>,
    pub(crate) inputs_error: Arc<Mutex<Option<String>>>,
    pub(crate) splits: Arc<Mutex<Vec<u64>>>,
    closed: Arc<Mutex<VecDeque<Captured>>>,
    notify: Arc<Mutex<Option<Notify>>>,
}

impl FakeRecorder {
    /// The capture closes `captured` at a silence: it waits to be taken and
    /// the split notifier, if one is set, is called.
    pub(crate) fn close_at_silence(&self, captured: Captured) {
        self.closed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(captured);
        let notify = self
            .notify
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(notify) = notify {
            notify();
        }
    }
}

impl Recorder for FakeRecorder {
    fn start(&self, device: &str, split_ms: u64) -> Result<String, PortError> {
        let _before = self.starts.fetch_add(1, Ordering::SeqCst);
        self.splits
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(split_ms);
        self.opened
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(device.to_string());
        if let Some(level) = self
            .level
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            level(0.25);
        }
        Ok(if device.is_empty() {
            "Fake Mic".to_string()
        } else {
            device.to_string()
        })
    }

    fn inputs(&self) -> Result<InputDevices, PortError> {
        match self
            .inputs_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        {
            Some(e) => Err(PortError(e)),
            None => Ok(InputDevices::new(
                Some("Fake Mic".to_string()),
                ["Other Mic".to_string()],
            )),
        }
    }

    fn stop(&self) -> Result<Captured, PortError> {
        let _before = self.stops.fetch_add(1, Ordering::SeqCst);
        Ok(Captured {
            samples: vec![0.5; 16_000],
            channels: 1,
            sample_rate: 16_000,
        })
    }

    fn take_closed(&self) -> Vec<Captured> {
        self.closed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain(..)
            .collect()
    }

    fn on_split(&self, notify: Arc<dyn Fn() + Send + Sync>) {
        *self.notify.lock().unwrap_or_else(PoisonError::into_inner) = Some(notify);
    }
}

/// A transcriber that answers from a queue ("hello world" when it is empty),
/// optionally waiting for a message on `gate` before each answer. Its model
/// is missing while `model` names a file that does not exist. Its "model"
/// is a [`LoadOnce`], so `unload` behaves like the real adapter's: it waits
/// for a transcription in progress, and nothing runs afterwards.
#[derive(Clone, Default)]
pub(crate) struct FakeTranscriber {
    pub(crate) model: Option<PathBuf>,
    pub(crate) answers: Arc<Mutex<VecDeque<Result<String, String>>>>,
    pub(crate) gate: Option<Arc<Mutex<Receiver<()>>>>,
    pub(crate) warms: Arc<AtomicU32>,
    pub(crate) dropped: DropFlag,
    pub(crate) once: Arc<LoadOnce<()>>,
    /// Models freed by `unload`.
    pub(crate) unloads: Arc<AtomicU32>,
    /// Calls to `warm` or `transcribe`, refused or not.
    pub(crate) asks: Arc<AtomicU32>,
    /// Told when a transcription has the model, before its gate.
    pub(crate) entered: Option<Arc<Mutex<Sender<()>>>>,
}

impl FakeTranscriber {
    /// Answers `answers` in turn.
    pub(crate) fn answering(answers: &[Result<&str, &str>]) -> Self {
        let queue = answers
            .iter()
            .map(|a| a.map(str::to_string).map_err(str::to_string))
            .collect();
        Self {
            answers: Arc::new(Mutex::new(queue)),
            ..Self::default()
        }
    }

    /// Waits for a message on the returned sender before each answer.
    pub(crate) fn gated(self) -> (Self, Sender<()>) {
        let (tx, rx) = mpsc::channel();
        let gated = Self {
            gate: Some(Arc::new(Mutex::new(rx))),
            ..self
        };
        (gated, tx)
    }

    /// Tells the returned receiver each time a transcription has the model.
    pub(crate) fn entering(self) -> (Self, Receiver<()>) {
        let (tx, rx) = mpsc::channel();
        let entering = Self {
            entered: Some(Arc::new(Mutex::new(tx))),
            ..self
        };
        (entering, rx)
    }
}

impl Transcriber for FakeTranscriber {
    fn missing_model(&self) -> Option<String> {
        let path = self.model.as_ref()?;
        (!path.is_file()).then(|| path.display().to_string())
    }

    fn warm(&self) -> Result<(), PortError> {
        let _asked = self.asks.fetch_add(1, Ordering::SeqCst);
        self.once.with(&mut || Ok(()), &mut |()| {
            let _before = self.warms.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }

    fn transcribe(&self, samples: &[f32], _prompt: Option<&str>) -> Result<Vec<Phrase>, PortError> {
        let _asked = self.asks.fetch_add(1, Ordering::SeqCst);
        self.once.with(&mut || Ok(()), &mut |()| {
            if let Some(entered) = &self.entered {
                let _sent = entered
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .send(());
            }
            if let Some(gate) = &self.gate {
                // A closed gate (the test ended) lets the answer through.
                let _opened = gate.lock().unwrap_or_else(PoisonError::into_inner).recv();
            }
            let next = self
                .answers
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop_front();
            next.unwrap_or_else(|| Ok("hello world".to_string()))
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
            let _before = self.unloads.fetch_add(1, Ordering::SeqCst);
        }
        freed
    }
}

/// Set when any copy of it is dropped.
#[derive(Clone, Default)]
pub(crate) struct DropFlag(Arc<AtomicBool>);

impl DropFlag {
    /// Whether a copy has been dropped.
    pub(crate) fn is_set(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// A network that serves `body` for any URL and counts the requests,
/// optionally waiting for a message on `gate` before answering.
#[derive(Clone, Default)]
pub(crate) struct FakeDownloader {
    pub(crate) body: Vec<u8>,
    pub(crate) opens: Arc<AtomicU32>,
    pub(crate) gate: Option<Arc<Mutex<Receiver<()>>>>,
}

impl FakeDownloader {
    /// Waits for a message on the returned sender before answering.
    pub(crate) fn gated(self) -> (Self, Sender<()>) {
        let (tx, rx) = mpsc::channel();
        let gated = Self {
            gate: Some(Arc::new(Mutex::new(rx))),
            ..self
        };
        (gated, tx)
    }
}

impl Downloader for FakeDownloader {
    fn open(&self, _url: &str) -> Result<Box<dyn Read>, PortError> {
        let _before = self.opens.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = &self.gate {
            // A closed gate (the test ended) lets the answer through.
            let _opened = gate.lock().unwrap_or_else(PoisonError::into_inner).recv();
        }
        Ok(Box::new(Cursor::new(self.body.clone())))
    }
}

/// A clock that always reads 2026-10-26 14:03:09.
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

/// An encoder that counts its calls and writes nothing.
#[derive(Clone, Default)]
pub(crate) struct FakeEncoder {
    pub(crate) calls: Arc<AtomicU32>,
}

impl Encoder for FakeEncoder {
    fn to_m4a(&self, _wav: &str, _m4a: &str) -> Result<(), PortError> {
        let _before = self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// A summarizer that answers "A dictated note.", optionally waiting for a
/// message on `gate` first.
#[derive(Clone, Default)]
pub(crate) struct FakeSummarizer {
    pub(crate) gate: Option<Arc<Mutex<Receiver<()>>>>,
    /// The model each call named.
    pub(crate) models: Arc<Mutex<Vec<String>>>,
}

impl FakeSummarizer {
    /// Waits for a message on the returned sender before each answer.
    pub(crate) fn gated() -> (Self, Sender<()>) {
        let (tx, rx) = mpsc::channel();
        let gated = Self {
            gate: Some(Arc::new(Mutex::new(rx))),
            models: Arc::default(),
        };
        (gated, tx)
    }
}

impl Summarizer for FakeSummarizer {
    fn summarize(&self, model: &str, _transcript: &str) -> Result<String, PortError> {
        self.models
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(model.to_string());
        if let Some(gate) = &self.gate {
            // A closed gate (the test ended) lets the answer through.
            let _opened = gate.lock().unwrap_or_else(PoisonError::into_inner).recv();
        }
        Ok("A dictated note.".to_string())
    }
}

/// A cleanup model that answers `reply` (an empty reply unless set) and
/// counts its calls.
#[derive(Clone)]
pub(crate) struct FakeCleaner {
    pub(crate) reply: Arc<Mutex<Result<String, String>>>,
    pub(crate) calls: Arc<AtomicUsize>,
    /// The model each call named.
    pub(crate) models: Arc<Mutex<Vec<String>>>,
}

impl Default for FakeCleaner {
    fn default() -> Self {
        Self {
            reply: Arc::new(Mutex::new(Ok(String::new()))),
            calls: Arc::default(),
            models: Arc::default(),
        }
    }
}

impl Cleaner for FakeCleaner {
    fn clean(&self, model: &str, _system: &str, _request: &str) -> Result<String, PortError> {
        self.models
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(model.to_string());
        let _before = self.calls.fetch_add(1, Ordering::SeqCst);
        self.reply
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .map_err(PortError)
    }
}

/// A warmer that records each call on entry, as `("warm", model)` or
/// `("release", model)`, fails both while `fail` is set, and optionally waits
/// for a message on a gate before answering.
#[derive(Clone, Default)]
pub(crate) struct FakeWarmer {
    pub(crate) calls: Arc<Mutex<Vec<(String, String)>>>,
    pub(crate) fail: Arc<AtomicBool>,
    pub(crate) warm_gate: Option<Arc<Mutex<Receiver<()>>>>,
    pub(crate) release_gate: Option<Arc<Mutex<Receiver<()>>>>,
}

impl FakeWarmer {
    /// Each warm waits for a message on the returned sender.
    pub(crate) fn gated_warm(self) -> (Self, Sender<()>) {
        let (tx, rx) = mpsc::channel();
        let gated = Self {
            warm_gate: Some(Arc::new(Mutex::new(rx))),
            ..self
        };
        (gated, tx)
    }

    /// Each release waits for a message on the returned sender.
    pub(crate) fn gated_release(self) -> (Self, Sender<()>) {
        let (tx, rx) = mpsc::channel();
        let gated = Self {
            release_gate: Some(Arc::new(Mutex::new(rx))),
            ..self
        };
        (gated, tx)
    }

    /// A copy of the calls so far.
    pub(crate) fn seen(&self) -> Vec<(String, String)> {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn call(
        &self,
        kind: &str,
        model: &str,
        gate: Option<&Arc<Mutex<Receiver<()>>>>,
    ) -> Result<(), PortError> {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((kind.to_string(), model.to_string()));
        if let Some(gate) = gate {
            // A closed gate (the test ended) lets the answer through.
            let _opened = gate.lock().unwrap_or_else(PoisonError::into_inner).recv();
        }
        if self.fail.load(Ordering::SeqCst) {
            Err(PortError("refused".to_string()))
        } else {
            Ok(())
        }
    }
}

impl Warmer for FakeWarmer {
    fn warm(&self, model: &str) -> Result<(), PortError> {
        self.call("warm", model, self.warm_gate.as_ref())
    }

    fn release(&self, model: &str) -> Result<(), PortError> {
        self.call("release", model, self.release_gate.as_ref())
    }
}

/// A clipboard that keeps every write.
#[derive(Clone, Default)]
pub(crate) struct RecordingClipboard {
    pub(crate) writes: Arc<Mutex<Vec<String>>>,
}

impl Clipboard for RecordingClipboard {
    fn write(&self, text: &str) -> Result<(), PortError> {
        self.writes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(text.to_string());
        Ok(())
    }
}

/// Every fake, and the engine's events.
#[derive(Default)]
pub(crate) struct Fakes {
    pub(crate) recorder: FakeRecorder,
    pub(crate) transcriber: FakeTranscriber,
    pub(crate) downloader: FakeDownloader,
    pub(crate) encoder: FakeEncoder,
    pub(crate) summarizer: FakeSummarizer,
    pub(crate) cleaner: FakeCleaner,
    pub(crate) warmer: FakeWarmer,
}

impl Fakes {
    /// Fakes whose transcriber is `transcriber`.
    pub(crate) fn with(transcriber: FakeTranscriber) -> Self {
        Self {
            transcriber,
            ..Self::default()
        }
    }

    /// An engine over clones of the fakes, and its events.
    pub(crate) fn open(&self, config: EngineConfig) -> (Engine, Receiver<Event>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let (recorder, transcriber, downloader) = (
            self.recorder.clone(),
            self.transcriber.clone(),
            self.downloader.clone(),
        );
        let (encoder, summarizer, cleaner) = (
            self.encoder.clone(),
            self.summarizer.clone(),
            self.cleaner.clone(),
        );
        let warmer = self.warmer.clone();
        let engine = Engine::with_adapters(
            config,
            move |level| {
                *recorder
                    .level
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = Some(level);
                Adapters {
                    recorder: Box::new(recorder),
                    transcriber: Box::new(transcriber),
                    downloader: Box::new(downloader),
                    clock: Box::new(FakeClock),
                    encoder: Box::new(encoder),
                    summarizer: Box::new(summarizer),
                    cleaner: Box::new(cleaner),
                    warmer: Arc::new(warmer),
                }
            },
            events(tx),
        );
        (engine, rx)
    }
}

/// An events callback that forwards to `tx`.
pub(crate) fn events(tx: Mutex<Sender<Event>>) -> Arc<dyn Fn(Event) + Send + Sync> {
    Arc::new(move |e| {
        let _sent = tx.lock().unwrap_or_else(PoisonError::into_inner).send(e);
    })
}

/// The first snapshot from `rx` that `want` accepts, within [`PATIENCE`].
pub(crate) fn until(
    rx: &Receiver<Event>,
    want: impl Fn(&Snapshot) -> bool,
) -> Result<Snapshot, RecvTimeoutError> {
    let started = Instant::now();
    loop {
        match rx.recv_timeout(PATIENCE.saturating_sub(started.elapsed()))? {
            Event::State(s) if want(&s) => return Ok(s),
            Event::State(_) | Event::Level(_) => {}
        }
    }
}

/// A server at the returned URL that answers one request with the raw HTTP
/// `response`, after reading the request's head.
#[cfg(test)]
pub(crate) fn serve_once(response: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let _server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        while reader.read_line(&mut line).unwrap_or(0) > 0 && line != "\r\n" {
            line.clear();
        }
        let _written = reader.get_mut().write_all(response.as_bytes());
    });
    format!("http://127.0.0.1:{port}")
}

/// A URL where nothing listens.
#[cfg(test)]
pub(crate) fn closed_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    format!("http://127.0.0.1:{port}")
}
