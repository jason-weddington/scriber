//! Remote audio: a [`Recorder`] fed pushed 16 kHz mono samples instead of a
//! device. It is a second thin adapter over the core's [`Tap`] and
//! [`feeder`], so segmenting, the silence split and the level meter behave as
//! they do for the microphone.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use scriber_core::level::{Tap, feeder};
use scriber_core::model::InputDevices;
use scriber_core::ports::{Captured, PortError, Recorder, fail};
use scriber_core::sync::lock;

use crate::LevelSink;

/// The sample rate of pushed audio.
pub const STREAM_SAMPLE_RATE: u32 = 16_000;
/// The channel count of pushed audio.
pub const STREAM_CHANNELS: u16 = 1;
/// The device name answered when the settings name none.
pub const STREAM_DEFAULT_LABEL: &str = "Remote microphone";

type Split = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct Shared {
    capture: Mutex<Option<Arc<Mutex<Tap>>>>,
    label: Mutex<Option<String>>,
    level: Mutex<Option<LevelSink>>,
    split: Mutex<Option<Split>>,
    dropped: AtomicU64,
    pushed: AtomicU64,
}

/// A recorder that opens no device: the app pushes 16 kHz mono samples with
/// [`Self::push`] or [`Self::push_pcm16`]. Clones share one capture, so the
/// app keeps a handle to push into while the engine owns another.
///
/// `push` runs the level sink and the split notifier inline on the calling
/// thread, before it returns. The notifier only spawns the scriber-split
/// thread, so pushing never takes the session lock. A push racing a `stop`
/// that lands in the already-taken capture is discarded and not counted.
#[derive(Clone, Default)]
pub struct StreamRecorder {
    shared: Arc<Shared>,
}

impl StreamRecorder {
    /// A recorder that is not started.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Where the input level goes; replaces an earlier sink. Until it is
    /// called, levels go nowhere. [`crate::Engine::open`] calls it; callers
    /// that build `Adapters` through [`crate::Engine::with_adapters`] call it
    /// inside `make` with the sink they are given. One reading per push at
    /// most: the loudest completed 800-sample window of the block.
    pub fn with_level(&self, level: LevelSink) {
        *lock(&self.shared.level) = Some(level);
    }

    /// Feeds 16 kHz mono samples in -1..1. While not started they are
    /// discarded and counted in [`Self::dropped_samples`].
    pub fn push(&self, samples: &[f32]) {
        let count = u64::try_from(samples.len()).unwrap_or(u64::MAX);
        let _seen = self.shared.pushed.fetch_add(count, Ordering::Relaxed);
        let tap = lock(&self.shared.capture).clone();
        let level = lock(&self.shared.level).clone();
        let notify = lock(&self.shared.split).clone();
        let Some(tap) = tap else {
            let _lost = self.shared.dropped.fetch_add(count, Ordering::Relaxed);
            return;
        };
        let report = move |l: f64| {
            if let Some(level) = &level {
                level(l);
            }
        };
        let split = move || {
            if let Some(notify) = &notify {
                notify();
            }
        };
        let mut deliver = feeder::<()>(tap, report, split);
        deliver(samples, &());
    }

    /// [`Self::push`] for 16-bit PCM: each sample is divided by 32768.
    pub fn push_pcm16(&self, samples: &[i16]) {
        let converted: Vec<f32> = samples.iter().map(|s| f32::from(*s) / 32768.0).collect();
        self.push(&converted);
    }

    /// Samples pushed while not started, plus untaken samples discarded when
    /// `start` replaced a live capture. Cumulative, never reset.
    #[must_use]
    pub fn dropped_samples(&self) -> u64 {
        self.shared.dropped.load(Ordering::Relaxed)
    }

    /// Every sample pushed, kept or dropped. Cumulative, never reset.
    #[must_use]
    pub fn pushed_samples(&self) -> u64 {
        self.shared.pushed.load(Ordering::Relaxed)
    }
}

impl Recorder for StreamRecorder {
    /// Opens nothing and never fails. `device` (or [`STREAM_DEFAULT_LABEL`]
    /// when empty) is the answer. A live capture it replaces has its untaken
    /// samples added to [`StreamRecorder::dropped_samples`].
    fn start(&self, device: &str, split_ms: u64) -> Result<String, PortError> {
        let label = if device.is_empty() {
            STREAM_DEFAULT_LABEL.to_string()
        } else {
            device.to_string()
        };
        let fresh = Arc::new(Mutex::new(Tap::new(
            STREAM_CHANNELS,
            STREAM_SAMPLE_RATE,
            split_ms,
        )));
        let old = lock(&self.shared.capture).replace(fresh);
        if let Some(old) = old {
            let lost = lock(&old).take().samples.len();
            let lost = u64::try_from(lost).unwrap_or(u64::MAX);
            let _was = self.shared.dropped.fetch_add(lost, Ordering::Relaxed);
        }
        *lock(&self.shared.label) = Some(label.clone());
        Ok(label)
    }

    fn inputs(&self) -> Result<InputDevices, PortError> {
        let label = lock(&self.shared.label)
            .clone()
            .unwrap_or_else(|| STREAM_DEFAULT_LABEL.to_string());
        Ok(InputDevices::new(Some(label.clone()), vec![label]))
    }

    fn stop(&self) -> Result<Captured, PortError> {
        let tap = lock(&self.shared.capture)
            .take()
            .ok_or(fail("not recording"))?;
        Ok(lock(&tap).take())
    }

    fn take_closed(&self) -> Vec<Captured> {
        let tap = lock(&self.shared.capture).clone();
        tap.map(|tap| lock(&tap).take_closed()).unwrap_or_default()
    }

    fn on_split(&self, notify: Arc<dyn Fn() + Send + Sync>) {
        *lock(&self.shared.split) = Some(notify);
    }
}
