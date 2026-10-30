//! Real-model smoke check, never run by the gate: transcribes a 16 kHz mono
//! WAV through the engine's Whisper adapter, warming it up first as the
//! engine does at launch (trimming trailing silence and applying `kept_text` as
//! the app does), and prints the text and how long each step took.
//! `scripts/smoke.sh` makes the WAV and runs this from the repo root:
//!
//! ```sh
//! cargo run --release -p scriber-engine --example smoke -- <model.bin> <speech.wav>
//! ```

use std::io::Write;
use std::time::Instant;

use scriber_core::audio::decode_wav;
use scriber_core::transcript::{
    is_silent, kept_text, phantom, phrase_audio, trim_trailing_silence,
};
use scriber_core::vocabulary::prompt;
use scriber_engine::{Phrase, Transcriber, Whisper};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let usage = "usage: smoke <model.bin> <speech.wav>";
    let (model, wav) = (args.next().ok_or(usage)?, args.next().ok_or(usage)?);
    let samples = decode_wav(&std::fs::read(wav)?)?;
    let seconds = f64::from(u32::try_from(samples.len())?) / 16_000.0;
    let trimmed = trim_trailing_silence(&samples);
    let trimmed_seconds = f64::from(u32::try_from(trimmed.len())?) / 16_000.0;
    let whisper = Whisper::new(model.into());
    let mut out = std::io::stdout();
    writeln!(
        out,
        "audio: {seconds:.1} s, trimmed to {trimmed_seconds:.1} s, silent: {}",
        is_silent(&samples)
    )?;

    let load = Instant::now();
    whisper.warm()?;
    let load = load.elapsed();
    writeln!(out, "warm-up (model load, as at launch): {load:.2?}")?;

    let first = Instant::now();
    let heard = whisper.transcribe(trimmed, None)?;
    let first = first.elapsed();
    writeln!(
        out,
        "first transcription after the warm-up, no prompt: {first:.2?}"
    )?;
    print_phrases(&mut out, trimmed, &heard)?;
    writeln!(out, "text: {:?}", kept_text(trimmed, &heard))?;

    let style = prompt("");
    let glossary = prompt("Scriber\ntalos\nagent-gtd");
    let runs = [
        ("style sentence only", &style),
        ("style sentence and a glossary", &glossary),
    ];
    for (label, p) in runs {
        writeln!(out, "prompt: {p:?}")?;
        let start = Instant::now();
        let again = whisper.transcribe(trimmed, Some(p.as_str()))?;
        let elapsed = start.elapsed();
        writeln!(out, "{label}: {elapsed:.2?}")?;
        print_phrases(&mut out, trimmed, &again)?;
        writeln!(out, "text: {:?}", kept_text(trimmed, &again))?;
    }
    Ok(())
}

/// One line per phrase: its span in seconds, whether its own audio is
/// silent, whether it is a phantom, and its text.
fn print_phrases(
    out: &mut impl Write,
    trimmed: &[f32],
    phrases: &[Phrase],
) -> Result<(), Box<dyn std::error::Error>> {
    for p in phrases {
        let start_s = f64::from(u32::try_from(p.start)?) / 16_000.0;
        let end_s = f64::from(u32::try_from(p.end)?) / 16_000.0;
        writeln!(
            out,
            "[{start_s:.2}-{end_s:.2}] silent={} phantom={} text={:?}",
            is_silent(phrase_audio(trimmed, p)),
            phantom(trimmed, p),
            p.text
        )?;
    }
    Ok(())
}
