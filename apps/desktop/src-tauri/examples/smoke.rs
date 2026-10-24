//! Real-model smoke check, never run by the gate: transcribes a 16 kHz mono
//! WAV through the app's own Whisper adapter and prints the text and how long
//! it took. `scripts/smoke.sh` makes the WAV and runs this:
//!
//! ```sh
//! cargo run --release --example smoke -- <model.bin> <speech.wav>
//! ```

use std::io::Write;
use std::time::Instant;

use scriber_app_lib::Whisper;
use scriber_core::audio::decode_wav;
use scriber_core::ports::Transcriber;
use scriber_core::transcript::{is_silent, keep};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let usage = "usage: smoke <model.bin> <speech.wav>";
    let (model, wav) = (args.next().ok_or(usage)?, args.next().ok_or(usage)?);
    let samples = decode_wav(&std::fs::read(wav)?)?;
    let seconds = f64::from(u32::try_from(samples.len())?) / 16_000.0;
    let whisper = Whisper::new(model.into());
    let mut out = std::io::stdout();
    writeln!(
        out,
        "audio: {seconds:.1} s, silent: {}",
        is_silent(&samples)
    )?;

    let cold = Instant::now();
    let first = whisper.transcribe(&samples, None)?;
    let cold = cold.elapsed();
    writeln!(out, "cold (model load + transcribe): {cold:.2?}")?;
    writeln!(out, "text: {:?}", keep(samples.len(), &first))?;

    let warm = Instant::now();
    let second = whisper.transcribe(&samples, Some("Glossary: Scriber, talos, agent-gtd"))?;
    let warm = warm.elapsed();
    writeln!(out, "warm, with a glossary prompt: {warm:.2?}")?;
    writeln!(out, "text: {:?}", keep(samples.len(), &second))?;
    Ok(())
}
