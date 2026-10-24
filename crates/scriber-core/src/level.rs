//! The live input level the window draws as a waveform while recording.
//!
//! The recorder adapter hands every block of samples the device delivers to
//! a [`Tap`], which keeps them for the segment and meters them: one reading
//! per [`WINDOW_MS`] of sound, the RMS of the window's samples in dBFS,
//! mapped linearly from [`FLOOR_DBFS`] (0.0) to 0 dBFS (1.0) and clamped.
//! Decibels, not linear RMS: normal speech sits around -40 to -20 dBFS,
//! which a linear scale would draw as a nearly flat line.

use std::sync::{Arc, Mutex};

use crate::ports::Captured;
use crate::sync::lock;

/// The length of sound one reading covers, in milliseconds. One reading per
/// window bounds the level event's rate at 1000 / 50 = 20 per second.
pub const WINDOW_MS: u32 = 50;

/// The level drawn as silence: an RMS at or below this many dBFS reads 0.0.
pub const FLOOR_DBFS: f64 = -60.0;

/// An RMS (0 to 1, full scale) as a level from 0.0 to 1.0: its dBFS mapped
/// linearly from [`FLOOR_DBFS`] to 0 dBFS and clamped. Silence, and an RMS
/// that is not a number, read 0.0.
#[must_use]
pub fn level(rms: f64) -> f64 {
    let dbfs = 20.0 * rms.log10();
    let level = (dbfs - FLOOR_DBFS) / -FLOOR_DBFS;
    if level.is_nan() {
        0.0
    } else {
        level.clamp(0.0, 1.0)
    }
}

/// Turns a stream of sample blocks into one level per [`WINDOW_MS`].
#[derive(Debug, Clone, PartialEq)]
pub struct Meter {
    /// Samples per window, all channels counted; 0 when the format has no
    /// channels or too low a rate, and then the meter never reads.
    window: u32,
    /// The sum of the squares of the current window's samples so far.
    squares: f64,
    /// How many samples the current window has so far.
    count: u32,
}

impl Meter {
    /// A meter for interleaved sound with `channels` channels at
    /// `sample_rate` frames per second.
    #[must_use]
    pub fn new(channels: u16, sample_rate: u32) -> Self {
        let frames = sample_rate.saturating_mul(WINDOW_MS) / 1_000;
        Self {
            window: frames.saturating_mul(u32::from(channels)),
            squares: 0.0,
            count: 0,
        }
    }

    /// Meters one block. Returns the loudest window the block completed, if
    /// it completed any: a block can complete several, but the window gets
    /// at most one reading per block and so never more than one per window.
    /// A window's leftover samples carry over to the next block.
    pub fn feed(&mut self, block: &[f32]) -> Option<f64> {
        if self.window == 0 {
            return None;
        }
        let mut loudest: Option<f64> = None;
        for sample in block {
            self.squares += f64::from(*sample) * f64::from(*sample);
            self.count = self.count.saturating_add(1);
            if self.count == self.window {
                let reading = level((self.squares / f64::from(self.count)).sqrt());
                loudest = Some(loudest.map_or(reading, |l| l.max(reading)));
                self.squares = 0.0;
                self.count = 0;
            }
        }
        loudest
    }
}

/// One segment's capture as the device delivers it: every block is kept for
/// the segment and metered for the window.
#[derive(Debug, Clone, PartialEq)]
pub struct Tap {
    samples: Vec<f32>,
    channels: u16,
    sample_rate: u32,
    meter: Meter,
}

impl Tap {
    /// An empty capture of interleaved sound with `channels` channels at
    /// `sample_rate` frames per second.
    #[must_use]
    pub fn new(channels: u16, sample_rate: u32) -> Self {
        Self {
            samples: Vec::new(),
            channels,
            sample_rate,
            meter: Meter::new(channels, sample_rate),
        }
    }

    /// Keeps `block` and meters it; see [`Meter::feed`].
    pub fn feed(&mut self, block: &[f32]) -> Option<f64> {
        self.samples.extend_from_slice(block);
        self.meter.feed(block)
    }

    /// Hands over everything kept so far, leaving the capture empty.
    pub fn take(&mut self) -> Captured {
        Captured::new(
            std::mem::take(&mut self.samples),
            self.channels,
            self.sample_rate,
        )
    }
}

/// The recorder's callback for `tap`: keeps and meters each block the
/// device delivers, and hands each reading to `report` once the tap is
/// unlocked, so a slow report never holds up [`Tap::take`]. `I` is whatever
/// else the device passes with a block; it is ignored.
pub fn feeder<I: 'static>(
    tap: Arc<Mutex<Tap>>,
    report: impl Fn(f64) + Send + 'static,
) -> impl FnMut(&[f32], &I) + Send + 'static {
    move |block, _| {
        let level = lock(&tap).feed(block);
        if let Some(level) = level {
            report(level);
        }
    }
}
