//! Fakes for the engine's adapters. Each is `Clone` over shared state, so a
//! test keeps a copy to look at while the engine owns another.

use std::collections::VecDeque;
use std::io::{Cursor, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use scriber_engine::{
    Adapters, Captured, Downloader, Engine, EngineConfig, Event, LevelSink, PortError, Recorder,
    Snapshot, Transcriber,
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
/// is missing while `model` names a file that does not exist.
#[derive(Clone, Default)]
pub(crate) struct FakeTranscriber {
    pub(crate) model: Option<PathBuf>,
    pub(crate) answers: Arc<Mutex<VecDeque<Result<String, String>>>>,
    pub(crate) gate: Option<Arc<Mutex<Receiver<()>>>>,
    pub(crate) warms: Arc<AtomicU32>,
    pub(crate) dropped: DropFlag,
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
}

impl Transcriber for FakeTranscriber {
    fn missing_model(&self) -> Option<String> {
        let path = self.model.as_ref()?;
        (!path.is_file()).then(|| path.display().to_string())
    }

    fn warm(&self) -> Result<(), PortError> {
        let _before = self.warms.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn transcribe(&self, _samples: &[f32], _prompt: Option<&str>) -> Result<String, PortError> {
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

/// A network that serves `body` for any URL and counts the requests.
#[derive(Clone, Default)]
pub(crate) struct FakeDownloader {
    pub(crate) body: Vec<u8>,
    pub(crate) opens: Arc<AtomicU32>,
}

impl Downloader for FakeDownloader {
    fn open(&self, _url: &str) -> Result<Box<dyn Read>, PortError> {
        let _before = self.opens.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Cursor::new(self.body.clone())))
    }
}

/// Every fake, and the engine's events.
#[derive(Default)]
pub(crate) struct Fakes {
    pub(crate) recorder: FakeRecorder,
    pub(crate) transcriber: FakeTranscriber,
    pub(crate) downloader: FakeDownloader,
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
