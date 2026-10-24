//! The microphone: cpal's default input device at its default config.

use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use scriber_core::level::{Tap, feeder};
use scriber_core::ports::{Captured, PortError, Recorder, fail};
use scriber_core::sync::lock;

/// The default input device. Every block it delivers is metered
/// ([`scriber_core::level`]) and the level handed to the sink given to
/// [`CpalRecorder::new`], on the audio thread.
pub struct CpalRecorder {
    level: Arc<dyn Fn(f64) + Send + Sync>,
    /// cpal's stream and the capture it feeds, while recording.
    capture: Mutex<Option<(cpal::Stream, Arc<Mutex<Tap>>)>>,
}

impl CpalRecorder {
    /// A recorder that reports the input level to `level`. Opens no device:
    /// only [`Recorder::start`] does.
    #[must_use]
    pub fn new(level: Arc<dyn Fn(f64) + Send + Sync>) -> Self {
        Self {
            level,
            capture: Mutex::default(),
        }
    }
}

impl Recorder for CpalRecorder {
    fn start(&self) -> Result<(), PortError> {
        let (host, none) = (cpal::default_host(), fail("there is no input device"));
        let device = host.default_input_device().ok_or(none)?;
        let config = device.default_input_config().map_err(fail)?;
        let (channels, rate) = (config.channels(), config.sample_rate());
        let tap = Arc::new(Mutex::new(Tap::new(channels, rate)));
        let level = Arc::clone(&self.level);
        let deliver = feeder(Arc::clone(&tap), move |l| level(l));
        // A dropout loses a few milliseconds of sound; the segment goes on.
        let stream = device.build_input_stream(config.into(), deliver, |_| {}, None);
        let stream = stream.map_err(fail)?;
        stream.play().map_err(fail)?;
        *lock(&self.capture) = Some((stream, tap));
        Ok(())
    }

    fn stop(&self) -> Result<Captured, PortError> {
        let (stream, tap) = lock(&self.capture).take().ok_or(fail("not recording"))?;
        drop(stream);
        Ok(lock(&tap).take())
    }
}
