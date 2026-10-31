//! The live input level the window draws as a waveform while recording.
//!
//! The recorder adapter hands every block of samples the device delivers to
//! a [`Tap`], which keeps them for the segment and meters them: one reading
//! per [`WINDOW_MS`] of sound, the RMS of the window's samples in dBFS,
//! mapped linearly from [`FLOOR_DBFS`] (0.0) to [`CEILING_DBFS`] (1.0) and clamped.
//! Decibels, not linear RMS. The ceiling sits near clipping so that only
//! genuinely loud input reads 1.0; close-mic condenser speech that pinned the
//! old -20 dBFS ceiling now draws below the top; and laptop-mic dictation
//! (-41 to -33 dBFS, quiet room near -52) still reads about 0.24 to 0.41.
//!
//! When the silence split is on, the tap also finds the sustained silences
//! that close a segment while recording goes on; see [`Splitter`].

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::ports::Captured;
use crate::sync::lock;

/// The length of sound one reading covers, in milliseconds. One reading per
/// window bounds the level event's rate at 1000 / 50 = 20 per second.
pub const WINDOW_MS: u32 = 50;

/// The level drawn as silence: an RMS at or below this many dBFS reads 0.0.
pub const FLOOR_DBFS: f64 = -52.0;

/// The level drawn full height: an RMS at or above this many dBFS (near clipping over a 50 ms window) reads 1.0.
pub const CEILING_DBFS: f64 = -6.0;

/// An RMS (0 to 1, full scale) as a level from 0.0 to 1.0: its dBFS mapped
/// linearly from [`FLOOR_DBFS`] to [`CEILING_DBFS`] and clamped. Silence, and
/// an RMS that is not a number, read 0.0.
#[must_use]
pub fn level(rms: f64) -> f64 {
    let dbfs = 20.0 * rms.log10();
    let level = (dbfs - FLOOR_DBFS) / (CEILING_DBFS - FLOOR_DBFS);
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
        Self {
            window: window_samples(channels, sample_rate),
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

/// The shortest segment the automatic split may close: a cut never leaves a
/// segment of under this many milliseconds of audio.
pub const MIN_SPLIT_SEGMENT_MS: u64 = 2_000;

/// Samples per window, all channels counted, exactly as [`Meter`] counts
/// them; 0 when the format has no channels or too low a rate.
fn window_samples(channels: u16, sample_rate: u32) -> u32 {
    let frames = sample_rate.saturating_mul(WINDOW_MS) / 1_000;
    frames.saturating_mul(u32::from(channels))
}

/// Whether a window of `samples` samples whose squares sum to
/// `sum_of_squares` is silent: its RMS is at or below
/// [`crate::transcript::TRAILING_SILENCE_RMS`].
#[must_use]
pub fn is_silent_window(sum_of_squares: f64, samples: u32) -> bool {
    sum_of_squares
        <= f64::from(crate::transcript::TRAILING_SILENCE_RMS).powi(2) * f64::from(samples)
}

/// Finds where to close a segment at a sustained silence: after `split_ms`
/// of consecutive silent windows (see [`is_silent_window`]), once the
/// segment has had a window above the threshold and is at least
/// [`MIN_SPLIT_SEGMENT_MS`] long. Windows are counted from the segment's
/// first sample, so a cut always falls on a window boundary.
#[derive(Debug, Clone, PartialEq)]
pub struct Splitter {
    /// Samples per window, all channels counted; 0 never cuts.
    window: u32,
    /// Consecutive silent windows that make a cut; 0 never cuts.
    needed: u64,
    /// The fewest samples a segment may have when it is cut.
    min_len: u64,
    /// The sum of the squares of the current window's samples so far.
    squares: f64,
    /// How many samples the current window has so far.
    count: u32,
    /// Consecutive silent windows just completed.
    run: u64,
    /// Whether the segment has had a window above the threshold.
    heard: bool,
    /// The segment's samples so far.
    len: u64,
}

impl Splitter {
    /// A splitter for interleaved sound with `channels` channels at
    /// `sample_rate` frames per second that cuts after `split_ms` of
    /// silence; 0 never cuts.
    #[must_use]
    pub fn new(channels: u16, sample_rate: u32, split_ms: u64) -> Self {
        Self {
            window: window_samples(channels, sample_rate),
            needed: split_ms.div_ceil(u64::from(WINDOW_MS)),
            min_len: (u64::from(sample_rate).saturating_mul(MIN_SPLIT_SEGMENT_MS) / 1_000)
                .saturating_mul(u64::from(channels)),
            squares: 0.0,
            count: 0,
            run: 0,
            heard: false,
            len: 0,
        }
    }

    /// Starts a fresh segment.
    fn reset(&mut self) {
        self.squares = 0.0;
        self.count = 0;
        self.run = 0;
        self.heard = false;
        self.len = 0;
    }

    /// Takes one sample. Returns true when the segment should be cut after
    /// it, and then starts a fresh segment.
    pub fn push(&mut self, sample: f32) -> bool {
        if self.window == 0 || self.needed == 0 {
            return false;
        }
        self.len = self.len.saturating_add(1);
        self.squares += f64::from(sample) * f64::from(sample);
        self.count = self.count.saturating_add(1);
        if self.count < self.window {
            return false;
        }
        let silent = is_silent_window(self.squares, self.count);
        self.squares = 0.0;
        self.count = 0;
        if !silent {
            self.run = 0;
            self.heard = true;
            return false;
        }
        self.run = self.run.saturating_add(1);
        if self.run >= self.needed && self.heard && self.len >= self.min_len {
            self.reset();
            return true;
        }
        false
    }
}

/// How silent a capture was, for the microphone log.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SilenceProfile {
    /// The longest run of silent windows, in milliseconds.
    pub longest_silence_ms: u64,
    /// The quietest complete window's RMS, rounded to 4 decimal places; 0.0
    /// when there is no complete window.
    pub quietest_rms: f64,
}

/// The [`SilenceProfile`] of `samples`, judged in [`Splitter`]'s windows
/// from the first sample; a trailing partial window is ignored.
#[must_use]
pub fn silence_profile(samples: &[f32], channels: u16, sample_rate: u32) -> SilenceProfile {
    let window = window_samples(channels, sample_rate);
    let mut longest = 0_u64;
    let mut run = 0_u64;
    let mut quietest: Option<f64> = None;
    if window > 0 {
        for chunk in samples.chunks_exact(usize::try_from(window).unwrap_or(usize::MAX)) {
            let squares: f64 = chunk.iter().map(|s| f64::from(*s) * f64::from(*s)).sum();
            let rms = (squares / f64::from(window)).sqrt();
            quietest = Some(quietest.map_or(rms, |q| q.min(rms)));
            if is_silent_window(squares, window) {
                run = run.saturating_add(1);
                longest = longest.max(run);
            } else {
                run = 0;
            }
        }
    }
    SilenceProfile {
        longest_silence_ms: longest.saturating_mul(u64::from(WINDOW_MS)),
        quietest_rms: quietest.map_or(0.0, |q| (q * 10_000.0).round() / 10_000.0),
    }
}

/// One recording's capture as the device delivers it: every block is kept
/// and metered for the window, and the open segment is closed at a sustained
/// silence when the split is on (see [`Splitter`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Tap {
    open: Vec<f32>,
    closed: VecDeque<Vec<f32>>,
    channels: u16,
    sample_rate: u32,
    meter: Meter,
    splitter: Splitter,
}

impl Tap {
    /// An empty capture of interleaved sound with `channels` channels at
    /// `sample_rate` frames per second that closes a segment after
    /// `split_ms` of silence; 0 never splits.
    #[must_use]
    pub fn new(channels: u16, sample_rate: u32, split_ms: u64) -> Self {
        Self {
            open: Vec::new(),
            closed: VecDeque::new(),
            channels,
            sample_rate,
            meter: Meter::new(channels, sample_rate),
            splitter: Splitter::new(channels, sample_rate, split_ms),
        }
    }

    /// Keeps `block`, closing the open segment wherever the splitter cuts,
    /// and meters it; see [`Meter::feed`].
    pub fn feed(&mut self, block: &[f32]) -> Option<f64> {
        for sample in block {
            self.open.push(*sample);
            if self.splitter.push(*sample) {
                self.closed.push_back(std::mem::take(&mut self.open));
            }
        }
        self.meter.feed(block)
    }

    /// How many closed segments are waiting to be taken.
    #[must_use]
    pub fn waiting(&self) -> usize {
        self.closed.len()
    }

    /// Hands over the closed segments, oldest first, leaving the open one.
    pub fn take_closed(&mut self) -> Vec<Captured> {
        self.closed
            .drain(..)
            .map(|samples| Captured::new(samples, self.channels, self.sample_rate))
            .collect()
    }

    /// Hands over everything kept so far, as one capture: the closed
    /// segments not yet taken, in order, then the open one. Leaves the
    /// capture empty and the splitter fresh.
    pub fn take(&mut self) -> Captured {
        let mut samples: Vec<f32> = self.closed.drain(..).flatten().collect();
        samples.append(&mut self.open);
        self.splitter.reset();
        Captured::new(samples, self.channels, self.sample_rate)
    }
}

/// The recorder's callback for `tap`: keeps and meters each block the
/// device delivers, and hands each reading to `report` once the tap is
/// unlocked, so a slow report never holds up [`Tap::take`]. When the block
/// closed a segment, it then calls `split` once, also outside the lock. `I`
/// is whatever else the device passes with a block; it is ignored.
pub fn feeder<I: 'static>(
    tap: Arc<Mutex<Tap>>,
    report: impl Fn(f64) + Send + 'static,
    split: impl Fn() + Send + 'static,
) -> impl FnMut(&[f32], &I) + Send + 'static {
    move |block, _| {
        let (level, grew) = {
            let mut tap = lock(&tap);
            let before = tap.waiting();
            let level = tap.feed(block);
            (level, tap.waiting() > before)
        };
        if let Some(level) = level {
            report(level);
        }
        if grew {
            split();
        }
    }
}
