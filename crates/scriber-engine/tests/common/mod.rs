//! Fakes for the engine's adapters. Each is `Clone` over shared state, so a
//! test keeps a copy to look at while the engine owns another.

use std::collections::VecDeque;
use std::io::{Cursor, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use scriber_core::sync::LoadOnce;
use scriber_engine::{
    Adapters, Captured, Clipboard, Clock, Downloader, Encoder, Engine, EngineConfig, Event,
    LevelSink, LocalTime, PortError, Recorder, Snapshot, Summarizer, Transcriber,
};

/// How long a test waits for anything.
pub(crate) const PATIENCE: Duration = Duration::from_secs(5);

/// A microphone that captures one second of loud sound per segment and
/// reports a level of 0.25 when it starts.
#[derive(Clone, Default)]
pub(crate) struct FakeRecorder {
    level: Arc<Mutex<Option<LevelSink>>>,
    pub(crate) starts: Arc<AtomicU32>,
    pub(crate) stops: Arc<AtomicU32>,
}

impl Recorder for FakeRecorder {
    fn start(&self) -> Result<(), PortError> {
        let _before = self.starts.fetch_add(1, Ordering::SeqCst);
        if let Some(level) = self
            .level
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            level(0.25);
        }
        Ok(())
    }

    fn stop(&self) -> Result<Captured, PortError> {
        let _before = self.stops.fetch_add(1, Ordering::SeqCst);
        Ok(Captured {
            samples: vec![0.5; 16_000],
            channels: 1,
            sample_rate: 16_000,
        })
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

    fn transcribe(&self, _samples: &[f32], _prompt: Option<&str>) -> Result<String, PortError> {
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
}

impl FakeSummarizer {
    /// Waits for a message on the returned sender before each answer.
    pub(crate) fn gated() -> (Self, Sender<()>) {
        let (tx, rx) = mpsc::channel();
        let gated = Self {
            gate: Some(Arc::new(Mutex::new(rx))),
        };
        (gated, tx)
    }
}

impl Summarizer for FakeSummarizer {
    fn summarize(&self, _transcript: &str) -> Result<String, PortError> {
        if let Some(gate) = &self.gate {
            // A closed gate (the test ended) lets the answer through.
            let _opened = gate.lock().unwrap_or_else(PoisonError::into_inner).recv();
        }
        Ok("A dictated note.".to_string())
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
        let (encoder, summarizer) = (self.encoder.clone(), self.summarizer.clone());
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
