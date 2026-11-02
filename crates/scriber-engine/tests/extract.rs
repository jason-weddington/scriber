//! The study-104 extractor's pure logic (`scriber_engine::extract`).

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::path::Path;

    use scriber_core::audio::decode_wav;
    use scriber_core::transcript::{
        SILENCE_RMS, TRAILING_PADDING, TRAILING_SILENCE_RMS, is_silent, join, trim_trailing_silence,
    };
    use scriber_engine::extract::{
        AudioDecode, Causality, Common, Config, Decisions, DecodeParams, Decoded, ExtractError,
        FeaturesReport, InputKind, InputMode, InputSegmentTiming, Manifest, Mode, PieceDecision,
        PlannedPiece, Provenance, RecordingStatus, Repeat, SegmentFile, SegmentInput, SegmentM0,
        SileroOut, Span, Stage2Output, Status, Thresholds, VadRun, VadSettings, WHISPER_RS_VERSION,
        afconvert_args, assemble_pieces, build_prompt, build_segment, causality, check_contiguous,
        check_phrases, check_spans, chunk_ranges, compare_repeat, f32_wav_header, features_report,
        file_name, needs_decode, now_rfc3339, offset_spans, parse_config, plan_pieces,
        prefix_lengths, provenance, read_f32_wav, recording_m0, recording_name, resolve_input,
        samples_to_seconds, seconds_to_samples, segment_m0, segment_wav_names, sha256_hex,
        stage2_report, window_rms_max, write_f32_wav,
    };
    use scriber_engine::{DetailedPhrase, DetailedToken, Phrase};
    use serde_json::{Value, json};
    use sha2::{Digest as _, Sha256};

    // ---- helpers ----------------------------------------------------------------

    fn ph(text: &str, start: usize, end: usize) -> Phrase {
        Phrase {
            text: text.to_string(),
            start,
            end,
        }
    }

    fn dp(text: &str, start: usize, end: usize) -> DetailedPhrase {
        DetailedPhrase {
            phrase: ph(text, start, end),
            no_speech_probability: 0.25,
            tokens: vec![
                DetailedToken {
                    id: 50_364,
                    special: true,
                    text: "[_TT_0]".to_string(),
                    probability: 0.5,
                    log_probability: -0.75,
                },
                DetailedToken {
                    id: 2_000,
                    special: false,
                    text: text.to_string(),
                    probability: 0.875,
                    log_probability: -0.125,
                },
            ],
        }
    }

    fn decoded(phrases: Vec<DetailedPhrase>, elapsed_ms: u64) -> Decoded {
        Decoded {
            phrases,
            elapsed_ms,
        }
    }

    fn loud(n: usize) -> Vec<f32> {
        vec![0.5; n]
    }

    fn quiet(n: usize) -> Vec<f32> {
        vec![0.0; n]
    }

    fn cat(parts: &[&[f32]]) -> Vec<f32> {
        parts.concat()
    }

    fn config_err(text: &str) -> String {
        match parse_config(text) {
            Err(ExtractError::Config(m)) => m,
            other => panic!("expected a config error for {text}, got {other:?}"),
        }
    }

    fn rec_err<T: std::fmt::Debug>(r: Result<T, ExtractError>) -> String {
        match r {
            Err(ExtractError::Recording(m)) => m,
            other => panic!("expected a recording error, got {other:?}"),
        }
    }

    fn features_json(extra: &str) -> String {
        format!(
            r#"{{"mode": "features", "model": "/m/ggml.bin", "output_dir": "/out", "recordings": ["/lib/r1", "/lib/r2/"]{extra}}}"#
        )
    }

    fn stage2_json(extra: &str) -> String {
        format!(
            r#"{{"mode": "stage2", "model": "/m/ggml.bin", "output_dir": "/out", "recordings": ["/lib/r1", "/lib/r2"]{extra}}}"#
        )
    }

    fn keys(v: &Value) -> Vec<String> {
        let mut k: Vec<String> = v
            .as_object()
            .unwrap_or_else(|| panic!("not an object: {v}"))
            .keys()
            .cloned()
            .collect();
        k.sort();
        k
    }

    fn assert_keys(v: &Value, want: &[&str], at: &str) {
        let mut want: Vec<String> = want.iter().map(ToString::to_string).collect();
        want.sort();
        assert_eq!(keys(v), want, "keys at {at}");
    }

    fn span(start_s: f64, end_s: f64) -> Span {
        Span { start_s, end_s }
    }

    fn bits(x: f64) -> u64 {
        x.to_bits()
    }

    // ---- errors and numbers ----------------------------------------------------------

    #[test]
    fn errors_say_what_failed() {
        assert_eq!(
            ExtractError::Config("x".into()).to_string(),
            "config: x",
            "config"
        );
        assert_eq!(ExtractError::Wav("x".into()).to_string(), "wav: x", "wav");
        assert_eq!(
            ExtractError::Recording("x".into()).to_string(),
            "x",
            "recording"
        );
        assert_eq!(ExtractError::Io("x".into()).to_string(), "io: x", "io");
    }

    #[test]
    fn seconds_become_samples_rounded_and_in_range() {
        assert_eq!(seconds_to_samples(0.0), Ok(0), "zero");
        assert_eq!(seconds_to_samples(0.5), Ok(8_000), "half a second");
        assert_eq!(seconds_to_samples(0.000_03), Ok(0), "rounds down");
        assert_eq!(seconds_to_samples(0.000_04), Ok(1), "rounds up");
        assert_eq!(
            seconds_to_samples(268_435.455_937_5),
            Ok(4_294_967_295),
            "the last u32 sample"
        );
        let _over = rec_err(seconds_to_samples(268_435.456));
        let _neg = rec_err(seconds_to_samples(-0.5));
        let _nan = rec_err(seconds_to_samples(f64::NAN));
        let _inf = rec_err(seconds_to_samples(f64::INFINITY));
        let _ninf = rec_err(seconds_to_samples(f64::NEG_INFINITY));
        let _huge = rec_err(seconds_to_samples(1e12));
    }

    #[test]
    fn samples_become_seconds() {
        assert_eq!(samples_to_seconds(8_000).map(bits), Ok(bits(0.5)), "half");
        assert_eq!(samples_to_seconds(0).map(bits), Ok(bits(0.0)), "zero");
        let too_many = usize::try_from(u64::from(u32::MAX) + 1).unwrap();
        let _err = rec_err(samples_to_seconds(too_many));
    }

    #[test]
    fn window_rms_max_is_the_loudest_window() {
        assert_eq!(window_rms_max(&[]).to_bits(), 0.0f32.to_bits(), "empty");
        assert_eq!(
            window_rms_max(&loud(1_600)).to_bits(),
            0.5f32.to_bits(),
            "one window"
        );
        let partial = cat(&[&quiet(1_600), &loud(100)]);
        assert_eq!(
            window_rms_max(&partial).to_bits(),
            0.5f32.to_bits(),
            "a trailing partial window counts on its own length"
        );
        let rising = cat(&[&vec![0.25; 1_600], &loud(1_600)]);
        let falling = cat(&[&loud(1_600), &vec![0.25; 1_600]]);
        assert_eq!(
            window_rms_max(&rising).to_bits(),
            0.5f32.to_bits(),
            "rising"
        );
        assert_eq!(
            window_rms_max(&falling).to_bits(),
            0.5f32.to_bits(),
            "falling"
        );
        // The RMS, not the mean or the peak: one window, half at 0.5, half 0.
        let half = cat(&[&loud(800), &quiet(800)]);
        let want = (0.125f32).sqrt();
        assert!(
            (window_rms_max(&half) - want).abs() < 1e-6,
            "rms of a half-loud window"
        );
    }

    #[test]
    fn window_rms_max_agrees_with_is_silent_at_the_boundary() {
        let levels = [
            0.0, 0.009, 0.0099, 0.009_999, 0.01, 0.010_001, 0.0101, 0.011, 0.5,
        ];
        for level in levels {
            for n in [1, 100, 1_600, 1_700, 3_200] {
                let x = vec![level; n];
                assert_eq!(
                    window_rms_max(&x) < SILENCE_RMS,
                    is_silent(&x),
                    "level {level}, {n} samples"
                );
                let with_quiet = cat(&[&quiet(1_600), &x]);
                assert_eq!(
                    window_rms_max(&with_quiet) < SILENCE_RMS,
                    is_silent(&with_quiet),
                    "level {level} after a quiet window, {n} samples"
                );
            }
        }
    }

    #[test]
    fn only_empty_audio_is_not_decoded() {
        assert!(!needs_decode(&[]), "empty");
        assert!(needs_decode(&[0.0]), "one sample");
    }

    #[test]
    fn the_prompt_comes_from_the_vocabulary_file_only() {
        assert_eq!(build_prompt(None), None, "no vocabulary, no prompt");
        assert_eq!(
            build_prompt(Some("a\nb")),
            Some(scriber_core::vocabulary::prompt("a\nb")),
            "the vocabulary prompt"
        );
    }

    #[test]
    fn names_are_final_path_components() {
        assert_eq!(recording_name("/lib/r1/").unwrap(), "r1", "trailing slash");
        assert_eq!(recording_name("r2").unwrap(), "r2", "bare");
        assert!(
            matches!(recording_name("/"), Err(ExtractError::Config(_))),
            "root has none"
        );
        assert_eq!(file_name("/m/ggml.bin"), "ggml.bin", "file");
        assert_eq!(file_name("/"), "/", "no file name: the path");
    }

    #[test]
    fn the_whisper_rs_version_matches_the_lockfile() {
        let lock = include_str!("../../../Cargo.lock");
        let pinned = format!("name = \"whisper-rs\"\nversion = \"{WHISPER_RS_VERSION}\"");
        assert!(lock.contains(&pinned), "Cargo.lock locks {pinned}");
        assert_eq!(WHISPER_RS_VERSION, "0.16.0", "the pinned version");
    }

    #[test]
    fn now_is_rfc3339_utc() {
        let now = now_rfc3339();
        assert!(now.ends_with('Z'), "UTC: {now}");
        assert!(
            chrono::DateTime::parse_from_rfc3339(&now).is_ok(),
            "parses: {now}"
        );
    }

    // ---- config ----------------------------------------------------------------------

    #[test]
    fn a_features_config_parses() {
        let c = parse_config(&features_json(
            r#", "input": "segments", "vocabulary": "/v.txt", "silero": {"model": "/s.bin", "settings": [{"name": "a", "threshold": 0.5, "min_silence_duration_ms": 100, "speech_pad_ms": 30, "min_speech_duration_ms": 250}, {"name": "b", "threshold": 0.25, "min_silence_duration_ms": 1, "speech_pad_ms": 2, "min_speech_duration_ms": 3}]}"#,
        ))
        .unwrap();
        assert_eq!(c.mode, Mode::Features, "mode");
        assert_eq!(c.input, InputMode::Segments, "input");
        assert_eq!(c.model, "/m/ggml.bin", "model");
        assert_eq!(c.output_dir, "/out", "output");
        assert_eq!(c.recordings, ["/lib/r1", "/lib/r2/"], "recordings");
        assert_eq!(c.vocabulary.as_deref(), Some("/v.txt"), "vocabulary");
        let silero = c.silero.unwrap();
        assert_eq!(silero.model, "/s.bin", "silero model");
        assert_eq!(
            silero.settings[1],
            VadSettings {
                name: "b".into(),
                threshold: 0.25,
                min_silence_duration_ms: 1,
                speech_pad_ms: 2,
                min_speech_duration_ms: 3
            },
            "a setting"
        );
        assert_eq!(c.decisions, None, "no decisions");
        let plain = parse_config(&features_json("")).unwrap();
        assert_eq!(plain.input, InputMode::Auto, "input defaults to auto");
        assert_eq!(plain.vocabulary, None, "no vocabulary");
        assert_eq!(plain.silero, None, "no silero");
        let m4a = parse_config(&features_json(r#", "input": "m4a""#)).unwrap();
        assert_eq!(m4a.input, InputMode::M4a, "m4a");
        let auto = parse_config(&features_json(r#", "input": "auto""#)).unwrap();
        assert_eq!(auto.input, InputMode::Auto, "auto");
    }

    #[test]
    fn a_stage2_config_parses() {
        let c = parse_config(&stage2_json(
            r#", "decisions": {"r1": {"split_s": [0.5, 1.5], "pieces": [{"skip": true}, {"skip": false, "trim_end_s": 0.25, "drop": [2, 0]}, {}]}, "r2": {}}"#,
        ))
        .unwrap();
        assert_eq!(c.mode, Mode::Stage2, "mode");
        let d = c.decisions.unwrap();
        assert_eq!(
            d["r1"],
            Decisions {
                split_s: Some(vec![0.5, 1.5]),
                pieces: Some(vec![
                    PieceDecision {
                        skip: Some(true),
                        trim_end_s: None,
                        drop: None
                    },
                    PieceDecision {
                        skip: Some(false),
                        trim_end_s: Some(0.25),
                        drop: Some(vec![2, 0])
                    },
                    PieceDecision::default(),
                ])
            },
            "r1"
        );
        assert_eq!(d["r2"], Decisions::default(), "r2");
    }

    #[test]
    fn unknown_fields_are_refused_at_every_level() {
        let top = config_err(&features_json(r#", "colour": 1"#));
        assert!(top.contains("colour"), "{top}");
        let entry = config_err(&stage2_json(r#", "decisions": {"r1": {"skip": true}}"#));
        assert!(entry.contains("skip"), "{entry}");
        let piece = config_err(&stage2_json(
            r#", "decisions": {"r1": {"pieces": [{"trim": 1.0}]}}"#,
        ));
        assert!(piece.contains("trim"), "{piece}");
        let setting = config_err(&features_json(
            r#", "silero": {"model": "/s", "settings": [{"name": "a", "threshold": 0.5, "min_silence_duration_ms": 1, "speech_pad_ms": 1, "min_speech_duration_ms": 1, "x": 1}]}"#,
        ));
        assert!(setting.contains('x'), "{setting}");
        let input = config_err(&features_json(r#", "input": "wav""#));
        assert!(input.contains("wav"), "{input}");
    }

    #[test]
    fn modes_take_only_their_own_sections() {
        assert_eq!(
            config_err(&features_json(r#", "decisions": {}"#)),
            "decisions are for mode stage2 only",
            "features"
        );
        assert_eq!(
            config_err(&stage2_json(
                r#", "silero": {"model": "/s", "settings": []}"#
            )),
            "silero is for mode features only",
            "stage2"
        );
    }

    #[test]
    fn recordings_must_be_named_uniquely() {
        assert_eq!(
            config_err(
                r#"{"mode": "features", "model": "m", "output_dir": "o", "recordings": []}"#
            ),
            "recordings is empty",
            "empty"
        );
        assert_eq!(
            config_err(
                r#"{"mode": "features", "model": "m", "output_dir": "o", "recordings": ["/a/r1", "/b/r1/"]}"#
            ),
            "duplicate recording name r1",
            "duplicate"
        );
        let root = config_err(
            r#"{"mode": "features", "model": "m", "output_dir": "o", "recordings": ["/"]}"#,
        );
        assert!(root.contains("no folder name"), "{root}");
    }

    #[test]
    fn silero_settings_are_present_and_unique() {
        assert_eq!(
            config_err(&features_json(
                r#", "silero": {"model": "/s", "settings": []}"#
            )),
            "silero.settings is empty",
            "empty"
        );
        let s = r#"{"name": "a", "threshold": 0.5, "min_silence_duration_ms": 1, "speech_pad_ms": 1, "min_speech_duration_ms": 1}"#;
        assert_eq!(
            config_err(&features_json(&format!(
                r#", "silero": {{"model": "/s", "settings": [{s}, {s}]}}"#
            ))),
            "duplicate silero setting a",
            "duplicate"
        );
    }

    #[test]
    fn repeat_decode_is_an_optional_bool_in_both_modes() {
        for json in [features_json, stage2_json] {
            assert!(!parse_config(&json("")).unwrap().repeat_decode, "default");
            assert!(
                parse_config(&json(r#", "repeat_decode": true"#))
                    .unwrap()
                    .repeat_decode,
                "true"
            );
            assert!(
                !parse_config(&json(r#", "repeat_decode": false"#))
                    .unwrap()
                    .repeat_decode,
                "false"
            );
            let _e = config_err(&json(r#", "repeat_decode": "yes""#));
        }
    }

    #[test]
    fn decisions_name_configured_recordings() {
        assert_eq!(
            config_err(&stage2_json(r#", "decisions": {"r1": {}, "zz": {}}"#)),
            "decisions names unknown recording zz",
            "unknown"
        );
    }

    #[test]
    fn piece_decisions_are_checked() {
        let skip_trim = config_err(&stage2_json(
            r#", "decisions": {"r1": {"pieces": [{"skip": true, "trim_end_s": 1.0}]}}"#,
        ));
        assert!(skip_trim.contains("skipped"), "{skip_trim}");
        let skip_drop = config_err(&stage2_json(
            r#", "decisions": {"r1": {"pieces": [{"skip": true, "drop": [0]}]}}"#,
        ));
        assert!(skip_drop.contains("skipped"), "{skip_drop}");
        assert!(
            parse_config(&stage2_json(
                r#", "decisions": {"r1": {"pieces": [{"skip": false, "trim_end_s": 1.0, "drop": [0]}, {"skip": true}]}}"#
            ))
            .is_ok(),
            "an unskipped piece may trim and drop"
        );
        assert_eq!(
            config_err(&stage2_json(
                r#", "decisions": {"r2": {"pieces": [{}, {"drop": [1, 0, 1]}]}}"#
            )),
            "r2: piece 1 drops phrase 1 twice",
            "repeated drop"
        );
        for t in ["0", "0.0", "-1"] {
            let e = config_err(&stage2_json(&format!(
                r#", "decisions": {{"r1": {{"pieces": [{{"trim_end_s": {t}}}]}}}}"#
            )));
            assert!(e.contains("trim_end_s"), "{t}: {e}");
        }
        assert!(
            parse_config(&stage2_json(
                r#", "decisions": {"r1": {"pieces": [{"trim_end_s": 0.001}]}}"#
            ))
            .is_ok(),
            "a small positive trim"
        );
    }

    #[test]
    fn splits_are_positive_and_strictly_increasing() {
        for (splits, want) in [
            ("[0]", "r1: split_s 0 is not > 0"),
            ("[-1.5]", "r1: split_s -1.5 is not > 0"),
            ("[1.0, 1.0]", "r1: split_s is not strictly increasing at 1"),
            ("[2.0, 1.0]", "r1: split_s is not strictly increasing at 1"),
        ] {
            assert_eq!(
                config_err(&stage2_json(&format!(
                    r#", "decisions": {{"r1": {{"split_s": {splits}}}}}"#
                ))),
                want,
                "{splits}"
            );
        }
        assert!(
            parse_config(&stage2_json(
                r#", "decisions": {"r1": {"split_s": [0.5, 0.75, 3]}}"#
            ))
            .is_ok(),
            "increasing"
        );
    }

    #[test]
    fn a_bad_json_is_a_config_error() {
        let _e = config_err("{");
    }

    // ---- hashing ---------------------------------------------------------------------

    /// Reads from `data`, checking that every read asks for one 64 KiB chunk.
    struct Chunked<'a>(&'a [u8]);

    impl Read for Chunked<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            assert_eq!(buf.len(), 65_536, "reads in 64 KiB chunks");
            self.0.read(buf)
        }
    }

    struct Broken;

    impl Read for Broken {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("disk on fire"))
        }
    }

    #[test]
    fn sha256_streams_a_reader() {
        assert_eq!(
            sha256_hex(&mut b"abc".as_slice()).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            "abc"
        );
        assert_eq!(
            sha256_hex(&mut b"".as_slice()).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "empty"
        );
        let big: Vec<u8> = (0..65_537u32)
            .map(|i| u8::try_from(i % 251).unwrap())
            .collect();
        let want = Sha256::digest(&big)
            .iter()
            .fold(String::new(), |hex, b| hex + &format!("{b:02x}"));
        assert_eq!(
            sha256_hex(&mut Chunked(&big)).unwrap(),
            want,
            "across a chunk boundary"
        );
        assert_eq!(
            sha256_hex(&mut Broken),
            Err(ExtractError::Io("disk on fire".into())),
            "a failed read"
        );
    }

    // ---- input -----------------------------------------------------------------------

    #[test]
    fn input_kinds_name_themselves() {
        assert_eq!(InputKind::SegmentWavs.label(), "segment-wavs", "segments");
        assert_eq!(InputKind::M4a.label(), "m4a-decoded-single-segment", "m4a");
        assert_eq!(InputKind::Wav.label(), "wav-single-segment", "wav");
        assert_eq!(InputKind::SegmentWavs.audio_file(), "segments", "segments");
        assert_eq!(InputKind::M4a.audio_file(), "audio.m4a", "m4a");
        assert_eq!(InputKind::Wav.audio_file(), "audio.wav", "wav");
    }

    #[test]
    fn auto_prefers_segments_then_m4a_then_wav() {
        for (m4a, wav) in [(false, false), (true, false), (false, true), (true, true)] {
            assert_eq!(
                resolve_input(InputMode::Auto, true, m4a, wav),
                Ok(InputKind::SegmentWavs),
                "segments win ({m4a}, {wav})"
            );
        }
        assert_eq!(
            resolve_input(InputMode::Auto, false, true, true),
            Ok(InputKind::M4a),
            "m4a over wav"
        );
        assert_eq!(
            resolve_input(InputMode::Auto, false, true, false),
            Ok(InputKind::M4a),
            "m4a"
        );
        assert_eq!(
            resolve_input(InputMode::Auto, false, false, true),
            Ok(InputKind::Wav),
            "wav"
        );
        let _none = rec_err(resolve_input(InputMode::Auto, false, false, false));
    }

    #[test]
    fn segments_mode_needs_segments() {
        assert_eq!(
            resolve_input(InputMode::Segments, true, false, false),
            Ok(InputKind::SegmentWavs),
            "segments"
        );
        for (m4a, wav) in [(false, false), (true, true), (true, false), (false, true)] {
            assert_eq!(
                rec_err(resolve_input(InputMode::Segments, false, m4a, wav)),
                "input \"segments\" requires segments/segment-NNNN.wav files, found none",
                "({m4a}, {wav})"
            );
        }
    }

    #[test]
    fn m4a_mode_ignores_segments() {
        for segments in [false, true] {
            assert_eq!(
                resolve_input(InputMode::M4a, segments, true, true),
                Ok(InputKind::M4a),
                "m4a ({segments})"
            );
            assert_eq!(
                resolve_input(InputMode::M4a, segments, false, true),
                Ok(InputKind::Wav),
                "wav ({segments})"
            );
            let _none = rec_err(resolve_input(InputMode::M4a, segments, false, false));
        }
    }

    #[test]
    fn segment_wavs_are_listed_in_index_order() {
        let names = [
            "segment-0010.wav",
            "notes.txt",
            "segment-0003.wav",
            "segment-00a1.wav",
            "segment-0002.WAV",
            "segment-0004.wav.bak",
            "segment-9.wav",
            "segment-0000.wav",
        ];
        assert_eq!(
            segment_wav_names(&names),
            [
                "segment-0000.wav",
                "segment-0003.wav",
                "segment-9.wav",
                "segment-0010.wav"
            ],
            "by index, others ignored"
        );
        assert!(segment_wav_names(&[]).is_empty(), "empty");
        assert!(
            segment_wav_names(&["segment-x.wav", "audio.wav"]).is_empty(),
            "none"
        );
    }

    #[test]
    fn the_two_segments_fixture_reads() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/extract/two-segments/segments");
        let listed: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        let refs: Vec<&str> = listed.iter().map(String::as_str).collect();
        let names = segment_wav_names(&refs);
        assert_eq!(names, ["segment-0000.wav", "segment-0003.wav"], "names");
        let a = decode_wav(&std::fs::read(dir.join(&names[0])).unwrap()).unwrap();
        let b = decode_wav(&std::fs::read(dir.join(&names[1])).unwrap()).unwrap();
        assert_eq!(a.len(), 16_000, "first length");
        assert_eq!(b.len(), 8_000, "second length");
        assert!(a[0] > 0.0, "first is positive");
        assert!(b[0] < 0.0, "second is negative");
        assert_eq!(
            resolve_input(InputMode::Auto, !names.is_empty(), false, false),
            Ok(InputKind::SegmentWavs),
            "auto reads them"
        );
    }

    #[test]
    fn afconvert_decodes_to_16k_mono_float() {
        assert_eq!(
            afconvert_args(Path::new("/r/audio.m4a"), Path::new("/o/r.decoded.tmp.wav")),
            [
                "-f",
                "WAVE",
                "-d",
                "LEF32@16000",
                "-c",
                "1",
                "/r/audio.m4a",
                "/o/r.decoded.tmp.wav"
            ],
            "argv"
        );
    }

    // ---- float WAV -------------------------------------------------------------------

    #[test]
    fn the_float_wav_header_is_pinned() {
        // Byte rate 16000 * 4 = 64000 = 0x0000FA00: bytes 28..32 are 0, 250, 0, 0.
        assert_eq!(
            f32_wav_header(0).unwrap(),
            [
                82, 73, 70, 70, 36, 0, 0, 0, 87, 65, 86, 69, 102, 109, 116, 32, 16, 0, 0, 0, 3, 0,
                1, 0, 128, 62, 0, 0, 0, 250, 0, 0, 4, 0, 32, 0, 100, 97, 116, 97, 0, 0, 0, 0
            ],
            "n = 0"
        );
        let h = f32_wav_header(3).unwrap();
        assert_eq!(h[4..8], 48u32.to_le_bytes(), "riff size");
        assert_eq!(h[40..44], 12u32.to_le_bytes(), "data size");
        let last = f32_wav_header(1_073_741_814).unwrap();
        assert_eq!(
            last[4..8],
            u32::MAX.wrapping_sub(3).to_le_bytes(),
            "largest"
        );
        assert!(
            matches!(f32_wav_header(1_073_741_815), Err(ExtractError::Wav(_))),
            "riff size overflows"
        );
        assert!(
            matches!(f32_wav_header(1_073_741_824), Err(ExtractError::Wav(_))),
            "data size overflows"
        );
        let past_u32 = usize::try_from(u64::from(u32::MAX) + 1).unwrap();
        assert!(
            matches!(f32_wav_header(past_u32), Err(ExtractError::Wav(_))),
            "past u32"
        );
    }

    #[test]
    fn float_wavs_round_trip_bit_exactly() {
        let samples = [0.0, -0.0, 1.5, -1.0, f32::MIN_POSITIVE, f32::NAN, 0.1];
        let bytes = write_f32_wav(&samples).unwrap();
        assert_eq!(bytes.len(), 44 + 4 * samples.len(), "length");
        assert_eq!(
            bytes[..44],
            f32_wav_header(samples.len()).unwrap(),
            "header"
        );
        assert_eq!(bytes[44..48], 0.0f32.to_le_bytes(), "first sample");
        let back = read_f32_wav(&bytes).unwrap();
        let want: Vec<u32> = samples.iter().map(|s| s.to_bits()).collect();
        let got: Vec<u32> = back.iter().map(|s| s.to_bits()).collect();
        assert_eq!(got, want, "bits");
        assert_eq!(
            write_f32_wav(&[]).unwrap(),
            f32_wav_header(0).unwrap(),
            "empty is the header"
        );
        assert!(
            read_f32_wav(&write_f32_wav(&[]).unwrap())
                .unwrap()
                .is_empty(),
            "empty"
        );
    }

    fn chunk(id: [u8; 4], body: &[u8]) -> Vec<u8> {
        let mut c = id.to_vec();
        c.extend_from_slice(&u32::try_from(body.len()).unwrap().to_le_bytes());
        c.extend_from_slice(body);
        if body.len() % 2 == 1 {
            c.push(0);
        }
        c
    }

    fn fmt(tag: u16, channels: u16, rate: u32, bits: u16) -> Vec<u8> {
        let mut f = Vec::new();
        f.extend_from_slice(&tag.to_le_bytes());
        f.extend_from_slice(&channels.to_le_bytes());
        f.extend_from_slice(&rate.to_le_bytes());
        f.extend_from_slice(&rate.checked_mul(4).unwrap().to_le_bytes());
        f.extend_from_slice(&4u16.to_le_bytes());
        f.extend_from_slice(&bits.to_le_bytes());
        chunk(*b"fmt ", &f)
    }

    fn riff(chunks: &[Vec<u8>]) -> Vec<u8> {
        let body = chunks.concat();
        let mut r = b"RIFF".to_vec();
        r.extend_from_slice(
            &u32::try_from(body.len().checked_add(4).unwrap())
                .unwrap()
                .to_le_bytes(),
        );
        r.extend_from_slice(b"WAVE");
        r.extend_from_slice(&body);
        r
    }

    fn data(samples: &[f32]) -> Vec<u8> {
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        chunk(*b"data", &bytes)
    }

    fn wav_err(bytes: &[u8]) -> String {
        match read_f32_wav(bytes) {
            Err(ExtractError::Wav(m)) => m,
            other => panic!("expected a wav error, got {other:?}"),
        }
    }

    #[test]
    fn afconvert_style_float_wavs_read() {
        let good = fmt(3, 1, 16_000, 32);
        let samples = [0.25f32, -0.5];
        assert_eq!(
            read_f32_wav(&riff(&[good.clone(), data(&samples)])).unwrap(),
            samples,
            "plain"
        );
        assert_eq!(
            read_f32_wav(&riff(&[
                good.clone(),
                chunk(*b"fact", &2u32.to_le_bytes()),
                data(&samples)
            ]))
            .unwrap(),
            samples,
            "a fact chunk is skipped"
        );
        assert_eq!(
            read_f32_wav(&riff(&[
                chunk(*b"FLLR", &[1, 2, 3]),
                good.clone(),
                data(&samples)
            ]))
            .unwrap(),
            samples,
            "an odd-sized chunk's padding is honoured"
        );
        assert_eq!(
            wav_err(&riff(&[fmt(1, 1, 16_000, 32), data(&samples)])),
            "not IEEE float (format tag 3)",
            "tag"
        );
        assert_eq!(
            wav_err(&riff(&[fmt(3, 2, 16_000, 32), data(&samples)])),
            "not mono",
            "channels"
        );
        assert_eq!(
            wav_err(&riff(&[fmt(3, 1, 44_100, 32), data(&samples)])),
            "not 16 kHz",
            "rate"
        );
        assert_eq!(
            wav_err(&riff(&[fmt(3, 1, 16_000, 64), data(&samples)])),
            "not 32 bits per sample",
            "bits"
        );
        assert_eq!(
            wav_err(&riff(std::slice::from_ref(&good))),
            "no data chunk",
            "no data"
        );
        assert_eq!(wav_err(&riff(&[data(&samples)])), "no fmt chunk", "no fmt");
        let whole = riff(&[good.clone(), data(&samples)]);
        assert_eq!(
            wav_err(&whole[..whole.len() - 1]),
            "truncated",
            "short data"
        );
        let mut tail = whole.clone();
        tail.extend_from_slice(b"xy");
        assert_eq!(wav_err(&tail), "truncated", "a partial chunk id");
        let mut size = whole.clone();
        size.extend_from_slice(b"junk\x01\x00");
        assert_eq!(wav_err(&size), "truncated", "a partial chunk size");
        assert_eq!(
            wav_err(b"RIFF\0\0\0\0WAVX"),
            "not a RIFF WAVE file",
            "not wave"
        );
        assert_eq!(
            wav_err(b"RIFX\0\0\0\0WAVE"),
            "not a RIFF WAVE file",
            "not riff"
        );
        assert_eq!(wav_err(b""), "not a RIFF WAVE file", "empty");
        let odd = riff(&[good, chunk(*b"data", &[0, 0, 128, 62, 9])]);
        assert_eq!(
            read_f32_wav(&odd).unwrap(),
            [0.25],
            "a partial sample is ignored"
        );
    }

    const FLOAT_GUID: [u8; 16] = [
        3, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71,
    ];

    /// afconvert's `fmt ` chunk: format tag 0xFFFE, then cbSize 22, valid
    /// bits, channel mask and the `SubFormat` GUID.
    fn extensible_fmt(guid: [u8; 16], len: usize) -> Vec<u8> {
        let mut f = Vec::new();
        f.extend_from_slice(&0xFFFEu16.to_le_bytes());
        f.extend_from_slice(&1u16.to_le_bytes());
        f.extend_from_slice(&16_000u32.to_le_bytes());
        f.extend_from_slice(&64_000u32.to_le_bytes());
        f.extend_from_slice(&4u16.to_le_bytes());
        f.extend_from_slice(&32u16.to_le_bytes());
        f.extend_from_slice(&22u16.to_le_bytes());
        f.extend_from_slice(&32u16.to_le_bytes());
        f.extend_from_slice(&4u32.to_le_bytes());
        f.extend_from_slice(&guid);
        f.truncate(len);
        chunk(*b"fmt ", &f)
    }

    #[test]
    fn afconverts_extensible_float_wav_reads() {
        let samples = [0.25f32, -0.5, 0.125];
        let fllr = chunk(*b"FLLR", &[0u8; 4020]);
        assert_eq!(
            read_f32_wav(&riff(&[
                extensible_fmt(FLOAT_GUID, 40),
                fllr.clone(),
                data(&samples)
            ]))
            .unwrap(),
            samples,
            "extensible IEEE float"
        );
        let mut pcm = FLOAT_GUID;
        pcm[0] = 1;
        assert_eq!(
            wav_err(&riff(&[extensible_fmt(pcm, 40), data(&samples)])),
            "extensible SubFormat is not IEEE float",
            "PCM SubFormat"
        );
        assert_eq!(
            wav_err(&riff(&[extensible_fmt(FLOAT_GUID, 16), data(&samples)])),
            "extensible fmt chunk shorter than 40 bytes",
            "short fmt"
        );
        assert_eq!(
            wav_err(&riff(&[extensible_fmt(FLOAT_GUID, 39), data(&samples)])),
            "extensible fmt chunk shorter than 40 bytes",
            "one byte short"
        );
        let mut last = FLOAT_GUID;
        last[15] = 0x70;
        assert_eq!(
            wav_err(&riff(&[extensible_fmt(last, 40), data(&samples)])),
            "extensible SubFormat is not IEEE float",
            "the whole GUID is compared"
        );
        let tag3 = riff(&[fmt(3, 1, 16_000, 32), data(&samples)]);
        assert_eq!(read_f32_wav(&tag3).unwrap(), samples, "tag 3 still reads");
    }

    // ---- checks ----------------------------------------------------------------------

    #[test]
    fn spans_are_checked() {
        assert_eq!(check_spans(&[], 0.0), Ok(()), "none");
        assert_eq!(
            check_spans(&[(0.0, 0.0), (0.0, 1.0), (0.5, 3.0)], 2.0),
            Ok(()),
            "equal starts, ending exactly 1 s past"
        );
        assert_eq!(
            rec_err(check_spans(&[(0.0, 1.0), (1.5, 1.0)], 2.0)),
            "span 1 (1.5-1 s of 2 s) is inverted",
            "inverted"
        );
        assert_eq!(
            rec_err(check_spans(&[(-0.5, 1.0)], 2.0)),
            "span 0 (-0.5-1 s of 2 s) starts before 0",
            "negative"
        );
        assert_eq!(
            rec_err(check_spans(&[(0.0, 3.000_001)], 2.0)),
            "span 0 (0-3.000001 s of 2 s) ends past the audio",
            "too long"
        );
        assert_eq!(
            rec_err(check_spans(&[(1.0, 1.5), (0.5, 1.5)], 2.0)),
            "span 1 (0.5-1.5 s of 2 s) is out of order",
            "out of order"
        );
    }

    #[test]
    fn phrases_are_checked() {
        assert_eq!(
            check_phrases(&[(0.0, 1.0), (1.0, 2.0)], 1.0),
            Ok(()),
            "ending 1 s past"
        );
        assert_eq!(
            rec_err(check_phrases(&[(0.0, 1.0), (1.0, 2.5)], 1.0)),
            "phrase 1 (1-2.5 s of 1 s) ends past the audio",
            "too long"
        );
        assert_eq!(
            rec_err(check_phrases(&[(2.0, 1.0)], 5.0)),
            "phrase 0 (2-1 s of 5 s) is inverted",
            "inverted"
        );
        assert_eq!(
            rec_err(check_phrases(&[(-1.0, 1.0)], 5.0)),
            "phrase 0 (-1-1 s of 5 s) starts before 0",
            "negative"
        );
        assert_eq!(
            rec_err(check_phrases(&[(1.0, 2.0), (0.0, 2.0)], 5.0)),
            "phrase 1 (0-2 s of 5 s) is out of order",
            "out of order"
        );
    }

    // ---- stage 2 ---------------------------------------------------------------------

    fn piece(range: std::ops::Range<usize>) -> PlannedPiece {
        PlannedPiece {
            decode_len: range.len(),
            range,
            skip: false,
            trim_end_s: None,
            drop: vec![],
        }
    }

    fn splits(s: &[f64]) -> Decisions {
        Decisions {
            split_s: Some(s.to_vec()),
            pieces: None,
        }
    }

    fn pieces(p: Vec<PieceDecision>) -> Decisions {
        Decisions {
            split_s: None,
            pieces: Some(p),
        }
    }

    fn keep() -> PieceDecision {
        PieceDecision::default()
    }

    #[test]
    fn no_decisions_is_one_ungated_piece_per_input_segment() {
        let want = vec![piece(0..16_000), piece(16_000..24_000)];
        assert_eq!(plan_pieces(&[16_000, 8_000], None).unwrap(), want, "none");
        assert_eq!(
            plan_pieces(&[16_000, 8_000], Some(&Decisions::default())).unwrap(),
            want,
            "an empty entry"
        );
        let empty_lists = Decisions {
            split_s: Some(vec![]),
            pieces: Some(vec![]),
        };
        assert_eq!(
            plan_pieces(&[16_000, 8_000], Some(&empty_lists)).unwrap(),
            want,
            "empty lists"
        );
        assert_eq!(
            plan_pieces(&[16_000, 8_000], Some(&pieces(vec![]))).unwrap(),
            want,
            "an empty piece list alone"
        );
        assert_eq!(plan_pieces(&[], None).unwrap(), vec![], "no input");
        assert_eq!(
            plan_pieces(&[0, 8_000], None).unwrap(),
            vec![piece(0..0), piece(0..8_000)],
            "an empty input segment"
        );
    }

    #[test]
    fn splits_cut_input_segments() {
        assert_eq!(
            plan_pieces(&[32_000, 16_000], Some(&splits(&[0.5]))).unwrap(),
            vec![piece(0..8_000), piece(8_000..32_000), piece(32_000..48_000)],
            "inside the first"
        );
        assert_eq!(
            plan_pieces(&[32_000, 16_000], Some(&splits(&[0.5, 2.5]))).unwrap(),
            vec![
                piece(0..8_000),
                piece(8_000..32_000),
                piece(32_000..40_000),
                piece(40_000..48_000)
            ],
            "in both"
        );
        assert_eq!(
            plan_pieces(&[32_000, 16_000], Some(&splits(&[0.5, 1.0, 2.999_9]))).unwrap(),
            vec![
                piece(0..8_000),
                piece(8_000..16_000),
                piece(16_000..32_000),
                piece(32_000..47_998),
                piece(47_998..48_000)
            ],
            "two in one, one just short of the end"
        );
    }

    #[test]
    fn bad_splits_fail_the_recording() {
        let lengths = [32_000, 16_000];
        for bad in [
            vec![1.0, 0.5],
            vec![0.0],
            vec![3.0],
            vec![3.5],
            vec![2.0],
            vec![0.5, 0.500_01],
            vec![-1.0],
        ] {
            let e = rec_err(plan_pieces(&lengths, Some(&splits(&bad))));
            let last = bad.last().unwrap();
            assert!(e.contains(&last.to_string()), "{bad:?}: {e}");
        }
    }

    #[test]
    fn piece_decisions_apply_per_piece() {
        let d = pieces(vec![
            PieceDecision {
                skip: Some(true),
                ..keep()
            },
            PieceDecision {
                trim_end_s: Some(0.25),
                drop: Some(vec![1, 0]),
                ..keep()
            },
        ]);
        assert_eq!(
            plan_pieces(&[16_000, 8_000], Some(&d)).unwrap(),
            vec![
                PlannedPiece {
                    range: 0..16_000,
                    skip: true,
                    decode_len: 0,
                    trim_end_s: None,
                    drop: vec![]
                },
                PlannedPiece {
                    range: 16_000..24_000,
                    skip: false,
                    decode_len: 4_000,
                    trim_end_s: Some(0.25),
                    drop: vec![1, 0]
                }
            ],
            "skip, then trim and drop"
        );
        let unskipped = pieces(vec![
            PieceDecision {
                skip: Some(false),
                trim_end_s: Some(1.0),
                drop: Some(vec![]),
            },
            keep(),
        ]);
        assert_eq!(
            plan_pieces(&[16_000, 8_000], Some(&unskipped)).unwrap(),
            vec![
                PlannedPiece {
                    trim_end_s: Some(1.0),
                    ..piece(0..16_000)
                },
                piece(16_000..24_000)
            ],
            "skip false, trimmed to its whole length"
        );
    }

    #[test]
    fn splits_and_pieces_combine() {
        let d = Decisions {
            split_s: Some(vec![0.5, 2.5]),
            pieces: Some(vec![
                keep(),
                PieceDecision {
                    skip: Some(true),
                    ..keep()
                },
                PieceDecision {
                    trim_end_s: Some(0.1),
                    ..keep()
                },
                PieceDecision {
                    drop: Some(vec![0]),
                    ..keep()
                },
            ]),
        };
        let plan = plan_pieces(&[32_000, 16_000], Some(&d)).unwrap();
        assert_eq!(
            plan,
            vec![
                piece(0..8_000),
                PlannedPiece {
                    skip: true,
                    decode_len: 0,
                    ..piece(8_000..32_000)
                },
                PlannedPiece {
                    decode_len: 1_600,
                    trim_end_s: Some(0.1),
                    ..piece(32_000..40_000)
                },
                PlannedPiece {
                    drop: vec![0],
                    ..piece(40_000..48_000)
                },
            ],
            "combined"
        );
        assert_eq!(check_contiguous(&plan, 48_000), Ok(()), "contiguous");
    }

    #[test]
    fn piece_lists_and_trims_must_fit() {
        assert_eq!(
            rec_err(plan_pieces(&[16_000, 8_000], Some(&pieces(vec![keep()])))),
            "decisions pieces has 1 entries, recording has 2 pieces",
            "too few"
        );
        let empty_pieces = Decisions {
            split_s: Some(vec![0.5]),
            pieces: Some(vec![]),
        };
        assert_eq!(
            rec_err(plan_pieces(&[16_000, 8_000], Some(&empty_pieces))),
            "decisions pieces has 0 entries, recording has 3 pieces",
            "an empty list with a split"
        );
        let trim = |t: f64| {
            pieces(vec![
                PieceDecision {
                    trim_end_s: Some(t),
                    ..keep()
                },
                keep(),
            ])
        };
        for bad in [0.0, 0.000_01, 1.000_1, 2.0, -1.0] {
            let e = rec_err(plan_pieces(&[16_000, 8_000], Some(&trim(bad))));
            assert!(e.contains(&bad.to_string()), "{bad}: {e}");
        }
        assert_eq!(
            plan_pieces(&[16_000, 8_000], Some(&trim(0.000_04))).unwrap()[0].decode_len,
            1,
            "one sample"
        );
    }

    #[test]
    fn pieces_must_be_contiguous() {
        assert_eq!(check_contiguous(&[], 0), Ok(()), "empty");
        assert_eq!(
            rec_err(check_contiguous(&[], 5)),
            "the pieces end at sample 0, not 5",
            "nothing for something"
        );
        assert_eq!(
            rec_err(check_contiguous(&[piece(1..5)], 5)),
            "piece 0 starts at sample 1, not 0",
            "late start"
        );
        assert_eq!(
            rec_err(check_contiguous(&[piece(0..5), piece(6..8)], 8)),
            "piece 1 starts at sample 6, not 5",
            "gap"
        );
        assert_eq!(
            rec_err(check_contiguous(&[piece(0..5), piece(4..8)], 8)),
            "piece 1 starts at sample 4, not 5",
            "overlap"
        );
        assert_eq!(
            rec_err(check_contiguous(&[piece(0..5), piece(5..8)], 9)),
            "the pieces end at sample 8, not 9",
            "short"
        );
        assert_eq!(
            check_contiguous(&[piece(0..5), piece(5..8)], 8),
            Ok(()),
            "ok"
        );
    }

    fn skipped_piece(range: std::ops::Range<usize>) -> PlannedPiece {
        PlannedPiece {
            skip: true,
            decode_len: 0,
            ..piece(range)
        }
    }

    fn with_nsp(mut d: DetailedPhrase, p: f32) -> DetailedPhrase {
        d.no_speech_probability = p;
        d
    }

    #[test]
    fn no_gating_joins_each_input_segment_like_production() {
        let plan = plan_pieces(&[32_000, 16_000], None).unwrap();
        let out = assemble_pieces(
            &plan,
            &quiet(48_000),
            &[
                decoded(
                    vec![dp(" Hello", 0, 8_000), dp(" world. ", 8_000, 32_000)],
                    12,
                ),
                decoded(vec![dp(" Second.", 1_600, 16_000)], 7),
            ],
            None,
        )
        .unwrap();
        assert_eq!(
            out.text.as_deref(),
            Some("Hello world.\n\nSecond."),
            "joined as production joins parts"
        );
        assert!(!out.skipped, "not skipped");
        let second = &out.pieces[1];
        assert_eq!(second.index, 1, "index");
        assert_eq!(bits(second.start_s), bits(2.0), "start");
        assert_eq!(bits(second.end_s), bits(3.0), "end");
        assert_eq!(second.elapsed_ms, 7, "elapsed");
        assert_eq!(second.trim_end_s, None, "no trim");
        assert!(!second.skipped, "not skipped");
        assert_eq!(second.text.as_deref(), Some("Second."), "piece text");
        let p = &second.phrases[0];
        assert_eq!(p.phrase.index, 0, "phrase index");
        assert_eq!(
            bits(p.phrase.t0_s),
            bits(33_600.0 / 16_000.0),
            "absolute t0"
        );
        assert_eq!(bits(p.phrase.t1_s), bits(3.0), "absolute t1");
        assert_eq!(p.phrase.text, " Second.", "raw text");
        assert!(!p.dropped, "kept");
        assert_eq!(
            out.pieces[0].phrases[1].phrase.index, 1,
            "second phrase index"
        );

        let one = assemble_pieces(
            &plan_pieces(&[16_000], None).unwrap(),
            &quiet(16_000),
            &[decoded(vec![dp(" One", 0, 100), dp(" two ", 100, 200)], 1)],
            None,
        )
        .unwrap();
        assert_eq!(one.text.as_deref(), Some("One two"), "one segment");
    }

    #[test]
    fn skipped_and_dropped_pieces_leave_no_text() {
        let plan = vec![
            skipped_piece(0..100),
            PlannedPiece {
                drop: vec![0, 1],
                trim_end_s: Some(0.001),
                decode_len: 16,
                ..piece(100..200)
            },
            PlannedPiece {
                drop: vec![1],
                ..piece(200..300)
            },
        ];
        let out = assemble_pieces(
            &plan,
            &quiet(300),
            &[
                Decoded::default(),
                decoded(vec![dp(" a", 0, 1), dp(" b", 1, 2)], 3),
                decoded(vec![dp(" c", 0, 1), dp(" Thank you.", 1, 2)], 4),
            ],
            None,
        )
        .unwrap();
        assert_eq!(
            out.text.as_deref(),
            Some("c"),
            "only the kept text, no separators"
        );
        assert_eq!(out.pieces[0].text, None, "skipped piece");
        assert!(out.pieces[0].skipped, "skipped flag");
        assert_eq!(out.pieces[1].text.as_deref(), Some(""), "all dropped");
        assert_eq!(out.pieces[1].trim_end_s, Some(0.001), "trim echoed");
        assert!(out.pieces[1].phrases[1].dropped, "dropped flag");
        assert!(!out.pieces[2].phrases[0].dropped, "kept flag");
        assert!(out.pieces[2].phrases[1].dropped, "last dropped");
        assert!(!out.skipped, "not every piece skipped");

        let all = assemble_pieces(
            &[skipped_piece(0..100), skipped_piece(100..200)],
            &quiet(200),
            &[Decoded::default(), Decoded::default()],
            None,
        )
        .unwrap();
        assert!(all.skipped, "every piece skipped");
        assert_eq!(all.text, None, "no text");
        assert_eq!(all.text_undropped, None, "no undropped text");

        let empty = assemble_pieces(&[], &[], &[], None).unwrap();
        assert!(!empty.skipped, "no pieces is not skipped");
        assert_eq!(empty.text.as_deref(), Some(""), "empty text");

        let nothing =
            assemble_pieces(&[piece(0..100)], &quiet(100), &[Decoded::default()], None).unwrap();
        assert_eq!(nothing.text.as_deref(), Some(""), "nothing heard");
        assert_eq!(
            nothing.pieces[0].text_undropped.as_deref(),
            Some(""),
            "default decode"
        );
    }

    #[test]
    fn a_stage2_phrase_times_come_from_the_piece_start_by_integer_samples() {
        let at = |start: usize, p: DetailedPhrase| {
            let plan = [piece(start..start + 16_000)];
            let timeline = quiet(start + 16_000);
            let out = assemble_pieces(&plan, &timeline, &[decoded(vec![p], 1)], None).unwrap();
            let phrase = out.pieces[0].phrases[0].phrase.clone();
            (phrase.t0_s, phrase.t1_s)
        };
        let (t0, t1) = at(16_000, dp(" a", 0, 8_000));
        assert_eq!((bits(t0), bits(t1)), (bits(1.0), bits(1.5)), "dyadic");
        let (odd0, odd1) = at(16_001, dp(" a", 0, 3));
        assert_eq!(odd0, samples_to_seconds(16_001).unwrap(), "t0 exact");
        assert_eq!(odd1, samples_to_seconds(16_004).unwrap(), "t1 exact");
    }

    #[test]
    fn stage2_texts_with_and_without_drops() {
        let plan = [PlannedPiece {
            drop: vec![1],
            ..piece(0..16_000)
        }];
        let out = assemble_pieces(
            &plan,
            &quiet(16_000),
            &[decoded(
                vec![dp(" a", 0, 8_000), dp(" b", 8_000, 16_000)],
                1,
            )],
            None,
        )
        .unwrap();
        let p = &out.pieces[0];
        assert_eq!(p.text.as_deref(), Some("a"), "text");
        assert_eq!(p.text_undropped.as_deref(), Some("a b"), "undropped");
        assert_eq!(out.text.as_deref(), Some("a"), "recording text");
        assert_eq!(
            out.text_undropped.as_deref(),
            Some("a b"),
            "recording undropped"
        );

        let two = assemble_pieces(
            &[piece(0..8_000), piece(8_000..16_000)],
            &quiet(16_000),
            &[
                decoded(vec![dp(" a", 0, 100)], 1),
                decoded(vec![dp(" b", 0, 100)], 1),
            ],
            None,
        )
        .unwrap();
        assert_eq!(two.text_undropped, Some(join(["a", "b"])), "joined");

        let mixed = assemble_pieces(
            &[skipped_piece(0..100), piece(100..200)],
            &quiet(200),
            &[Decoded::default(), decoded(vec![dp(" x", 0, 5)], 1)],
            None,
        )
        .unwrap();
        assert_eq!(mixed.pieces[0].text_undropped, None, "skipped piece");
        assert_eq!(
            mixed.text_undropped.as_deref(),
            Some("x"),
            "skips add nothing"
        );
        let v = serde_json::to_value(&mixed.pieces[0]).unwrap();
        assert!(v["text_undropped"].is_null(), "null for a skipped piece");
    }

    #[test]
    fn assembly_checks_its_inputs() {
        let plan = vec![PlannedPiece {
            drop: vec![2],
            ..piece(0..100)
        }];
        assert_eq!(
            rec_err(assemble_pieces(
                &plan,
                &quiet(100),
                &[decoded(vec![dp("a", 0, 1), dp("b", 1, 2)], 0)],
                None
            )),
            "piece 0: drop index 2 is past its 2 phrases",
            "drop out of range"
        );
        assert!(
            assemble_pieces(
                &plan,
                &quiet(100),
                &[decoded(
                    vec![dp("a", 0, 1), dp("b", 1, 2), dp("c", 2, 3)],
                    0
                )],
                None
            )
            .is_ok(),
            "the last index"
        );
        let two = vec![piece(0..1), piece(1..2)];
        assert_eq!(
            rec_err(assemble_pieces(
                &two,
                &quiet(2),
                &[Decoded::default()],
                None
            )),
            "2 pieces, but 1 decodes",
            "decodes"
        );
        let repeats = [Some(Decoded::default())];
        assert_eq!(
            rec_err(assemble_pieces(
                &two,
                &quiet(2),
                &[Decoded::default(), Decoded::default()],
                Some(&repeats)
            )),
            "2 pieces, but 1 repeat decodes",
            "repeats"
        );
        assert_eq!(
            rec_err(assemble_pieces(
                &[piece(0..10)],
                &quiet(9),
                &[Decoded::default()],
                None
            )),
            "piece 0: ends at sample 10, past the audio",
            "past the audio"
        );
        assert!(
            assemble_pieces(&[piece(0..10)], &quiet(10), &[Decoded::default()], None).is_ok(),
            "exactly the audio"
        );
        assert!(
            rec_err(assemble_pieces(
                &[piece(0..10)],
                &quiet(10),
                &[decoded(vec![dp("a", 0, 20_000)], 0)],
                None
            ))
            .contains("phrase"),
            "phrase past the decode"
        );
        let both = [Decoded::default(), Decoded::default()];
        assert_eq!(
            rec_err(assemble_pieces(
                &two,
                &quiet(2),
                &both,
                Some(&[Some(Decoded::default()), None])
            )),
            "piece 1: no repeat decode",
            "missing repeat"
        );
        assert!(
            rec_err(assemble_pieces(
                &[piece(0..10)],
                &quiet(10),
                &[Decoded::default()],
                Some(&[Some(decoded(vec![dp("a", 0, 20_000)], 0))])
            ))
            .contains("phrase"),
            "phrase past the repeat"
        );
    }

    #[test]
    fn repeats_fill_each_piece_and_the_recording() {
        let plan = [piece(0..100), skipped_piece(100..200), piece(200..300)];
        let timeline = quiet(300);
        let first = decoded(vec![dp(" a", 0, 5)], 1);
        let same = first.clone();
        let other = decoded(vec![with_nsp(dp(" a", 0, 5), 0.5)], 1);
        let decodes = [first.clone(), Decoded::default(), first.clone()];
        let on = assemble_pieces(
            &plan,
            &timeline,
            &decodes,
            Some(&[Some(same.clone()), Some(other.clone()), Some(same.clone())]),
        )
        .unwrap();
        assert_eq!(on.repeat_all_equal, Some(true), "all equal");
        assert_eq!(on.pieces[1].repeat, None, "skipped piece ignores its entry");
        assert_eq!(
            on.pieces[0].repeat,
            Some(compare_repeat(&first, &same)),
            "piece"
        );
        let differing = assemble_pieces(
            &plan,
            &timeline,
            &decodes,
            Some(&[Some(same.clone()), None, Some(other.clone())]),
        )
        .unwrap();
        assert_eq!(differing.repeat_all_equal, Some(false), "one differs");
        assert_eq!(
            differing.pieces[2]
                .repeat
                .as_ref()
                .unwrap()
                .no_speech_probability_max_abs_diff,
            Some(0.25),
            "shows the difference"
        );
        let off = assemble_pieces(&plan, &timeline, &decodes, None).unwrap();
        assert_eq!(off.repeat_all_equal, None, "off");
        assert!(off.pieces.iter().all(|p| p.repeat.is_none()), "no repeats");
        let skipped = assemble_pieces(
            &[skipped_piece(0..10)],
            &quiet(10),
            &[Decoded::default()],
            Some(&[None]),
        )
        .unwrap();
        assert_eq!(skipped.repeat_all_equal, Some(true), "vacuous");
    }

    // ---- repeat comparison -----------------------------------------------------------

    fn three(probs: [f32; 3]) -> Decoded {
        decoded(
            vec![
                with_nsp(dp(" a", 0, 1), probs[0]),
                with_nsp(dp(" b", 1, 2), probs[1]),
                with_nsp(dp(" c", 2, 3), probs[2]),
            ],
            1,
        )
    }

    #[test]
    fn comparing_repeats() {
        let a = three([0.0, 0.0, 0.0]);
        let r1 = compare_repeat(&a, &a.clone());
        assert_eq!(
            (
                r1.text_equal,
                r1.no_speech_probability_max_abs_diff,
                r1.segment_count_equal
            ),
            (true, Some(0.0), true),
            "identical"
        );
        assert!(r1.all_equal(), "identical is all equal");

        let mut text = a.clone();
        text.phrases[1].phrase.text = " B".into();
        let r2 = compare_repeat(&a, &text);
        assert_eq!(
            (
                r2.text_equal,
                r2.no_speech_probability_max_abs_diff,
                r2.segment_count_equal
            ),
            (false, Some(0.0), true),
            "one character"
        );
        assert!(!r2.all_equal(), "text differs");

        let r3 = compare_repeat(&a, &three([0.0, 0.25, 0.0]));
        assert_eq!(r3.no_speech_probability_max_abs_diff, Some(0.25), "middle");
        assert!(!r3.all_equal(), "probability differs");
        assert_eq!(
            compare_repeat(&three([0.125, 0.5, 0.25]), &three([0.0, 0.0, 0.0]))
                .no_speech_probability_max_abs_diff,
            Some(0.5),
            "max, not sum or first or last"
        );
        assert_eq!(
            compare_repeat(&three([0.0, 0.5, 0.0]), &three([0.0, 0.0, 0.0]))
                .no_speech_probability_max_abs_diff,
            Some(0.5),
            "negative direction is absolute"
        );
        assert_eq!(
            compare_repeat(&three([0.0, 0.0, 0.0]), &three([0.0, 0.5, 0.0]))
                .no_speech_probability_max_abs_diff,
            Some(0.5),
            "absolute"
        );

        let fewer = decoded(a.phrases[..2].to_vec(), 1);
        let r4 = compare_repeat(&a, &fewer);
        assert_eq!(r4.no_speech_probability_max_abs_diff, None, "counts differ");
        assert!(!r4.segment_count_equal, "count flag");
        let one_phrase = decoded(vec![dp(" ab", 0, 1)], 1);
        let two = decoded(vec![dp(" a", 0, 1), dp("b", 1, 2)], 1);
        assert!(
            compare_repeat(&one_phrase, &two).text_equal,
            "concatenation is untrimmed"
        );
        let joined = decoded(vec![dp(" a", 0, 1)], 1);
        let r5 = compare_repeat(&joined, &decoded(vec![dp(" a", 0, 1), dp("", 1, 1)], 1));
        assert!(r5.text_equal, "equal text, different counts");
        assert!(!r5.segment_count_equal, "counts differ");
        assert!(!r5.all_equal(), "counts differ is not all equal");

        let r6 = compare_repeat(&Decoded::default(), &Decoded::default());
        assert_eq!(
            (
                r6.text_equal,
                r6.no_speech_probability_max_abs_diff,
                r6.segment_count_equal
            ),
            (true, Some(0.0), true),
            "both empty"
        );
        let nan = compare_repeat(
            &decoded(vec![with_nsp(dp("a", 0, 1), f32::NAN)], 1),
            &decoded(vec![with_nsp(dp("a", 0, 1), 0.0)], 1),
        );
        assert!(
            nan.no_speech_probability_max_abs_diff.unwrap().is_nan(),
            "NaN"
        );
        assert!(!nan.all_equal(), "NaN is not equal");
        let nan_later = compare_repeat(
            &decoded(vec![dp("a", 0, 1), with_nsp(dp("a", 1, 2), f32::NAN)], 1),
            &decoded(vec![dp("a", 0, 1), dp("a", 1, 2)], 1),
        );
        assert!(
            nan_later
                .no_speech_probability_max_abs_diff
                .unwrap()
                .is_nan(),
            "NaN after a number"
        );
    }

    #[test]
    fn all_equal_needs_every_field() {
        let ok = Repeat {
            text_equal: true,
            no_speech_probability_max_abs_diff: Some(0.0),
            segment_count_equal: true,
        };
        assert!(ok.all_equal(), "all three");
        let text = Repeat {
            text_equal: false,
            ..ok.clone()
        };
        let count = Repeat {
            segment_count_equal: false,
            ..ok.clone()
        };
        let diff = Repeat {
            no_speech_probability_max_abs_diff: Some(0.125),
            ..ok.clone()
        };
        let none = Repeat {
            no_speech_probability_max_abs_diff: None,
            ..ok.clone()
        };
        for (r, what) in [
            (text, "text"),
            (count, "count"),
            (diff, "diff"),
            (none, "none"),
        ] {
            assert!(!r.all_equal(), "{what}");
        }
        assert_keys(
            &serde_json::to_value(&ok).unwrap(),
            &[
                "text_equal",
                "no_speech_probability_max_abs_diff",
                "segment_count_equal",
            ],
            "repeat keys",
        );
    }

    // ---- M0 --------------------------------------------------------------------------

    #[test]
    fn m0_keeps_normal_text() {
        let audio = loud(32_000);
        let trimmed = trim_trailing_silence(&audio);
        let m0 = segment_m0(&audio, trimmed, &[ph(" Hello there.", 0, 32_000)]);
        assert_eq!(
            m0,
            SegmentM0 {
                skipped_as_silent: false,
                text_before_cleanup: Some("Hello there.".into()),
                phantom_dropped_index: None,
                kept_phrase_indices: vec![0],
                keep_rejected: false,
            },
            "normal"
        );
    }

    #[test]
    fn m0_drops_a_trailing_phantom() {
        let audio = cat(&[&loud(32_000), &quiet(32_000)]);
        let trimmed = trim_trailing_silence(&audio);
        assert_eq!(trimmed.len(), 32_000 + TRAILING_PADDING, "trimmed");
        let m0 = segment_m0(
            &audio,
            trimmed,
            &[ph(" Hello.", 0, 32_000), ph(" Thank you.", 32_000, 40_000)],
        );
        assert_eq!(
            m0,
            SegmentM0 {
                skipped_as_silent: false,
                text_before_cleanup: Some("Hello.".into()),
                phantom_dropped_index: Some(1),
                kept_phrase_indices: vec![0],
                keep_rejected: false,
            },
            "phantom"
        );
    }

    #[test]
    fn m0_skips_a_silent_segment() {
        let audio = quiet(32_000);
        let m0 = segment_m0(&audio, &audio, &[ph(" Thank you.", 0, 32_000)]);
        assert_eq!(
            m0,
            SegmentM0 {
                skipped_as_silent: true,
                text_before_cleanup: None,
                phantom_dropped_index: None,
                kept_phrase_indices: vec![],
                keep_rejected: false,
            },
            "silent"
        );
    }

    #[test]
    fn m0_without_phrases_has_no_text() {
        let audio = loud(1_600);
        let m0 = segment_m0(&audio, &audio, &[]);
        assert_eq!(m0.text_before_cleanup, None, "no text");
        assert_eq!(m0.phantom_dropped_index, None, "no phantom");
        assert!(m0.kept_phrase_indices.is_empty(), "none kept");
        assert!(!m0.keep_rejected, "nothing to reject");
        assert!(!m0.skipped_as_silent, "loud");
    }

    #[test]
    fn m0_reports_keep_rejections() {
        let audio = loud(16_000);
        let thanks = segment_m0(&audio, &audio, &[ph(" Thank you.", 0, 16_000)]);
        assert_eq!(thanks.text_before_cleanup, None, "rejected");
        assert_eq!(thanks.kept_phrase_indices, [0], "kept: loud, not a phantom");
        assert!(thanks.keep_rejected, "short hallucination");
        let dot = segment_m0(&audio, &audio, &[ph(".", 0, 16_000)]);
        assert!(dot.keep_rejected, "no letter or digit");
        assert_eq!(dot.phantom_dropped_index, None, "not a phantom");
        // A lone phantom: dropped, nothing kept, so nothing rejected.
        let quiet_end = cat(&[&loud(16_000), &quiet(16_000)]);
        let alone = segment_m0(&quiet_end, &quiet_end, &[ph(" Thank you.", 16_000, 32_000)]);
        assert_eq!(alone.phantom_dropped_index, Some(0), "dropped");
        assert!(alone.kept_phrase_indices.is_empty(), "none kept");
        assert!(!alone.keep_rejected, "not rejected");
    }

    fn m0(text: Option<&str>, silent: bool, phantom: Option<usize>, rejected: bool) -> SegmentM0 {
        SegmentM0 {
            skipped_as_silent: silent,
            text_before_cleanup: text.map(String::from),
            phantom_dropped_index: phantom,
            kept_phrase_indices: vec![],
            keep_rejected: rejected,
        }
    }

    #[test]
    fn the_recording_m0_joins_its_segments() {
        let silent_then_text = recording_m0(&[
            m0(None, true, None, false),
            m0(Some("b"), false, None, false),
        ]);
        assert_eq!(
            silent_then_text.text_before_cleanup.as_deref(),
            Some("b"),
            "no separator"
        );
        assert!(
            !silent_then_text.skipped_as_silent,
            "not every segment silent"
        );
        let both = recording_m0(&[
            m0(Some("a"), false, None, false),
            m0(Some("b"), false, None, true),
        ]);
        assert_eq!(
            both.text_before_cleanup.as_deref(),
            Some("a\n\nb"),
            "joined"
        );
        assert_eq!(both.keep_rejected_segments, [1], "rejected");
        let phantom = recording_m0(&[
            m0(Some("a"), false, None, false),
            m0(Some("b"), false, Some(3), false),
        ]);
        assert_eq!(phantom.phantom_dropped.len(), 1, "one phantom");
        assert_eq!(phantom.phantom_dropped[0].segment, 1, "segment");
        assert_eq!(phantom.phantom_dropped[0].index, 3, "index");
        let silent = recording_m0(&[m0(None, true, None, false), m0(None, true, None, false)]);
        assert!(silent.skipped_as_silent, "all silent");
        assert_eq!(silent.text_before_cleanup, None, "no text");
    }

    // ---- features --------------------------------------------------------------------

    fn speech_then_silence() -> SegmentInput {
        let untrimmed = cat(&[&loud(32_000), &quiet(32_000)]);
        SegmentInput {
            file: Some("segment-0000.wav".into()),
            untrimmed,
            untrimmed_decode: decoded(
                vec![dp(" Hello.", 0, 32_000), dp(" Thank you.", 32_000, 64_000)],
                40,
            ),
            trimmed_decode: decoded(
                vec![dp(" Hello.", 0, 32_000), dp(" Thank you.", 32_000, 40_000)],
                30,
            ),
            repeat_decode: None,
        }
    }

    fn short_speech() -> SegmentInput {
        SegmentInput {
            file: Some("segment-0003.wav".into()),
            untrimmed: loud(16_000),
            untrimmed_decode: decoded(vec![dp(" Again.", 0, 16_000)], 5),
            trimmed_decode: decoded(vec![dp(" Again.", 1_600, 16_000)], 6),
            repeat_decode: None,
        }
    }

    #[test]
    fn a_segment_reports_both_decodes_and_its_m0() {
        let s = build_segment(4, &speech_then_silence()).unwrap();
        assert_eq!(s.index, 4, "index");
        assert_eq!(s.file.as_deref(), Some("segment-0000.wav"), "file");
        assert_eq!(s.samples_untrimmed, 64_000, "untrimmed");
        assert_eq!(s.samples_trimmed, 40_000, "trimmed");
        assert_eq!(bits(s.duration_untrimmed_s), bits(4.0), "untrimmed s");
        assert_eq!(bits(s.duration_trimmed_s), bits(2.5), "trimmed s");
        assert!(!s.is_silent, "loud");
        assert_eq!(s.rms_max.to_bits(), 0.5f32.to_bits(), "rms");
        assert_eq!(s.untrimmed.elapsed_ms, 40, "untrimmed elapsed");
        assert_eq!(s.trimmed.elapsed_ms, 30, "trimmed elapsed");
        let hello = &s.trimmed.phrases[0];
        assert_eq!(hello.index, 0, "index");
        assert_eq!(bits(hello.t0_s), bits(0.0), "t0");
        assert_eq!(bits(hello.t1_s), bits(2.0), "t1");
        assert_eq!(hello.text, " Hello.", "text");
        assert_eq!(
            hello.no_speech_probability.to_bits(),
            0.25f32.to_bits(),
            "nsp"
        );
        assert!(!hello.silent, "loud phrase");
        assert!(!hello.phantom, "not a phantom");
        assert_eq!(hello.rms_max.to_bits(), 0.5f32.to_bits(), "phrase rms");
        assert_eq!(hello.tokens.len(), 2, "tokens");
        let t = &hello.tokens[0];
        assert_eq!(t.text, "[_TT_0]", "token text");
        assert_eq!(t.id, 50_364, "token id");
        assert!(t.special, "special");
        assert_eq!(t.probability.to_bits(), 0.5f32.to_bits(), "p");
        assert_eq!(t.log_probability.to_bits(), (-0.75f32).to_bits(), "plog");
        assert!(!hello.tokens[1].special, "a text token");
        let thanks = &s.trimmed.phrases[1];
        assert_eq!(thanks.index, 1, "second index");
        assert!(thanks.silent, "silent phrase");
        assert!(thanks.phantom, "phantom");
        assert_eq!(thanks.rms_max.to_bits(), 0.0f32.to_bits(), "silent rms");
        assert!(s.untrimmed.phrases[1].phantom, "a phantom untrimmed too");
        assert_eq!(bits(s.untrimmed.phrases[1].t1_s), bits(4.0), "untrimmed t1");
        assert_eq!(s.m0.phantom_dropped_index, Some(1), "m0 phantom");
        assert_eq!(
            s.m0.text_before_cleanup.as_deref(),
            Some("Hello."),
            "m0 text"
        );
    }

    #[test]
    fn a_segment_m0_uses_the_trimmed_decode() {
        let mut input = speech_then_silence();
        input.trimmed_decode = decoded(vec![dp(" Trimmed.", 0, 40_000)], 1);
        let s = build_segment(0, &input).unwrap();
        assert_eq!(
            s.m0.text_before_cleanup.as_deref(),
            Some("Trimmed."),
            "trimmed phrases"
        );
        assert_eq!(s.m0.phantom_dropped_index, None, "no phantom");
    }

    #[test]
    fn phrases_past_the_audio_fail_the_segment() {
        let mut untrimmed_bad = speech_then_silence();
        untrimmed_bad.untrimmed_decode = decoded(vec![dp("x", 0, 64_000 + 16_001)], 0);
        let e = rec_err(build_segment(0, &untrimmed_bad));
        assert!(e.contains("ends past the audio"), "{e}");
        let mut ok_edge = speech_then_silence();
        ok_edge.untrimmed_decode = decoded(vec![dp("x", 0, 64_000 + 16_000)], 0);
        assert!(build_segment(0, &ok_edge).is_ok(), "1 s past is allowed");
        let mut trimmed_bad = speech_then_silence();
        trimmed_bad.trimmed_decode = decoded(vec![dp("x", 0, 40_000 + 16_001)], 0);
        let trimmed_e = rec_err(build_segment(0, &trimmed_bad));
        assert!(
            trimmed_e.contains("ends past the audio"),
            "trimmed: {trimmed_e}"
        );
        let mut inverted = speech_then_silence();
        inverted.trimmed_decode = decoded(vec![dp("x", 10, 5)], 0);
        assert!(
            rec_err(build_segment(0, &inverted)).contains("inverted"),
            "inverted"
        );
    }

    #[test]
    fn an_empty_segment_reports_empty_decodes() {
        let input = SegmentInput {
            file: None,
            untrimmed: vec![],
            untrimmed_decode: Decoded::default(),
            trimmed_decode: Decoded::default(),
            repeat_decode: None,
        };
        let s = build_segment(0, &input).unwrap();
        assert_eq!(s.samples_trimmed, 0, "empty");
        assert!(s.is_silent, "silent");
        assert!(s.m0.skipped_as_silent, "skipped");
        assert_eq!(s.untrimmed.elapsed_ms, 0, "not decoded");
        assert!(s.trimmed.phrases.is_empty(), "no phrases");
    }

    fn provenance_fixture() -> Provenance {
        Provenance {
            scriber_commit: "abc123".into(),
            scriber_dirty: Some(false),
        }
    }

    fn common_fixture(prompt: Option<String>, kind: InputKind) -> Common {
        Common {
            input: kind.label(),
            provenance: provenance_fixture(),
            whisper_rs_version: WHISPER_RS_VERSION,
            model_file: "ggml.bin".into(),
            model_sha256: "00".into(),
            audio_file: kind.audio_file(),
            audio_sha256: "11".into(),
            audio_segment_files: vec![SegmentFile {
                file: "segment-0000.wav".into(),
                sha256: "22".into(),
                samples: 64_000,
            }],
            audio_decode: AudioDecode::new(vec!["-f".into()], 80_000),
            decode: DecodeParams::new(prompt),
            prompt_matches_production: false,
        }
    }

    fn settings() -> VadSettings {
        VadSettings {
            name: "default".into(),
            threshold: 0.5,
            min_silence_duration_ms: 100,
            speech_pad_ms: 30,
            min_speech_duration_ms: 250,
        }
    }

    fn causality_fixture() -> Causality {
        causality(
            "r1",
            "default",
            &[span(0.5, 1.0)],
            &[(2.0, vec![span(0.5, 1.0)])],
            &[span(0.5, 1.0)],
        )
    }

    #[test]
    fn a_two_segment_recording_concatenates_its_segments() {
        let segments = vec![
            build_segment(0, &speech_then_silence()).unwrap(),
            build_segment(1, &short_speech()).unwrap(),
        ];
        let r = features_report(
            common_fixture(None, InputKind::SegmentWavs),
            segments,
            None,
            false,
        )
        .unwrap();
        assert_eq!(r.segment_count, 2, "count");
        assert_eq!(r.samples_untrimmed, 80_000, "untrimmed sum");
        assert_eq!(r.samples_trimmed, 40_000 + 16_000, "trimmed sum");
        assert_eq!(bits(r.duration_untrimmed_s), bits(5.0), "untrimmed s");
        assert_eq!(bits(r.duration_trimmed_s), bits(3.5), "trimmed s");
        assert!(!r.is_silent, "loud");
        assert_eq!(r.rms_max.to_bits(), 0.5f32.to_bits(), "rms");
        assert_eq!(r.untrimmed.elapsed_ms, 45, "untrimmed elapsed");
        assert_eq!(r.trimmed.elapsed_ms, 36, "trimmed elapsed");
        assert_eq!(r.untrimmed.phrases.len(), 3, "untrimmed phrases");
        let again_u = &r.untrimmed.phrases[2];
        assert_eq!(again_u.segment, 1, "segment");
        assert_eq!(again_u.phrase.index, 0, "index within its segment");
        assert_eq!(bits(again_u.phrase.t0_s), bits(4.0), "offset by 64000");
        assert_eq!(bits(again_u.phrase.t1_s), bits(5.0), "t1 offset");
        let again_t = &r.trimmed.phrases[2];
        assert_eq!(
            bits(again_t.phrase.t0_s),
            bits(2.5 + 0.1),
            "offset by 40000"
        );
        assert_eq!(r.trimmed.phrases[0].segment, 0, "first segment");
        assert_eq!(
            bits(r.trimmed.phrases[0].phrase.t0_s),
            bits(0.0),
            "no offset"
        );
        assert_eq!(
            r.m0.text_before_cleanup.as_deref(),
            Some("Hello.\n\nAgain."),
            "joined"
        );
        assert_eq!(r.m0.phantom_dropped.len(), 1, "one phantom");
        assert_eq!(r.thresholds, Thresholds::default(), "thresholds");
        assert_eq!(r.segments[1].index, 1, "segment kept");
    }

    fn lengths_input(n: usize) -> SegmentInput {
        SegmentInput {
            file: None,
            untrimmed: loud(n),
            untrimmed_decode: Decoded::default(),
            trimmed_decode: Decoded::default(),
            repeat_decode: None,
        }
    }

    fn report_of(inputs: &[SegmentInput], repeat: bool) -> Result<FeaturesReport, ExtractError> {
        let segments = inputs
            .iter()
            .enumerate()
            .map(|(k, i)| build_segment(k, i).unwrap())
            .collect();
        features_report(
            common_fixture(None, InputKind::SegmentWavs),
            segments,
            None,
            repeat,
        )
    }

    #[test]
    fn segments_know_where_they_sit_on_the_timeline() {
        let r = report_of(&[lengths_input(16_000), lengths_input(8_000)], false).unwrap();
        let offsets: Vec<f64> = r.segments.iter().map(|s| s.offset_s).collect();
        let lengths: Vec<f64> = r.segments.iter().map(|s| s.length_s).collect();
        assert_eq!(offsets, [0.0, 1.0], "offsets");
        assert_eq!(lengths, [1.0, 0.5], "lengths");
        assert_eq!(
            lengths,
            r.segments
                .iter()
                .map(|s| s.duration_untrimmed_s)
                .collect::<Vec<_>>(),
            "length is the untrimmed duration"
        );
        let mut two = [speech_then_silence(), short_speech()];
        two[1].untrimmed_decode = decoded(vec![dp(" x", 0, 16_000)], 1);
        let joined = report_of(&two, false).unwrap();
        let shift =
            joined.untrimmed.phrases[2].phrase.t0_s - joined.segments[1].untrimmed.phrases[0].t0_s;
        assert_eq!(
            bits(joined.segments[1].offset_s),
            bits(shift),
            "the concatenation shift"
        );
        let odd = report_of(&[lengths_input(16_001), lengths_input(7)], false).unwrap();
        assert_eq!(odd.segments[0].offset_s, 0.0, "first");
        assert_eq!(
            odd.segments[1].offset_s,
            samples_to_seconds(16_001).unwrap(),
            "non-dyadic offset"
        );
        assert_eq!(
            odd.segments[1].length_s,
            samples_to_seconds(7).unwrap(),
            "non-dyadic length"
        );
        let single = build_segment(0, &lengths_input(100)).unwrap();
        assert_eq!(single.offset_s, 0.0, "a lone segment starts at 0");
    }

    fn with_repeat(mut input: SegmentInput, second: Decoded) -> SegmentInput {
        input.repeat_decode = Some(second);
        input
    }

    #[test]
    fn repeat_decodes_are_reported_when_asked_for() {
        let base = short_speech();
        let same = with_repeat(base.clone(), base.untrimmed_decode.clone());
        let r = report_of(&[same.clone(), same.clone()], true).unwrap();
        assert_eq!(r.repeat_all_equal, Some(true), "all equal");
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["repeat_all_equal"], true, "key");
        assert_keys(
            &v["segments"][0]["repeat"],
            &[
                "text_equal",
                "no_speech_probability_max_abs_diff",
                "segment_count_equal",
            ],
            "repeat",
        );
        let off = report_of(std::slice::from_ref(&base), false).unwrap();
        assert_eq!(off.repeat_all_equal, None, "off");
        let off_v = serde_json::to_value(&off).unwrap();
        assert!(
            !off_v.as_object().unwrap().contains_key("repeat_all_equal"),
            "no key"
        );
        assert!(
            !off_v["segments"][0]
                .as_object()
                .unwrap()
                .contains_key("repeat"),
            "no segment key"
        );
        assert_eq!(
            report_of(&[], true).unwrap().repeat_all_equal,
            Some(true),
            "vacuous"
        );
        assert_eq!(
            report_of(&[], false).unwrap().repeat_all_equal,
            None,
            "off, empty"
        );

        let texted = decoded(vec![dp(" Other.", 0, 16_000)], 5);
        let counted = decoded(vec![dp(" Again.", 0, 8_000), dp("", 8_000, 16_000)], 5);
        let probable = decoded(vec![with_nsp(dp(" Again.", 0, 16_000), 0.5)], 5);
        for (second, what) in [
            (texted, "text"),
            (counted, "count"),
            (probable, "probability"),
        ] {
            let differs =
                report_of(&[same.clone(), with_repeat(base.clone(), second)], true).unwrap();
            assert_eq!(differs.repeat_all_equal, Some(false), "{what}");
        }
        let bad = with_repeat(base.clone(), decoded(vec![dp("x", 0, 40_000)], 0));
        assert!(
            rec_err(build_segment(0, &bad)).contains("past"),
            "repeat phrase checked"
        );

        let on_missing = report_of(std::slice::from_ref(&base), true);
        assert!(rec_err(on_missing).contains("missing"), "on but missing");
        let off_present = report_of(&[same], false);
        assert!(rec_err(off_present).contains("present"), "off but present");
    }

    #[test]
    fn a_silent_recording_is_silent() {
        let silent = SegmentInput {
            file: None,
            untrimmed: quiet(16_000),
            untrimmed_decode: Decoded::default(),
            trimmed_decode: Decoded::default(),
            repeat_decode: None,
        };
        let r = features_report(
            common_fixture(None, InputKind::Wav),
            vec![build_segment(0, &silent).unwrap()],
            None,
            false,
        )
        .unwrap();
        assert!(r.is_silent, "silent");
        assert!(r.m0.skipped_as_silent, "skipped");
        assert_eq!(r.rms_max.to_bits(), 0.0f32.to_bits(), "rms");
        let mixed = features_report(
            common_fixture(None, InputKind::SegmentWavs),
            vec![
                build_segment(0, &silent).unwrap(),
                build_segment(1, &short_speech()).unwrap(),
            ],
            None,
            false,
        )
        .unwrap();
        assert!(!mixed.is_silent, "one loud segment");
        assert_eq!(mixed.rms_max.to_bits(), 0.5f32.to_bits(), "the loudest");
        let reversed = features_report(
            common_fixture(None, InputKind::SegmentWavs),
            vec![
                build_segment(0, &short_speech()).unwrap(),
                build_segment(1, &silent).unwrap(),
            ],
            None,
            false,
        )
        .unwrap();
        assert_eq!(
            reversed.rms_max.to_bits(),
            0.5f32.to_bits(),
            "the loudest first"
        );
    }

    #[test]
    fn the_report_parts_carry_the_production_values() {
        let t = Thresholds::default();
        assert_eq!(t.silence_rms.to_bits(), SILENCE_RMS.to_bits(), "silence");
        assert_eq!(
            t.trailing_silence_rms.to_bits(),
            TRAILING_SILENCE_RMS.to_bits(),
            "trailing"
        );
        assert_eq!(t.trailing_padding, TRAILING_PADDING, "padding");
        let d = DecodeParams::new(Some("p".into()));
        assert_eq!(d.strategy, "beam_search", "strategy");
        assert_eq!(d.beam_size, 5, "beam");
        assert_eq!(d.patience.to_bits(), (-1.0f32).to_bits(), "patience");
        assert_eq!(d.language, "en", "language");
        assert!(d.suppress_nst, "nst");
        assert_eq!(d.prompt.as_deref(), Some("p"), "prompt");
        let m4a = AudioDecode::new(vec!["-f".into(), "WAVE".into()], 10);
        assert_eq!(m4a.tool.as_deref(), Some("/usr/bin/afconvert"), "tool");
        assert_eq!(m4a.args, ["-f", "WAVE"], "args");
        assert_eq!(m4a.samples, 10, "samples");
        assert_eq!(m4a.sample_rate, 16_000, "rate");
        let wav = AudioDecode::new(vec![], 3);
        assert_eq!(wav.tool, None, "no tool");
        assert!(wav.args.is_empty(), "no args");
        let run = VadRun::new(&settings(), 9, vec![span(0.0, 1.0)]);
        assert_eq!(run.name, "default", "name");
        assert_eq!(run.threshold.to_bits(), 0.5f32.to_bits(), "threshold");
        assert_eq!(run.min_silence_duration_ms, 100, "silence");
        assert_eq!(run.speech_pad_ms, 30, "pad");
        assert_eq!(run.min_speech_duration_ms, 250, "speech");
        assert_eq!(run.vad_elapsed_ms, 9, "elapsed");
        assert_eq!(run.spans, [span(0.0, 1.0)], "spans");
        assert_eq!(Span::from((1.5, 2.5)), span(1.5, 2.5), "span from a pair");
    }

    #[test]
    fn provenance_prefers_the_environment_then_git() {
        assert_eq!(
            provenance(Some("deadbeef"), Some("abc"), Some(" M x")),
            Ok(Provenance {
                scriber_commit: "deadbeef".into(),
                scriber_dirty: None
            }),
            "env"
        );
        assert_eq!(
            provenance(Some(""), Some("abc\n"), Some("")),
            Ok(Provenance {
                scriber_commit: "abc".into(),
                scriber_dirty: Some(false)
            }),
            "empty env, clean tree"
        );
        assert_eq!(
            provenance(None, Some("abc"), Some(" M src/x.rs\n")),
            Ok(Provenance {
                scriber_commit: "abc".into(),
                scriber_dirty: Some(true)
            }),
            "dirty tree"
        );
        assert_eq!(
            provenance(None, Some("abc"), None),
            Ok(Provenance {
                scriber_commit: "abc".into(),
                scriber_dirty: None
            }),
            "no status"
        );
        assert!(
            matches!(provenance(None, None, None), Err(ExtractError::Config(_))),
            "nothing"
        );
        assert!(
            matches!(
                provenance(Some(" "), Some("\n"), Some("")),
                Err(ExtractError::Config(_))
            ),
            "blank"
        );
    }

    // ---- shapes ----------------------------------------------------------------------

    const COMMON_KEYS: [&str; 13] = [
        "input",
        "scriber_commit",
        "scriber_dirty",
        "whisper_rs_version",
        "model_file",
        "model_sha256",
        "audio_file",
        "audio_sha256",
        "audio_segment_files",
        "audio_decode",
        "decode",
        "prompt_matches_production",
        "decisions",
    ];

    const PHRASE_KEYS: [&str; 9] = [
        "index",
        "t0_s",
        "t1_s",
        "text",
        "no_speech_probability",
        "silent",
        "phantom",
        "rms_max",
        "tokens",
    ];

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one report, every nesting level of its shape"
    )]
    fn the_features_report_has_the_pinned_shape() {
        let segments = vec![
            build_segment(0, &speech_then_silence()).unwrap(),
            build_segment(1, &short_speech()).unwrap(),
        ];
        let silero = SileroOut {
            model_file: "silero.bin".into(),
            model_sha256: "33".into(),
            runs: vec![VadRun::new(&settings(), 1, vec![span(0.5, 1.0)])],
            causality: Some(causality_fixture()),
        };
        let r = features_report(
            common_fixture(Some("prompt".into()), InputKind::SegmentWavs),
            segments,
            Some(silero),
            false,
        )
        .unwrap();
        let v = serde_json::to_value(&r).unwrap();
        let mut top: Vec<&str> = COMMON_KEYS[..12].to_vec();
        top.extend([
            "thresholds",
            "segment_count",
            "samples_untrimmed",
            "samples_trimmed",
            "duration_untrimmed_s",
            "duration_trimmed_s",
            "is_silent",
            "rms_max",
            "untrimmed",
            "trimmed",
            "m0",
            "silero",
            "segments",
        ]);
        assert_keys(&v, &top, "top");
        assert_eq!(v["input"], "segment-wavs", "input");
        assert_eq!(v["prompt_matches_production"], false, "prompt flag");
        assert_eq!(v["whisper_rs_version"], "0.16.0", "version");
        assert_keys(
            &v["audio_segment_files"][0],
            &["file", "sha256", "samples"],
            "segment file",
        );
        assert_keys(
            &v["audio_decode"],
            &["tool", "args", "samples", "sample_rate"],
            "audio_decode",
        );
        assert_keys(
            &v["decode"],
            &[
                "strategy",
                "beam_size",
                "patience",
                "language",
                "suppress_nst",
                "prompt",
            ],
            "decode",
        );
        assert_eq!(v["decode"]["prompt"], "prompt", "prompt");
        assert_eq!(
            v["decode"],
            json!({"strategy": "beam_search", "beam_size": 5, "patience": -1.0, "language": "en", "suppress_nst": true, "prompt": "prompt"}),
            "decode values"
        );
        assert_keys(
            &v["thresholds"],
            &["silence_rms", "trailing_silence_rms", "trailing_padding"],
            "thresholds",
        );
        assert_keys(&v["untrimmed"], &["phrases", "elapsed_ms"], "untrimmed");
        let mut rec_phrase = PHRASE_KEYS.to_vec();
        rec_phrase.push("segment");
        assert_keys(
            &v["untrimmed"]["phrases"][0],
            &rec_phrase,
            "recording phrase",
        );
        assert_keys(&v["trimmed"]["phrases"][2], &rec_phrase, "trimmed phrase");
        assert_keys(
            &v["untrimmed"]["phrases"][0]["tokens"][0],
            &["text", "id", "special", "probability", "log_probability"],
            "token",
        );
        assert_keys(
            &v["m0"],
            &[
                "skipped_as_silent",
                "text_before_cleanup",
                "phantom_dropped",
                "keep_rejected_segments",
            ],
            "m0",
        );
        assert_eq!(
            v["m0"]["text_before_cleanup"], "Hello.\n\nAgain.",
            "m0 text"
        );
        assert_keys(
            &v["m0"]["phantom_dropped"][0],
            &["segment", "index"],
            "phantom ref",
        );
        assert_keys(
            &v["silero"],
            &["model_file", "model_sha256", "runs", "causality"],
            "silero",
        );
        assert_keys(
            &v["silero"]["runs"][0],
            &[
                "name",
                "threshold",
                "min_silence_duration_ms",
                "speech_pad_ms",
                "min_speech_duration_ms",
                "vad_elapsed_ms",
                "spans",
            ],
            "run",
        );
        assert_keys(
            &v["silero"]["runs"][0]["spans"][0],
            &["start_s", "end_s"],
            "span",
        );
        let c = &v["silero"]["causality"];
        assert_keys(
            c,
            &[
                "recording",
                "setting",
                "prefix_step_s",
                "chunk_s",
                "tolerance_s",
                "prefix_stable",
                "prefix_mismatch_count",
                "chunks_match_offline",
                "offline_span_count",
                "chunk_spans_raw_count",
                "chunk_spans_merged_count",
                "max_abs_delta_s",
                "prefix_runs",
                "chunk_spans",
                "first_mismatch",
                "causal",
                "verdict",
            ],
            "causality",
        );
        assert_keys(
            &c["prefix_runs"][0],
            &["prefix_s", "spans", "mismatched_span_indices"],
            "prefix run",
        );
        assert!(c["first_mismatch"].is_null(), "no mismatch");
        assert_eq!(v["segments"].as_array().unwrap().len(), 2, "two segments");
        for (k, s) in v["segments"].as_array().unwrap().iter().enumerate() {
            assert_keys(
                s,
                &[
                    "index",
                    "file",
                    "samples_untrimmed",
                    "samples_trimmed",
                    "duration_untrimmed_s",
                    "duration_trimmed_s",
                    "offset_s",
                    "length_s",
                    "is_silent",
                    "rms_max",
                    "untrimmed",
                    "trimmed",
                    "m0",
                ],
                "segment",
            );
            assert_eq!(s["index"], k, "segment index");
            assert_keys(&s["trimmed"], &["phrases", "elapsed_ms"], "segment decode");
            assert_keys(&s["trimmed"]["phrases"][0], &PHRASE_KEYS, "segment phrase");
            assert_keys(
                &s["m0"],
                &[
                    "skipped_as_silent",
                    "text_before_cleanup",
                    "phantom_dropped_index",
                    "kept_phrase_indices",
                    "keep_rejected",
                ],
                "segment m0",
            );
        }
        assert_eq!(v["segments"][1]["file"], "segment-0003.wav", "file");
        assert_eq!(
            v["segments"][0]["m0"]["phantom_dropped_index"], 1,
            "phantom index"
        );
    }

    #[test]
    fn absent_features_values_are_null_not_missing() {
        let silent = SegmentInput {
            file: None,
            untrimmed: quiet(16_000),
            untrimmed_decode: Decoded::default(),
            trimmed_decode: Decoded::default(),
            repeat_decode: None,
        };
        let mut common = common_fixture(None, InputKind::Wav);
        common.provenance.scriber_dirty = None;
        let r = features_report(
            common,
            vec![build_segment(0, &silent).unwrap()],
            None,
            false,
        )
        .unwrap();
        let v = serde_json::to_value(&r).unwrap();
        assert!(v["silero"].is_null(), "silero");
        assert!(v["decode"]["prompt"].is_null(), "prompt");
        assert!(v["scriber_dirty"].is_null(), "dirty");
        assert!(v["m0"]["text_before_cleanup"].is_null(), "recording text");
        let s = &v["segments"][0];
        assert!(s["file"].is_null(), "file");
        assert!(s["m0"]["text_before_cleanup"].is_null(), "segment text");
        assert!(s["m0"]["phantom_dropped_index"].is_null(), "phantom index");
        assert!(
            v.as_object().unwrap().contains_key("silero"),
            "the key is present"
        );
        let no_tool = serde_json::to_value(AudioDecode::new(vec![], 0)).unwrap();
        assert!(no_tool["tool"].is_null(), "tool");
    }

    #[test]
    fn the_stage2_report_places_its_input_segments() {
        let report = |lengths: &[usize]| {
            stage2_report(
                common_fixture(None, InputKind::SegmentWavs),
                None,
                lengths,
                assemble_pieces(&[], &[], &[], None).unwrap(),
            )
            .unwrap()
        };
        assert_eq!(
            report(&[16_000, 8_000]).input_segments,
            [
                InputSegmentTiming {
                    index: 0,
                    offset_s: 0.0,
                    length_s: 1.0
                },
                InputSegmentTiming {
                    index: 1,
                    offset_s: 1.0,
                    length_s: 0.5
                },
            ],
            "dyadic"
        );
        let odd = report(&[16_001, 7]).input_segments;
        assert_eq!(
            odd[1].offset_s,
            samples_to_seconds(16_001).unwrap(),
            "offset"
        );
        assert_eq!(odd[1].length_s, samples_to_seconds(7).unwrap(), "length");
    }

    #[test]
    fn the_stage2_report_shows_repeats_only_when_on() {
        let plan = [piece(0..16_000), skipped_piece(16_000..24_000)];
        let timeline = quiet(24_000);
        let first = decoded(vec![dp(" a", 0, 8_000)], 3);
        let decodes = [first.clone(), Decoded::default()];
        let report = |repeats: Option<&[Option<Decoded>]>| {
            let out = assemble_pieces(&plan, &timeline, &decodes, repeats).unwrap();
            serde_json::to_value(
                stage2_report(
                    common_fixture(None, InputKind::SegmentWavs),
                    None,
                    &[16_000, 8_000],
                    out,
                )
                .unwrap(),
            )
            .unwrap()
        };
        let mut top = COMMON_KEYS.to_vec();
        top.extend([
            "input_segment_lengths_s",
            "input_segments",
            "skipped",
            "pieces",
            "text",
            "text_undropped",
        ]);
        let off = report(None);
        assert_keys(&off, &top, "off");
        assert!(
            !off["pieces"][0].as_object().unwrap().contains_key("repeat"),
            "off piece"
        );

        top.push("repeat_all_equal");
        let on = report(Some(&[Some(first.clone()), None]));
        assert_keys(&on, &top, "on");
        assert_eq!(on["repeat_all_equal"], true, "all equal");
        assert_keys(
            &on["pieces"][0]["repeat"],
            &[
                "text_equal",
                "no_speech_probability_max_abs_diff",
                "segment_count_equal",
            ],
            "piece repeat",
        );
        assert!(
            !on["pieces"][1].as_object().unwrap().contains_key("repeat"),
            "skipped piece"
        );
        let differs = report(Some(&[Some(decoded(vec![dp(" b", 0, 8_000)], 3)), None]));
        assert_eq!(differs["repeat_all_equal"], false, "one differs");
        assert_eq!(
            differs["pieces"][0]["repeat"]["text_equal"], false,
            "its repeat shows it"
        );

        let skipped = assemble_pieces(
            &[skipped_piece(0..10)],
            &quiet(10),
            &[Decoded::default()],
            Some(&[None]),
        )
        .unwrap();
        let v = serde_json::to_value(
            stage2_report(common_fixture(None, InputKind::M4a), None, &[10], skipped).unwrap(),
        )
        .unwrap();
        assert_eq!(v["repeat_all_equal"], true, "all skipped");
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one report, every nesting level of its shape"
    )]
    fn the_stage2_report_has_the_pinned_shape() {
        let d = Decisions {
            split_s: None,
            pieces: Some(vec![
                PieceDecision {
                    trim_end_s: Some(0.5),
                    ..keep()
                },
                PieceDecision {
                    skip: Some(true),
                    ..keep()
                },
            ]),
        };
        let plan = plan_pieces(&[16_000, 8_000], Some(&d)).unwrap();
        let out = assemble_pieces(
            &plan,
            &quiet(24_000),
            &[decoded(vec![dp(" a", 0, 8_000)], 3), Decoded::default()],
            None,
        )
        .unwrap();
        let r = stage2_report(
            common_fixture(None, InputKind::SegmentWavs),
            Some(d),
            &[16_000, 8_000],
            out,
        )
        .unwrap();
        let v = serde_json::to_value(&r).unwrap();
        let mut top = COMMON_KEYS.to_vec();
        top.extend([
            "input_segment_lengths_s",
            "input_segments",
            "skipped",
            "pieces",
            "text",
            "text_undropped",
        ]);
        assert_keys(&v, &top, "top");
        assert_eq!(v["input_segment_lengths_s"], json!([1.0, 0.5]), "lengths");
        assert_eq!(v["text"], "a", "text");
        assert_eq!(v["skipped"], false, "skipped");
        assert_keys(&v["decisions"], &["split_s", "pieces"], "decisions");
        assert!(v["decisions"]["split_s"].is_null(), "split null");
        assert_keys(
            &v["decisions"]["pieces"][0],
            &["skip", "trim_end_s", "drop"],
            "piece decision",
        );
        let pieces_v = v["pieces"].as_array().unwrap();
        for p in pieces_v {
            assert_keys(
                p,
                &[
                    "index",
                    "start_s",
                    "end_s",
                    "skipped",
                    "trim_end_s",
                    "elapsed_ms",
                    "phrases",
                    "text",
                    "text_undropped",
                ],
                "piece",
            );
        }
        let mut stage2_phrase = PHRASE_KEYS.to_vec();
        stage2_phrase.push("dropped");
        assert_keys(&v["pieces"][0]["phrases"][0], &stage2_phrase, "phrase");
        assert_keys(
            &v["pieces"][0]["phrases"][0]["tokens"][0],
            &["text", "id", "special", "probability", "log_probability"],
            "stage-2 token",
        );
        assert_eq!(v["pieces"][0]["trim_end_s"], 0.5, "trim");
        assert!(v["pieces"][1]["trim_end_s"].is_null(), "no trim");
        assert!(v["pieces"][1]["text"].is_null(), "skipped piece text");

        let all_skipped = assemble_pieces(
            &[PlannedPiece {
                skip: true,
                decode_len: 0,
                ..piece(0..10)
            }],
            &quiet(10),
            &[Decoded::default()],
            None,
        )
        .unwrap();
        let none = stage2_report(
            common_fixture(None, InputKind::M4a),
            None,
            &[10],
            all_skipped,
        )
        .unwrap();
        let skipped = serde_json::to_value(&none).unwrap();
        assert!(skipped["text"].is_null(), "text when skipped");
        assert!(skipped["decisions"].is_null(), "no decisions");
        assert_eq!(skipped["skipped"], true, "skipped");
        let too_long = usize::try_from(u64::from(u32::MAX) + 1).unwrap();
        let _e = rec_err(stage2_report(
            common_fixture(None, InputKind::M4a),
            None,
            &[too_long],
            Stage2Output {
                skipped: false,
                pieces: vec![],
                text: None,
                text_undropped: None,
                repeat_all_equal: None,
            },
        ));
    }

    // ---- causality -------------------------------------------------------------------

    #[test]
    fn prefixes_grow_by_two_seconds_to_the_whole() {
        assert_eq!(prefix_lengths(0), [0], "empty");
        assert_eq!(prefix_lengths(16_000), [16_000], "short");
        assert_eq!(prefix_lengths(32_000), [32_000], "exactly one step");
        assert_eq!(prefix_lengths(32_001), [32_000, 32_001], "just over");
        assert_eq!(prefix_lengths(64_000), [32_000, 64_000], "two steps");
        assert_eq!(
            prefix_lengths(70_000),
            [32_000, 64_000, 70_000],
            "a partial last step"
        );
    }

    #[test]
    fn chunks_are_consecutive_seconds() {
        assert!(chunk_ranges(0).is_empty(), "empty");
        assert_eq!(
            chunk_ranges(16_000),
            std::iter::once(0..16_000).collect::<Vec<_>>(),
            "one"
        );
        assert_eq!(
            chunk_ranges(40_000),
            [0..16_000, 16_000..32_000, 32_000..40_000],
            "a partial last chunk"
        );
        assert_eq!(
            offset_spans(&[span(0.25, 0.5)], 2.0),
            [span(2.25, 2.5)],
            "offset"
        );
    }

    #[test]
    fn a_stable_run_is_causal() {
        let c = causality(
            "r1",
            "default",
            &[span(2.5, 3.0), span(0.5, 1.0)],
            &[
                (2.0, vec![span(0.7, 1.0)]),
                (4.0, vec![span(0.5, 1.0), span(2.5, 3.0)]),
            ],
            &[span(0.5, 1.0), span(2.5, 3.0)],
        );
        assert!(
            c.prefix_stable,
            "stable: a span ending exactly at L - 1 is unsettled"
        );
        assert!(c.causal, "causal");
        assert_eq!(c.prefix_mismatch_count, 0, "no mismatch");
        assert!(c.chunks_match_offline, "chunks match");
        assert_eq!(c.first_mismatch, None, "none");
        assert_eq!(bits(c.max_abs_delta_s), bits(0.0), "no delta");
        assert_eq!(c.offline_span_count, 2, "offline");
        assert_eq!(c.chunk_spans_raw_count, 2, "raw");
        assert_eq!(c.chunk_spans_merged_count, 2, "merged");
        assert_eq!(c.recording, "r1", "recording");
        assert_eq!(c.setting, "default", "setting");
        assert_eq!(bits(c.prefix_step_s), bits(2.0), "step");
        assert_eq!(bits(c.chunk_s), bits(1.0), "chunk");
        assert_eq!(bits(c.tolerance_s), bits(0.05), "tolerance");
        assert_eq!(c.prefix_runs.len(), 2, "runs");
        assert_eq!(bits(c.prefix_runs[0].prefix_s), bits(2.0), "prefix");
        assert_eq!(c.prefix_runs[0].spans, [span(0.7, 1.0)], "all spans kept");
        assert!(
            c.prefix_runs[1].mismatched_span_indices.is_empty(),
            "matched"
        );
        assert_eq!(
            c.verdict,
            "WhisperVadContext can be run causally on streaming chunks (prefix_stable=true, chunks_match_offline=true, max_abs_delta_s=0.000)",
            "verdict"
        );
    }

    #[test]
    fn an_unstable_prefix_is_not_causal() {
        let c = causality(
            "r1",
            "s",
            &[span(0.5, 1.0), span(2.0, 2.5)],
            &[
                (4.0, vec![span(0.5, 1.0)]),
                (6.0, vec![span(0.5, 1.0), span(2.0, 2.5)]),
            ],
            &[span(0.5, 1.0), span(2.0, 2.5)],
        );
        assert!(!c.prefix_stable, "unstable");
        assert!(!c.causal, "not causal");
        assert_eq!(c.prefix_mismatch_count, 1, "one bad run");
        assert_eq!(
            c.prefix_runs[0].mismatched_span_indices,
            [1],
            "unpaired index"
        );
        assert!(c.chunks_match_offline, "chunks still match");
        let m = c.first_mismatch.unwrap();
        assert_eq!(m.kind, "prefix", "kind");
        assert_eq!(bits(m.at_s), bits(4.0), "at");
        assert_eq!(
            m.offline,
            [span(0.5, 1.0), span(2.0, 2.5)],
            "offline settled"
        );
        assert_eq!(m.observed, [span(0.5, 1.0)], "observed settled");
        assert!(
            c.verdict.starts_with("WhisperVadContext cannot be run"),
            "{}",
            c.verdict
        );
        assert!(c.verdict.contains("prefix_stable=false"), "{}", c.verdict);
    }

    #[test]
    fn boundaries_match_within_the_tolerance() {
        let exact = causality(
            "r",
            "s",
            &[span(0.0, 1.0)],
            &[(3.0, vec![span(0.05, 1.0)])],
            &[span(0.0, 1.0)],
        );
        assert!(exact.prefix_stable, "exactly 0.05 off matches");
        assert_eq!(bits(exact.max_abs_delta_s), bits(0.05), "delta");
        let over = causality(
            "r",
            "s",
            &[span(0.5, 1.0)],
            &[(3.0, vec![span(0.5, 1.06)])],
            &[span(0.5, 1.0)],
        );
        assert!(!over.prefix_stable, "0.06 off does not");
        assert_eq!(over.prefix_runs[0].mismatched_span_indices, [0], "index");
        assert!(
            (over.max_abs_delta_s - 0.06).abs() < 1e-9,
            "delta {}",
            over.max_abs_delta_s
        );
        assert_eq!(
            over.verdict,
            "WhisperVadContext cannot be run causally on streaming chunks (prefix_stable=false, chunks_match_offline=true, max_abs_delta_s=0.060)",
            "verdict"
        );
        let starts = causality(
            "r",
            "s",
            &[span(0.5, 1.0)],
            &[(3.0, vec![span(0.6, 1.0)])],
            &[span(0.5, 1.0)],
        );
        assert!(!starts.prefix_stable, "start off by 0.1");
    }

    #[test]
    fn chunk_spans_merge_across_chunk_boundaries() {
        let c = causality(
            "r",
            "s",
            &[span(0.5, 2.5)],
            &[],
            &[span(0.5, 1.0), span(1.0, 2.0), span(2.0, 2.5)],
        );
        assert!(c.chunks_match_offline, "merged into one");
        assert_eq!(c.chunk_spans_raw_count, 3, "raw");
        assert_eq!(c.chunk_spans_merged_count, 1, "merged");
        assert_eq!(c.chunk_spans.len(), 3, "the raw spans are reported");
        let gap = causality(
            "r",
            "s",
            &[span(0.0, 1.0)],
            &[],
            &[span(0.0, 0.0), span(0.05, 1.0)],
        );
        assert!(gap.chunks_match_offline, "a gap of exactly 0.05 closes");
        let apart = causality(
            "r",
            "s",
            &[span(0.0, 1.0)],
            &[],
            &[span(1.5, 2.0), span(0.0, 0.5)],
        );
        assert!(!apart.chunks_match_offline, "a real gap stays");
        assert_eq!(apart.chunk_spans_merged_count, 2, "two");
        let m = apart.first_mismatch.unwrap();
        assert_eq!(m.kind, "chunk", "kind");
        assert_eq!(
            bits(m.at_s),
            bits(0.0),
            "at the first differing offline span"
        );
        assert_eq!(
            m.observed,
            [span(0.0, 0.5), span(1.5, 2.0)],
            "sorted, merged"
        );
        assert_eq!(m.offline, [span(0.0, 1.0)], "offline");
    }

    #[test]
    fn count_mismatches_and_nothing_paired() {
        let extra = causality(
            "r",
            "s",
            &[span(0.5, 1.0)],
            &[],
            &[span(0.5, 1.0), span(3.0, 3.5)],
        );
        assert!(!extra.chunks_match_offline, "an extra chunk span");
        let m = extra.first_mismatch.unwrap();
        assert_eq!(bits(m.at_s), bits(3.0), "at the unpaired chunk span");
        let none = causality("r", "s", &[], &[], &[]);
        assert!(
            none.prefix_stable && none.chunks_match_offline,
            "nothing to compare"
        );
        assert_eq!(bits(none.max_abs_delta_s), bits(0.0), "zero paired");
        let missing = causality("r", "s", &[span(0.5, 1.0)], &[(4.0, vec![])], &[]);
        assert_eq!(bits(missing.max_abs_delta_s), bits(0.0), "zero paired");
        assert!(!missing.chunks_match_offline, "no chunk spans");
        assert_eq!(missing.prefix_mismatch_count, 1, "prefix");
        assert_eq!(
            missing.first_mismatch.unwrap().kind,
            "prefix",
            "prefix first"
        );
    }

    #[test]
    fn the_largest_delta_wins_wherever_it_is() {
        let prefix_bigger = causality(
            "r",
            "s",
            &[span(0.0, 1.0)],
            &[(3.0, vec![span(0.0, 1.04)])],
            &[span(0.0, 1.02)],
        );
        assert!(
            (prefix_bigger.max_abs_delta_s - 0.04).abs() < 1e-9,
            "prefix {}",
            prefix_bigger.max_abs_delta_s
        );
        let chunk_bigger = causality(
            "r",
            "s",
            &[span(0.0, 1.0)],
            &[(3.0, vec![span(0.0, 1.02)])],
            &[span(0.0, 1.04)],
        );
        assert!(
            (chunk_bigger.max_abs_delta_s - 0.04).abs() < 1e-9,
            "chunk {}",
            chunk_bigger.max_abs_delta_s
        );
        let runs = causality(
            "r",
            "s",
            &[span(0.0, 1.0)],
            &[(3.0, vec![span(0.0, 1.04)]), (5.0, vec![span(0.0, 1.02)])],
            &[span(0.0, 1.0)],
        );
        assert!(
            (runs.max_abs_delta_s - 0.04).abs() < 1e-9,
            "runs {}",
            runs.max_abs_delta_s
        );
        let starts = causality(
            "r",
            "s",
            &[span(0.0, 1.0)],
            &[(3.0, vec![span(0.03, 1.01)])],
            &[span(0.0, 1.0)],
        );
        assert!(
            (starts.max_abs_delta_s - 0.03).abs() < 1e-9,
            "start delta {}",
            starts.max_abs_delta_s
        );
    }

    // ---- manifest --------------------------------------------------------------------

    fn status(name: &str, status: Status, error: Option<&str>) -> RecordingStatus {
        RecordingStatus {
            name: name.into(),
            status,
            error: error.map(String::from),
            outputs: vec![format!("{name}.json")],
        }
    }

    #[test]
    fn the_manifest_counts_and_decides_the_exit_code() {
        let config: Config = parse_config(&stage2_json("")).unwrap();
        let m = Manifest::new(
            config.clone(),
            provenance_fixture(),
            ("ggml.bin".into(), "00".into()),
            ("2026-11-02T00:00:00Z".into(), "2026-11-02T00:01:00Z".into()),
            vec![
                status("r1", Status::Ok, None),
                status("r2", Status::Error, Some("boom")),
                status("r3", Status::Skipped, None),
            ],
        );
        assert_eq!(m.ok_count, 1, "ok");
        assert_eq!(m.error_count, 1, "error");
        assert_eq!(m.exit_code(), 2, "a failure");
        assert_eq!(m.mode, Mode::Stage2, "mode");
        let v = serde_json::to_value(&m).unwrap();
        assert_keys(
            &v,
            &[
                "mode",
                "scriber_commit",
                "scriber_dirty",
                "whisper_rs_version",
                "model_file",
                "model_sha256",
                "config",
                "started_at",
                "finished_at",
                "recordings",
                "ok_count",
                "error_count",
            ],
            "manifest",
        );
        assert_eq!(v["mode"], "stage2", "mode");
        assert_eq!(v["model_file"], "ggml.bin", "model");
        assert_eq!(v["model_sha256"], "00", "sha");
        assert_eq!(v["started_at"], "2026-11-02T00:00:00Z", "start");
        assert_eq!(v["finished_at"], "2026-11-02T00:01:00Z", "finish");
        assert_eq!(v["whisper_rs_version"], "0.16.0", "version");
        assert_eq!(v["config"]["input"], "auto", "config echoed");
        assert!(v["config"]["silero"].is_null(), "config nulls");
        assert_eq!(v["config"]["repeat_decode"], false, "repeat off");
        let on = parse_config(&stage2_json(r#", "repeat_decode": true"#)).unwrap();
        let on = Manifest::new(
            on,
            provenance_fixture(),
            ("ggml.bin".into(), "00".into()),
            ("a".into(), "b".into()),
            vec![],
        );
        assert_eq!(
            serde_json::to_value(&on).unwrap()["config"]["repeat_decode"],
            true,
            "repeat on"
        );
        assert_keys(
            &v["recordings"][0],
            &["name", "status", "error", "outputs"],
            "recording",
        );
        assert_eq!(v["recordings"][0]["status"], "ok", "ok");
        assert!(v["recordings"][0]["error"].is_null(), "no error");
        assert_eq!(v["recordings"][1]["status"], "error", "error");
        assert_eq!(v["recordings"][1]["error"], "boom", "message");
        assert_eq!(v["recordings"][2]["status"], "skipped", "skipped");
        let clean = Manifest::new(
            config,
            provenance_fixture(),
            ("m".into(), "s".into()),
            (String::new(), String::new()),
            vec![
                status("r1", Status::Ok, None),
                status("r2", Status::Skipped, None),
            ],
        );
        assert_eq!(clean.exit_code(), 0, "all ok");
        assert_eq!(clean.ok_count, 1, "skipped is not ok");
        assert_eq!(clean.error_count, 0, "no errors");
        let features = parse_config(&features_json("")).unwrap();
        let f = Manifest::new(
            features,
            provenance_fixture(),
            ("m".into(), "s".into()),
            (String::new(), String::new()),
            vec![],
        );
        assert_eq!(
            serde_json::to_value(&f).unwrap()["mode"],
            "features",
            "features"
        );
    }
}
