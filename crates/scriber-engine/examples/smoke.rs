//! Real-model smoke check, never run by the gate: transcribes a 16 kHz mono
//! WAV through the engine's Whisper adapter, warming it up first as the
//! engine does at launch, and prints the text and how long each step took.
//! `scripts/smoke.sh` makes the WAV and runs this from the repo root:
//!
//! ```sh
//! cargo run --release -p scriber-engine --example smoke -- <model.bin> <speech.wav>
//! ```

use std::io::Write;
use std::time::Instant;

use scriber_core::audio::decode_wav;
use scriber_core::transcript::{is_silent, keep};
use scriber_core::vocabulary::prompt;
use scriber_engine::{Transcriber, Whisper};

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

    let load = Instant::now();
    whisper.warm()?;
    let load = load.elapsed();
    writeln!(out, "warm-up (model load, as at launch): {load:.2?}")?;

    let first = Instant::now();
    let heard = whisper.transcribe(&samples, None)?;
    let first = first.elapsed();
    writeln!(
        out,
        "first transcription after the warm-up, no prompt: {first:.2?}"
    )?;
    writeln!(out, "text: {:?}", keep(samples.len(), &heard))?;

    let style = prompt("");
    let glossary = prompt("Scriber\ntalos\nagent-gtd");
    let runs = [
        ("style sentence only", &style),
        ("style sentence and a glossary", &glossary),
    ];
    for (label, p) in runs {
        writeln!(out, "prompt: {p:?}")?;
        let start = Instant::now();
        let again = whisper.transcribe(&samples, Some(p.as_str()))?;
        let elapsed = start.elapsed();
        writeln!(out, "{label}: {elapsed:.2?}")?;
        writeln!(out, "text: {:?}", keep(samples.len(), &again))?;
    }
    Ok(())
}
