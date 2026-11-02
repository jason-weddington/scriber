//! Study-104 extractor (applied-science `studies/104-scriber-speech-gating`), never run by the gate and never shipped: reads Scriber recording folders and writes the measurements the lab compares speech/non-speech gating methods with. It decodes with the real speech model, so it runs on the Mac:
//!
//! ```sh
//! cargo run --release -p scriber-engine --example extract -- <config.json>
//! ```
//!
//! All the logic lives in `scriber_engine::extract` (`src/extract.rs`), which the gate tests without a model; this file only reads files, runs afconvert, Whisper and Silero, and writes the results.
//!
//! # Config
//!
//! One JSON object, unknown fields refused at every level: `{"mode": "features" | "stage2", "input": "auto" | "m4a" | "segments" (optional, default "auto"), "model": "<ggml model path>", "output_dir": "<dir>", "recordings": ["<library folder>", ...], "vocabulary": "<file>" (optional), "silero": {"model": "<Silero ggml path>", "settings": [{"name": "...", "threshold": 0.5, "min_silence_duration_ms": 100, "speech_pad_ms": 30, "min_speech_duration_ms": 250}, ...]} (optional, features mode only), "decisions": {"<recording name>": {"split_s": [f64, ...], "pieces": [{"skip": bool, "trim_end_s": f64, "drop": [usize, ...]}, ...]}} (optional, stage2 mode only), "repeat_decode": bool (optional, default false, both modes)}`. A recording's name is its folder's final path component; names must be unique. `output_dir` is created when missing and existing files in it are overwritten.
//!
//! # Timeline
//!
//! `split_s`, the Silero spans, the stage-2 phrase times and the features report's top-level `untrimmed.phrases` times are seconds on the one concatenated untrimmed timeline, located per input segment by `offset_s`/`length_s` (features: on each `segments` entry; stage2: `input_segments`).
//!
//! Each segment's own `untrimmed`/`trimmed` phrase times are relative to that segment's start; the top-level `trimmed.phrases` are on the concatenated trimmed timeline.
//!
//! # Repeat decode
//!
//! With `repeat_decode`, each input segment's untrimmed PCM (features; the trimmed decode is not repeated) or each non-skipped piece (stage2) is decoded a second time with identical parameters. Each such segment or piece then has `repeat`: `{"text_equal": bool, "no_speech_probability_max_abs_diff": f64 or null (null when the Whisper segment counts differ), "segment_count_equal": bool}`, and the recording has `repeat_all_equal`: bool. Without it, no `repeat` fields appear.
//!
//! # Exit codes
//!
//! 1: the config is unusable (it does not parse or breaks a rule, the model or the Silero model or the vocabulary file cannot be read, or no scriber commit can be found); nothing is written. 2: at least one recording failed; the others are written, and so is the manifest. 0: every recording was extracted.
//!
//! # Input
//!
//! `input` chooses what each folder is read from. "auto": `segments/segment-NNNN.wav` when there is at least one, else `audio.m4a`, else `audio.wav`. "segments": the segment WAVs only (an error for that recording when there are none). "m4a": `audio.m4a`, else `audio.wav`, never the segment WAVs. Segment WAVs (16 kHz mono 16-bit PCM, the stored parts before the trailing trim) are read in ascending index order, each one input segment, so a multi-part recording keeps its real part boundaries (`input` = "segment-wavs"). `audio.m4a` is decoded by `/usr/bin/afconvert -f WAVE -d LEF32@16000 -c 1` (which writes the `WAVE_FORMAT_EXTENSIBLE` form of 32-bit float, read as such) to `<output_dir>/<name>.decoded.tmp.wav`, read and deleted, and is one input segment (`input` = "m4a-decoded-single-segment"); off macOS this call fails, naming `/usr/bin/afconvert`. `audio.wav` (left by the library when encoding failed or has not run) is one input segment (`input` = "wav-single-segment").
//!
//! Caveat: AAC is lossy, so an m4a-decoded PCM is NOT byte-identical to what production fed Whisper live, and a single-file input loses the part boundaries production decoded separately. Segment WAVs are the exact stored parts, and exist only when the hidden `keep_segments` setting was on for that recording.
//!
//! # Mode `features`
//!
//! The unit is the input segment, as in production: each part is checked with `is_silent`, trimmed by `trim_trailing_silence`, transcribed and passed through `kept_text`, and the parts' texts are joined with `transcript::join`. Per recording it writes `<name>.json`, and for each input segment k (0-based, four digits) `<name>.segKKKK.untrimmed.wav` (the decoded PCM) and `<name>.segKKKK.trimmed.wav` (after production's trailing trim, exactly what production hands Whisper), plus `<name>.untrimmed.wav` and `<name>.trimmed.wav`, the segments concatenated in order (for a single-file input these equal the seg0000 files byte for byte). Every WAV is 16 kHz mono 32-bit IEEE float; an empty PCM is a 44-byte header. Whisper decodes each segment's untrimmed and trimmed PCM separately with the production parameters (beam search 5, patience -1, English, non-speech tokens suppressed, the prompt below); an empty PCM is not decoded. The JSON reports, per segment and joined per recording, every phrase with its times, text, no-speech probability, silence and phantom flags, window RMS and every token (text, id, special, probability, log probability), and production's M0 outcome: whether the segment is skipped as silent, which phrases `kept_text` keeps, which last phrase is dropped as a phantom, whether `keep` rejected the text, and the text before cleanup. Cleanup (Ollama) never runs.
//!
//! With `silero`, each setting runs whisper.cpp's standalone `WhisperVadContext` on the recording's concatenated untrimmed PCM (max speech duration, samples overlap, threads and GPU at whisper-rs's defaults) and the spans are written in seconds, with the Silero model's SHA-256. whisper.cpp reports VAD times in centiseconds.
//!
//! Causality (`silero.causality`, first recording and first setting only): `WhisperVadContext` resets its LSTM state on every `segments_from_samples` call (the vendored whisper.cpp's `whisper_vad_detect_speech` clears it), so there is no stateful streaming API; re-running on growing prefixes is the only causal use available. The extractor runs it on prefixes at 2.0 s steps (the last is the whole recording) and calls the result causal (`causal` = `prefix_stable`) when, for every prefix of length L, the prefix's spans ending before L - 1 s pair one to one, sorted by start, with the offline spans ending before L - 1 s, every boundary within 0.05 s. As a side measurement it also runs consecutive independent 1.0 s chunks, offsets their spans onto the timeline, merges gaps of at most 0.05 s and compares them with all offline spans (`chunks_match_offline`). `verdict` states the outcome in one sentence. No result is claimed here: the lead runs it on the Mac.
//!
//! # Mode `stage2`
//!
//! The lab's gating decisions, as data, applied the way production applies its own. The recording's timeline is its input segments concatenated in order. Pieces are the input segments, cut further at each `split_s` time (seconds on the timeline, strictly increasing, inside the recording and off the input segment boundaries; G3), numbered from 0. `pieces`, when given, holds exactly one decision per piece: `skip: true` drops the piece undecoded (G1); `trim_end_s` (seconds from the piece's start, > 0 and at most its length) decodes only that much of it (G2, no padding and no `trim_trailing_silence`); `drop` lists Whisper phrase indices of the piece to leave out (G4). Each remaining piece is decoded with the production parameters (its own decode: phrase indices, and so `drop`, refer to this run's phrases of the piece, not to stage 1's), its kept phrase texts are concatenated as Whisper produced them and trimmed, and the non-empty piece texts are joined with `transcript::join` as production joins parts. A recording with no decisions (no entry, or no `split_s` and no `pieces`) is the no-gating reference: one untrimmed decode per input segment, nothing skipped, trimmed or dropped. `kept_text`, `keep`, `phantom`, `is_silent` and `trim_trailing_silence` are not applied. Each phrase carries the full features-mode phrase fields (`index`, `t0_s`, `t1_s`, `text`, `no_speech_probability`, `silent`, `phantom`, `rms_max`, `tokens`) computed on the piece's decoded PCM, reported for the lab only and never applied to `text`; only `drop` removes phrases from `text`. It writes `<name>.stage2.json` with each piece's extent, decisions, phrases (times on the timeline, plus `dropped` flags), `text` and `text_undropped` (the piece text with no drop applied), and the recording's `text` and `text_undropped` before cleanup; no WAV.
//!
//! # Provenance and prompt
//!
//! `scriber_commit` is the `SCRIBER_COMMIT` environment variable when set and non-empty (then `scriber_dirty` is null), else `git rev-parse HEAD` in this crate's directory, with `scriber_dirty` from `git status --porcelain`; with neither, the run fails with exit 1. `whisper_rs_version` is pinned and checked against Cargo.lock by a test. Model and audio SHA-256s are streamed in 64 KiB chunks.
//!
//! The prompt is `vocabulary::prompt` of the configured vocabulary file, or none. Production instead always prompts with `vocabulary::prompt_for` of its glossary (the style sentence plus learned terms), and no prompt and the style sentence alone give different output, so every report says `prompt_matches_production: false`.
//!
//! Every run also writes `manifest.json`: the mode, provenance, model, the parsed config, start and finish times (RFC 3339 UTC), and each recording's status ("ok", "error" with its message, or "skipped" for a stage-2 recording whose every piece is skipped) and output files.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Instant;

use scriber_core::audio::decode_wav;
use scriber_core::transcript::trim_trailing_silence;
use scriber_engine::extract::{
    AudioDecode, Common, Config, DecodeParams, Decoded, InputKind, Manifest, Mode, RecordingStatus,
    SegmentFile, SegmentInput, SileroOut, Span, Status, VadRun, WHISPER_RS_VERSION, afconvert_args,
    assemble_pieces, build_prompt, build_segment, causality, check_contiguous, check_spans,
    chunk_ranges, features_report, file_name, needs_decode, now_rfc3339, offset_spans,
    parse_config, plan_pieces, prefix_lengths, provenance, read_f32_wav, recording_name,
    resolve_input, samples_to_seconds, segment_wav_names, sha256_hex, stage2_report, write_f32_wav,
};
use scriber_engine::extract::{Provenance, VadSettings};
use scriber_engine::{AFCONVERT, Whisper, vad_spans};

type Failure = Box<dyn std::error::Error>;

/// Everything a recording's extraction needs.
struct Run {
    config: Config,
    provenance: Provenance,
    model_file: String,
    model_sha256: String,
    silero_sha256: Option<String>,
    prompt: Option<String>,
    whisper: Whisper,
    out: PathBuf,
}

/// A recording's input, read.
struct Loaded {
    kind: InputKind,
    segments: Vec<(Option<String>, Vec<f32>)>,
    audio_sha256: String,
    segment_files: Vec<SegmentFile>,
    afconvert: Vec<String>,
}

fn main() -> ExitCode {
    let started = now_rfc3339();
    let run = match setup() {
        Ok(run) => run,
        Err(e) => {
            report(&format!("extract: {e}"));
            return ExitCode::from(1);
        }
    };
    let mut statuses = Vec::new();
    for (i, folder) in run.config.recordings.iter().enumerate() {
        let name = recording_name(folder).unwrap_or_else(|_| folder.clone());
        let status = match extract(&run, i, folder, &name) {
            Ok((status, outputs)) => RecordingStatus {
                name,
                status,
                error: None,
                outputs,
            },
            Err(e) => {
                report(&format!("extract: {name}: {e}"));
                RecordingStatus {
                    name,
                    status: Status::Error,
                    error: Some(e.to_string()),
                    outputs: Vec::new(),
                }
            }
        };
        statuses.push(status);
    }
    let manifest = Manifest::new(
        run.config.clone(),
        run.provenance.clone(),
        (run.model_file.clone(), run.model_sha256.clone()),
        (started, now_rfc3339()),
        statuses,
    );
    let written = serde_json::to_vec_pretty(&manifest)
        .map_err(Failure::from)
        .and_then(|json| Ok(fs::write(run.out.join("manifest.json"), json)?));
    if let Err(e) = written {
        report(&format!("extract: manifest.json: {e}"));
        return ExitCode::from(2);
    }
    ExitCode::from(manifest.exit_code())
}

/// One line on stderr; there is nowhere to report a failure to write it.
fn report(line: &str) {
    let mut err = std::io::stderr();
    let _unreported = writeln!(err, "{line}");
}

/// The config stage: anything failing here writes nothing.
fn setup() -> Result<Run, Failure> {
    let mut args = std::env::args().skip(1);
    let usage = "usage: extract <config.json>";
    let path = args.next().ok_or(usage)?;
    if args.next().is_some() {
        return Err(usage.into());
    }
    let config = parse_config(&fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?)?;
    let env_commit = std::env::var("SCRIBER_COMMIT").ok();
    let dir = env!("CARGO_MANIFEST_DIR");
    let head = git(&["-C", dir, "rev-parse", "HEAD"]);
    let status = git(&["-C", dir, "status", "--porcelain"]);
    let provenance = provenance(env_commit.as_deref(), head.as_deref(), status.as_deref())?;
    let model_sha256 = hash_file(Path::new(&config.model))?;
    let silero_sha256 = match &config.silero {
        Some(s) => Some(hash_file(Path::new(&s.model))?),
        None => None,
    };
    let vocabulary = match &config.vocabulary {
        Some(v) => Some(fs::read_to_string(v).map_err(|e| format!("{v}: {e}"))?),
        None => None,
    };
    let out = PathBuf::from(&config.output_dir);
    fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    Ok(Run {
        model_file: file_name(&config.model),
        whisper: Whisper::new(PathBuf::from(&config.model)),
        prompt: build_prompt(vocabulary.as_deref()),
        config,
        provenance,
        model_sha256,
        silero_sha256,
        out,
    })
}

/// git's stdout, when it ran and succeeded.
fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn hash_file(path: &Path) -> Result<String, Failure> {
    let mut file = fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(sha256_hex(&mut file)?)
}

fn millis(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn load(run: &Run, folder: &Path, name: &str) -> Result<Loaded, Failure> {
    let dir = folder.join("segments");
    let listed: Vec<String> = fs::read_dir(&dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    let names = segment_wav_names(&listed.iter().map(String::as_str).collect::<Vec<_>>());
    let m4a = folder.join("audio.m4a");
    let wav = folder.join("audio.wav");
    let kind = resolve_input(
        run.config.input,
        !names.is_empty(),
        m4a.is_file(),
        wav.is_file(),
    )?;
    match kind {
        InputKind::SegmentWavs => {
            let mut segments = Vec::new();
            let mut files = Vec::new();
            let mut all: Box<dyn Read> = Box::new(std::io::empty());
            for n in &names {
                let path = dir.join(n);
                let bytes = fs::read(&path).map_err(|e| format!("{n}: {e}"))?;
                let samples = decode_wav(&bytes).map_err(|e| format!("{n}: {e}"))?;
                files.push(SegmentFile {
                    file: n.clone(),
                    sha256: sha256_hex(&mut bytes.as_slice())?,
                    samples: samples.len(),
                });
                segments.push((Some(n.clone()), samples));
                all = Box::new(all.chain(fs::File::open(&path)?));
            }
            Ok(Loaded {
                kind,
                segments,
                audio_sha256: sha256_hex(&mut all)?,
                segment_files: files,
                afconvert: Vec::new(),
            })
        }
        InputKind::M4a => {
            let tmp = run.out.join(format!("{name}.decoded.tmp.wav"));
            let args = afconvert_args(&m4a, &tmp);
            let decoded = afconvert(&args, &tmp);
            let _kept = fs::remove_file(&tmp);
            Ok(Loaded {
                kind,
                segments: vec![(None, decoded?)],
                audio_sha256: hash_file(&m4a)?,
                segment_files: Vec::new(),
                afconvert: args,
            })
        }
        InputKind::Wav => {
            let samples = decode_wav(&fs::read(&wav)?).map_err(|e| format!("audio.wav: {e}"))?;
            Ok(Loaded {
                kind,
                segments: vec![(None, samples)],
                audio_sha256: hash_file(&wav)?,
                segment_files: Vec::new(),
                afconvert: Vec::new(),
            })
        }
    }
}

/// Runs afconvert with `args` and reads the float WAV it wrote at `tmp`.
fn afconvert(args: &[String], tmp: &Path) -> Result<Vec<f32>, Failure> {
    let output = Command::new(AFCONVERT)
        .args(args)
        .output()
        .map_err(|e| format!("{AFCONVERT}: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("{AFCONVERT} failed ({}): {}", output.status, stderr.trim()).into());
    }
    Ok(read_f32_wav(&fs::read(tmp)?)?)
}

fn common(run: &Run, loaded: &Loaded) -> Common {
    let samples = loaded.segments.iter().map(|(_, s)| s.len()).sum();
    Common {
        input: loaded.kind.label(),
        provenance: run.provenance.clone(),
        whisper_rs_version: WHISPER_RS_VERSION,
        model_file: run.model_file.clone(),
        model_sha256: run.model_sha256.clone(),
        audio_file: loaded.kind.audio_file(),
        audio_sha256: loaded.audio_sha256.clone(),
        audio_segment_files: loaded.segment_files.clone(),
        audio_decode: AudioDecode::new(loaded.afconvert.clone(), samples),
        decode: DecodeParams::new(run.prompt.clone()),
        prompt_matches_production: false,
    }
}

fn extract(
    run: &Run,
    i: usize,
    folder: &str,
    name: &str,
) -> Result<(Status, Vec<String>), Failure> {
    let loaded = load(run, Path::new(folder), name)?;
    let files = match run.config.mode {
        Mode::Features => features(run, i == 0, name, &loaded)?,
        Mode::Stage2 => stage2(run, name, &loaded)?,
    };
    let mut outputs = Vec::new();
    for (file, bytes) in &files.1 {
        fs::write(run.out.join(file), bytes).map_err(|e| format!("{file}: {e}"))?;
        outputs.push(file.clone());
    }
    Ok((files.0, outputs))
}

/// Whisper on `pcm`, unless it is empty.
fn decode(run: &Run, pcm: &[f32]) -> Result<Decoded, Failure> {
    if !needs_decode(pcm) {
        return Ok(Decoded::default());
    }
    let start = Instant::now();
    let phrases = run
        .whisper
        .transcribe_detailed(pcm, run.prompt.as_deref())?;
    Ok(Decoded {
        phrases,
        elapsed_ms: millis(start),
    })
}

/// Silero on `pcm`, checked.
fn vad(model: &Path, pcm: &[f32], settings: &VadSettings) -> Result<Vec<Span>, Failure> {
    let spans = vad_spans(model, pcm, settings)?;
    check_spans(&spans, samples_to_seconds(pcm.len())?)?;
    Ok(spans.into_iter().map(Span::from).collect())
}

type Files = (Status, Vec<(String, Vec<u8>)>);

fn features(run: &Run, first: bool, name: &str, loaded: &Loaded) -> Result<Files, Failure> {
    let mut wavs = Vec::new();
    let mut segments = Vec::new();
    let (mut all_untrimmed, mut all_trimmed) = (Vec::new(), Vec::new());
    for (k, (file, untrimmed)) in loaded.segments.iter().enumerate() {
        let trimmed = trim_trailing_silence(untrimmed);
        let untrimmed_decode = decode(run, untrimmed)?;
        let trimmed_decode = decode(run, trimmed)?;
        let repeat_decode = if run.config.repeat_decode {
            Some(decode(run, untrimmed)?)
        } else {
            None
        };
        let input = SegmentInput {
            file: file.clone(),
            untrimmed: untrimmed.clone(),
            untrimmed_decode,
            trimmed_decode,
            repeat_decode,
        };
        segments.push(build_segment(k, &input)?);
        wavs.push((
            format!("{name}.seg{k:04}.untrimmed.wav"),
            write_f32_wav(untrimmed)?,
        ));
        wavs.push((
            format!("{name}.seg{k:04}.trimmed.wav"),
            write_f32_wav(trimmed)?,
        ));
        all_untrimmed.extend_from_slice(untrimmed);
        all_trimmed.extend_from_slice(trimmed);
    }
    wavs.push((
        format!("{name}.untrimmed.wav"),
        write_f32_wav(&all_untrimmed)?,
    ));
    wavs.push((format!("{name}.trimmed.wav"), write_f32_wav(&all_trimmed)?));
    let silero = match (&run.config.silero, &run.silero_sha256) {
        (Some(config), Some(sha)) => {
            let model = Path::new(&config.model);
            let mut runs = Vec::new();
            for settings in &config.settings {
                let start = Instant::now();
                let spans = vad(model, &all_untrimmed, settings)?;
                runs.push(VadRun::new(settings, millis(start), spans));
            }
            let causal = match (first, config.settings.first(), runs.first()) {
                (true, Some(settings), Some(offline)) => Some(causality_check(
                    name,
                    model,
                    &all_untrimmed,
                    settings,
                    &offline.spans,
                )?),
                _ => None,
            };
            Some(SileroOut {
                model_file: file_name(&config.model),
                model_sha256: sha.clone(),
                runs,
                causality: causal,
            })
        }
        _ => None,
    };
    let report = features_report(
        common(run, loaded),
        segments,
        silero,
        run.config.repeat_decode,
    )?;
    let mut files = vec![(format!("{name}.json"), serde_json::to_vec_pretty(&report)?)];
    files.extend(wavs);
    Ok((Status::Ok, files))
}

fn causality_check(
    name: &str,
    model: &Path,
    pcm: &[f32],
    settings: &VadSettings,
    offline: &[Span],
) -> Result<scriber_engine::extract::Causality, Failure> {
    let mut prefixes = Vec::new();
    for len in prefix_lengths(pcm.len()) {
        let prefix = pcm.get(..len).ok_or("prefix past the audio")?;
        prefixes.push((samples_to_seconds(len)?, vad(model, prefix, settings)?));
    }
    let mut chunks = Vec::new();
    for range in chunk_ranges(pcm.len()) {
        let offset = samples_to_seconds(range.start)?;
        let chunk = pcm.get(range).ok_or("chunk past the audio")?;
        chunks.extend(offset_spans(&vad(model, chunk, settings)?, offset));
    }
    Ok(causality(name, &settings.name, offline, &prefixes, &chunks))
}

fn stage2(run: &Run, name: &str, loaded: &Loaded) -> Result<Files, Failure> {
    let lengths: Vec<usize> = loaded.segments.iter().map(|(_, s)| s.len()).collect();
    let timeline: Vec<f32> = loaded
        .segments
        .iter()
        .flat_map(|(_, s)| s.iter().copied())
        .collect();
    let decisions = run.config.decisions.as_ref().and_then(|d| d.get(name));
    let plan = plan_pieces(&lengths, decisions)?;
    check_contiguous(&plan, timeline.len())?;
    let mut decoded = Vec::new();
    let mut repeats = Vec::new();
    for piece in &plan {
        let end = piece.range.start.saturating_add(piece.decode_len);
        let pcm = timeline
            .get(piece.range.start..end)
            .ok_or("piece past the audio")?;
        let skip = piece.skip;
        decoded.push(if skip {
            Decoded::default()
        } else {
            decode(run, pcm)?
        });
        repeats.push(if skip || !run.config.repeat_decode {
            None
        } else {
            Some(decode(run, pcm)?)
        });
    }
    let repeats = run.config.repeat_decode.then_some(repeats.as_slice());
    let output = assemble_pieces(&plan, &timeline, &decoded, repeats)?;
    let status = if output.skipped {
        Status::Skipped
    } else {
        Status::Ok
    };
    let report = stage2_report(common(run, loaded), decisions.cloned(), &lengths, output)?;
    Ok((
        status,
        vec![(
            format!("{name}.stage2.json"),
            serde_json::to_vec_pretty(&report)?,
        )],
    ))
}
