//! The microphone: the chosen cpal input device (or the default one) at
//! its default config, and the list of input devices.

use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use scriber_core::level::{Tap, feeder};
use scriber_core::model::InputDevices;
use scriber_core::ports::{Captured, PortError, Recorder, fail};
use scriber_core::sync::lock;

/// The chosen input device, or the default one. Every block it delivers is metered
/// ([`scriber_core::level`]) and the level handed to the sink given to
/// [`CpalRecorder::new`], on the audio thread.
pub struct CpalRecorder {
    level: Arc<dyn Fn(f64) + Send + Sync>,
    /// cpal's stream and the capture it feeds, while recording.
    capture: Mutex<Option<(cpal::Stream, Arc<Mutex<Tap>>)>>,
    /// Called on the audio thread when the capture closes a segment at a
    /// silence; set by [`Recorder::on_split`].
    split: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl CpalRecorder {
    /// A recorder that reports the input level to `level`. Opens no device:
    /// only [`Recorder::start`] does.
    #[must_use]
    pub fn new(level: Arc<dyn Fn(f64) + Send + Sync>) -> Self {
        Self {
            level,
            capture: Mutex::default(),
            split: Mutex::default(),
        }
    }
}

impl Recorder for CpalRecorder {
    fn start(&self, device: &str, split_ms: u64) -> Result<String, PortError> {
        let host = cpal::default_host();
        let device = if device.is_empty() {
            host.default_input_device()
                .ok_or(fail("there is no input device"))?
        } else {
            host.input_devices()
                .map_err(fail)?
                .find(|d| d.description().is_ok_and(|n| n.name() == device))
                .ok_or_else(|| fail(format!("there is no input device named {device}")))?
        };
        // A name that cannot be read never stops a recording.
        let name = device.description().map_or_else(
            |_| "Unknown microphone".to_string(),
            |d| d.name().to_string(),
        );
        let config = device.default_input_config().map_err(fail)?;
        let (channels, rate) = (config.channels(), config.sample_rate());
        let tap = Arc::new(Mutex::new(Tap::new(channels, rate, split_ms)));
        let level = Arc::clone(&self.level);
        let notify = lock(&self.split).clone();
        let split = move || {
            if let Some(notify) = &notify {
                notify();
            }
        };
        let deliver = feeder(Arc::clone(&tap), move |l| level(l), split);
        // A dropout loses a few milliseconds of sound; the segment goes on.
        let stream = device.build_input_stream(config.into(), deliver, |_| {}, None);
        let stream = stream.map_err(fail)?;
        stream.play().map_err(fail)?;
        *lock(&self.capture) = Some((stream, tap));
        Ok(name)
    }

    fn inputs(&self) -> Result<InputDevices, PortError> {
        let host = cpal::default_host();
        let name = |d: &cpal::Device| d.description().ok().map(|d| d.name().to_string());
        let default_device = host.default_input_device().as_ref().and_then(name);
        let names: Vec<String> = host
            .input_devices()
            .map_err(fail)?
            .filter_map(|d| name(&d))
            .collect();
        Ok(InputDevices::new(default_device, names))
    }

    fn stop(&self) -> Result<Captured, PortError> {
        let (stream, tap) = lock(&self.capture).take().ok_or(fail("not recording"))?;
        drop(stream);
        Ok(lock(&tap).take())
    }

    fn take_closed(&self) -> Vec<Captured> {
        lock(&self.capture)
            .as_ref()
            .map(|(_, tap)| lock(tap).take_closed())
            .unwrap_or_default()
    }

    fn on_split(&self, notify: Arc<dyn Fn() + Send + Sync>) {
        *lock(&self.split) = Some(notify);
    }
}
