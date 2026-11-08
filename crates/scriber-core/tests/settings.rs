//! The user's settings in `settings.json`.

#[expect(dead_code, reason = "these tests need only the fake data folder")]
mod common;

#[cfg(test)]
mod tests {
    use scriber_core::menubar::{DEFAULT_PAUSE_SHORTCUT, DEFAULT_SHORTCUT};
    use scriber_core::model::Settings;
    use scriber_core::ports::Files as _;
    use scriber_core::settings::{
        LIBRARY_WIDTH_DEFAULT, LIBRARY_WIDTH_MAX, LIBRARY_WIDTH_MIN, SETTINGS,
        SILENCE_SPLIT_MAX_MS, SILENCE_SPLIT_MIN_MS, load, save,
    };
    use serde_json::{Value, json};

    use crate::common::FakeFiles;

    /// The defaults: a `const` cannot hold a non-empty `String`, so this is a
    /// function.
    fn defaults() -> Settings {
        Settings {
            summaries: true,
            library_open: true,
            vocabulary: String::new(),
            cleanup: false,
            summary_model: String::new(),
            cleanup_model: String::new(),
            library_width: 220,
            input_device: String::new(),
            silence_split_ms: 0,
            keep_segments: false,
            learned_vocabulary: String::new(),
            model_folder: String::new(),
            toggle_shortcut: DEFAULT_SHORTCUT.to_string(),
            pause_shortcut: DEFAULT_PAUSE_SHORTCUT.to_string(),
        }
    }

    fn with(bytes: &[u8]) -> FakeFiles {
        let files = FakeFiles::default();
        files.write(SETTINGS, bytes).unwrap();
        files
    }

    #[test]
    fn the_path_is_pinned() {
        assert_eq!(SETTINGS, "settings.json", "path");
    }

    #[test]
    fn no_file_means_the_defaults() {
        assert_eq!(load(&FakeFiles::default()), defaults(), "missing");
        let files = with(br#"{"summaries":false,"library_open":false}"#);
        files.fail_read.set(true);
        assert_eq!(load(&files), defaults(), "unreadable");
    }

    #[test]
    fn an_invalid_file_means_the_defaults() {
        for bytes in [&b"not json"[..], b"[false]", b"false", b"", &[0xff, 0xfe]] {
            assert_eq!(load(&with(bytes)), defaults(), "{bytes:?}");
        }
    }

    #[test]
    fn each_key_is_read_on_its_own() {
        for (bytes, want) in [
            (
                &br#"{"summaries":false}"#[..],
                Settings {
                    summaries: false,
                    library_open: true,
                    vocabulary: String::new(),
                    cleanup: false,
                    summary_model: String::new(),
                    cleanup_model: String::new(),
                    library_width: 220,
                    input_device: String::new(),
                    silence_split_ms: 0,
                    keep_segments: false,
                    learned_vocabulary: String::new(),
                    model_folder: String::new(),
                    toggle_shortcut: DEFAULT_SHORTCUT.to_string(),
                    pause_shortcut: DEFAULT_PAUSE_SHORTCUT.to_string(),
                },
            ),
            (
                br#"{"library_open":false}"#,
                Settings {
                    summaries: true,
                    library_open: false,
                    vocabulary: String::new(),
                    cleanup: false,
                    summary_model: String::new(),
                    cleanup_model: String::new(),
                    library_width: 220,
                    input_device: String::new(),
                    silence_split_ms: 0,
                    keep_segments: false,
                    learned_vocabulary: String::new(),
                    model_folder: String::new(),
                    toggle_shortcut: DEFAULT_SHORTCUT.to_string(),
                    pause_shortcut: DEFAULT_PAUSE_SHORTCUT.to_string(),
                },
            ),
            (
                br#"{"summaries":"no","library_open":false}"#,
                Settings {
                    summaries: true,
                    library_open: false,
                    vocabulary: String::new(),
                    cleanup: false,
                    summary_model: String::new(),
                    cleanup_model: String::new(),
                    library_width: 220,
                    input_device: String::new(),
                    silence_split_ms: 0,
                    keep_segments: false,
                    learned_vocabulary: String::new(),
                    model_folder: String::new(),
                    toggle_shortcut: DEFAULT_SHORTCUT.to_string(),
                    pause_shortcut: DEFAULT_PAUSE_SHORTCUT.to_string(),
                },
            ),
            (
                br#"{"summaries":false,"library_open":0,"extra":1}"#,
                Settings {
                    summaries: false,
                    library_open: true,
                    vocabulary: String::new(),
                    cleanup: false,
                    summary_model: String::new(),
                    cleanup_model: String::new(),
                    library_width: 220,
                    input_device: String::new(),
                    silence_split_ms: 0,
                    keep_segments: false,
                    learned_vocabulary: String::new(),
                    model_folder: String::new(),
                    toggle_shortcut: DEFAULT_SHORTCUT.to_string(),
                    pause_shortcut: DEFAULT_PAUSE_SHORTCUT.to_string(),
                },
            ),
            (br#"{"libraryOpen":false}"#, defaults()),
        ] {
            assert_eq!(
                load(&with(bytes)),
                want,
                "{}",
                String::from_utf8_lossy(bytes)
            );
        }
    }

    #[test]
    fn save_then_load_round_trips() {
        let files = FakeFiles::default();
        let off = Settings {
            summaries: false,
            library_open: false,
            vocabulary: String::new(),
            cleanup: false,
            summary_model: String::new(),
            cleanup_model: String::new(),
            library_width: 220,
            input_device: String::new(),
            silence_split_ms: 0,
            keep_segments: false,
            learned_vocabulary: String::new(),
            model_folder: String::new(),
            toggle_shortcut: DEFAULT_SHORTCUT.to_string(),
            pause_shortcut: DEFAULT_PAUSE_SHORTCUT.to_string(),
        };
        save(&files, &off).unwrap();
        assert_eq!(load(&files), off, "read back");
        let written: Value = serde_json::from_slice(&files.read(SETTINGS).unwrap()).unwrap();
        assert_eq!(
            written,
            json!({
                "summaries": false,
                "library_open": false,
                "vocabulary": "",
                "cleanup": false,
                "summary_model": "",
                "cleanup_model": "",
                "library_width": 220,
                "input_device": "",
                "silence_split_ms": 0,
                "keep_segments": false,
                "learned_vocabulary": "",
                "model_folder": "",
                "toggle_shortcut": "Opt+Cmd+D",
                "pause_shortcut": "Opt+Cmd+P"
            }),
            "snake_case on disk"
        );
        assert_eq!(
            files.read(SETTINGS).unwrap(),
            br#"{"summaries":false,"library_open":false,"vocabulary":"","cleanup":false,"summary_model":"","cleanup_model":"","library_width":220,"input_device":"","silence_split_ms":0,"keep_segments":false,"learned_vocabulary":"","model_folder":"","toggle_shortcut":"Opt+Cmd+D","pause_shortcut":"Opt+Cmd+P"}"#,
            "compact"
        );
        assert!(
            files.read("settings.json.partial").is_err(),
            "no partial file left"
        );
        save(&files, &defaults()).unwrap();
        assert_eq!(load(&files), defaults(), "overwritten");
    }

    #[test]
    fn only_an_absolute_vocabulary_path_is_kept() {
        assert_eq!(
            load(&with(br#"{"vocabulary":"/x/terms.txt"}"#)).vocabulary,
            "/x/terms.txt",
            "absolute"
        );
        for bytes in [
            &b"{}"[..],
            br#"{"vocabulary":42}"#,
            br#"{"vocabulary":""}"#,
            br#"{"vocabulary":"terms.txt"}"#,
        ] {
            assert_eq!(
                load(&with(bytes)).vocabulary,
                "",
                "{}",
                String::from_utf8_lossy(bytes)
            );
        }
    }

    #[test]
    fn only_an_absolute_learned_vocabulary_path_is_kept() {
        assert_eq!(
            load(&with(br#"{"learned_vocabulary":"/x/learned.txt"}"#)).learned_vocabulary,
            "/x/learned.txt",
            "absolute"
        );
        for bytes in [
            &b"{}"[..],
            br#"{"learned_vocabulary":42}"#,
            br#"{"learned_vocabulary":""}"#,
            br#"{"learned_vocabulary":"learned.txt"}"#,
        ] {
            assert_eq!(
                load(&with(bytes)).learned_vocabulary,
                "",
                "{}",
                String::from_utf8_lossy(bytes)
            );
        }
    }

    #[test]
    fn the_learned_vocabulary_is_saved_last() {
        let files = FakeFiles::default();
        let s = Settings {
            learned_vocabulary: "/x/learned.txt".to_string(),
            ..defaults()
        };
        save(&files, &s).unwrap();
        assert_eq!(
            files.read(SETTINGS).unwrap(),
            br#"{"summaries":true,"library_open":true,"vocabulary":"","cleanup":false,"summary_model":"","cleanup_model":"","library_width":220,"input_device":"","silence_split_ms":0,"keep_segments":false,"learned_vocabulary":"/x/learned.txt","model_folder":"","toggle_shortcut":"Opt+Cmd+D","pause_shortcut":"Opt+Cmd+P"}"#,
            "bytes"
        );
        assert_eq!(load(&files), s, "read back");
    }

    #[test]
    fn only_an_absolute_model_folder_is_kept() {
        assert_eq!(
            load(&with(br#"{"model_folder":"/x/models"}"#)).model_folder,
            "/x/models",
            "absolute"
        );
        for bytes in [
            &b"{}"[..],
            br#"{"model_folder":42}"#,
            br#"{"model_folder":""}"#,
            br#"{"model_folder":"models"}"#,
        ] {
            assert_eq!(
                load(&with(bytes)).model_folder,
                "",
                "{}",
                String::from_utf8_lossy(bytes)
            );
        }
    }

    #[test]
    fn the_model_folder_is_saved_last() {
        let files = FakeFiles::default();
        let s = Settings {
            model_folder: "/x/models".to_string(),
            ..defaults()
        };
        save(&files, &s).unwrap();
        assert_eq!(
            files.read(SETTINGS).unwrap(),
            br#"{"summaries":true,"library_open":true,"vocabulary":"","cleanup":false,"summary_model":"","cleanup_model":"","library_width":220,"input_device":"","silence_split_ms":0,"keep_segments":false,"learned_vocabulary":"","model_folder":"/x/models","toggle_shortcut":"Opt+Cmd+D","pause_shortcut":"Opt+Cmd+P"}"#,
            "bytes"
        );
        assert_eq!(load(&files), s, "read back");
    }

    #[test]
    fn the_toggle_shortcut_defaults_to_opt_cmd_d() {
        assert_eq!(
            load(&FakeFiles::default()).toggle_shortcut,
            "Opt+Cmd+D",
            "no file"
        );
        assert_eq!(
            load(&with(b"{}")).toggle_shortcut,
            "Opt+Cmd+D",
            "a file without the key"
        );
        assert_eq!(DEFAULT_SHORTCUT, "Opt+Cmd+D", "the pinned default");
        for bytes in [
            &br#"{"toggle_shortcut":"nonsense"}"#[..],
            br#"{"toggle_shortcut":""}"#,
            br#"{"toggle_shortcut":"D"}"#,
            br#"{"toggle_shortcut":42}"#,
            br#"{"toggle_shortcut":[]}"#,
            br#"{"toggle_shortcut":null}"#,
        ] {
            assert_eq!(
                load(&with(bytes)).toggle_shortcut,
                "Opt+Cmd+D",
                "{}",
                String::from_utf8_lossy(bytes)
            );
        }
    }

    #[test]
    fn a_valid_toggle_shortcut_is_kept_verbatim() {
        for value in ["Opt+Cmd+K", "Opt+Shift+Cmd+K", "Ctrl+Opt+Shift+Cmd+F13"] {
            let bytes = format!(r#"{{"toggle_shortcut":"{value}"}}"#);
            assert_eq!(
                load(&with(bytes.as_bytes())).toggle_shortcut,
                value,
                "{value}"
            );
        }
    }

    #[test]
    fn the_pause_shortcut_follows_its_read_rule() {
        for bytes in [
            &b"{}"[..],
            br#"{"pause_shortcut":"nonsense"}"#,
            br#"{"pause_shortcut":""}"#,
            br#"{"pause_shortcut":"P"}"#,
            br#"{"pause_shortcut":"Ctrl+P"}"#,
            br#"{"pause_shortcut":42}"#,
            br#"{"pause_shortcut":[]}"#,
            br#"{"pause_shortcut":null}"#,
            br#"{"pause_shortcut":"Opt+Cmd+D"}"#,
        ] {
            assert_eq!(
                load(&with(bytes)).pause_shortcut,
                "Opt+Cmd+P",
                "{}",
                String::from_utf8_lossy(bytes)
            );
        }
        assert_eq!(
            load(&with(br#"{"pause_shortcut":"Opt+Shift+Cmd+P"}"#)).pause_shortcut,
            "Opt+Shift+Cmd+P",
            "a valid value is kept verbatim"
        );
        for bytes in [
            &br#"{"toggle_shortcut":"Opt+Cmd+P"}"#[..],
            br#"{"toggle_shortcut":"Opt+Cmd+P","pause_shortcut":"Opt+Cmd+P"}"#,
            br#"{"toggle_shortcut":"Opt+Cmd+P","pause_shortcut":"Cmd+Opt+P"}"#,
        ] {
            assert_eq!(
                load(&with(bytes)).pause_shortcut,
                "",
                "{}",
                String::from_utf8_lossy(bytes)
            );
        }
    }

    #[test]
    fn the_pause_shortcut_round_trips_after_the_toggle() {
        let files = FakeFiles::default();
        let s = Settings {
            pause_shortcut: "Opt+Shift+Cmd+P".to_string(),
            ..defaults()
        };
        save(&files, &s).unwrap();
        assert!(
            files
                .read(SETTINGS)
                .unwrap()
                .ends_with(br#","pause_shortcut":"Opt+Shift+Cmd+P"}"#),
            "the last key"
        );
        assert_eq!(load(&files), s, "read back");
    }

    #[test]
    fn the_toggle_shortcut_round_trips_before_the_pause() {
        let files = FakeFiles::default();
        let s = Settings {
            toggle_shortcut: "Opt+Shift+Cmd+K".to_string(),
            ..defaults()
        };
        save(&files, &s).unwrap();
        assert!(
            files.read(SETTINGS).unwrap().ends_with(
                br#"","toggle_shortcut":"Opt+Shift+Cmd+K","pause_shortcut":"Opt+Cmd+P"}"#
            ),
            "the last key"
        );
        assert_eq!(load(&files), s, "read back");
    }

    #[test]
    fn the_vocabulary_path_is_saved_third() {
        let files = FakeFiles::default();
        let s = Settings {
            vocabulary: "/x/terms.txt".to_string(),
            ..defaults()
        };
        save(&files, &s).unwrap();
        assert_eq!(
            files.read(SETTINGS).unwrap(),
            br#"{"summaries":true,"library_open":true,"vocabulary":"/x/terms.txt","cleanup":false,"summary_model":"","cleanup_model":"","library_width":220,"input_device":"","silence_split_ms":0,"keep_segments":false,"learned_vocabulary":"","model_folder":"","toggle_shortcut":"Opt+Cmd+D","pause_shortcut":"Opt+Cmd+P"}"#,
            "bytes"
        );
        assert_eq!(load(&files), s, "read back");
    }

    #[test]
    fn a_failed_save_is_an_error_and_keeps_the_old_file() {
        let files = with(br#"{"summaries":false}"#);
        files.fail_rename.set(true);
        assert!(save(&files, &defaults()).is_err(), "the rename failed");
        assert!(!load(&files).summaries, "the old file stays");
        files.fail_rename.set(false);
        files.fail_write.set(true);
        assert!(save(&files, &defaults()).is_err(), "the write failed");
    }

    #[test]
    fn cleanup_is_off_unless_it_is_true() {
        assert!(!load(&with(b"{}")).cleanup, "missing");
        assert!(load(&with(br#"{"cleanup":true}"#)).cleanup, "true");
        assert!(!load(&with(br#"{"cleanup":"yes"}"#)).cleanup, "not a bool");
        assert!(!load(&with(b"not json")).cleanup, "invalid file");
        let unreadable = with(br#"{"cleanup":true}"#);
        unreadable.fail_read.set(true);
        assert!(!load(&unreadable).cleanup, "unreadable");
    }

    #[test]
    fn cleanup_is_saved_fourth() {
        let files = FakeFiles::default();
        let s = Settings {
            cleanup: true,
            ..defaults()
        };
        save(&files, &s).unwrap();
        assert_eq!(
            files.read(SETTINGS).unwrap(),
            br#"{"summaries":true,"library_open":true,"vocabulary":"","cleanup":true,"summary_model":"","cleanup_model":"","library_width":220,"input_device":"","silence_split_ms":0,"keep_segments":false,"learned_vocabulary":"","model_folder":"","toggle_shortcut":"Opt+Cmd+D","pause_shortcut":"Opt+Cmd+P"}"#,
            "bytes"
        );
        assert_eq!(load(&files), s, "read back");
    }

    #[test]
    fn a_model_is_kept_only_when_it_is_a_string() {
        for key in ["summary_model", "cleanup_model"] {
            let pick = |s: Settings| {
                if key == "summary_model" {
                    s.summary_model
                } else {
                    s.cleanup_model
                }
            };
            assert_eq!(pick(load(&with(b"{}"))), "", "{key}: missing");
            let number = format!(r#"{{"{key}":42}}"#);
            assert_eq!(pick(load(&with(number.as_bytes()))), "", "{key}: 42");
            let named = format!(r#"{{"{key}":"gemma3:4b"}}"#);
            assert_eq!(
                pick(load(&with(named.as_bytes()))),
                "gemma3:4b",
                "{key}: a name"
            );
        }
    }

    #[test]
    fn the_models_are_saved_before_the_learned_vocabulary() {
        let files = FakeFiles::default();
        let s = Settings {
            summary_model: "gemma3:4b".to_string(),
            ..defaults()
        };
        save(&files, &s).unwrap();
        assert_eq!(
            files.read(SETTINGS).unwrap(),
            br#"{"summaries":true,"library_open":true,"vocabulary":"","cleanup":false,"summary_model":"gemma3:4b","cleanup_model":"","library_width":220,"input_device":"","silence_split_ms":0,"keep_segments":false,"learned_vocabulary":"","model_folder":"","toggle_shortcut":"Opt+Cmd+D","pause_shortcut":"Opt+Cmd+P"}"#,
            "bytes"
        );
        assert_eq!(load(&files), s, "read back");
        let other = Settings {
            cleanup_model: "llama3:8b".to_string(),
            ..defaults()
        };
        save(&files, &other).unwrap();
        assert_eq!(load(&files), other, "cleanup model read back");
    }

    #[test]
    fn the_width_constants_are_pinned() {
        assert_eq!(LIBRARY_WIDTH_DEFAULT, 220, "default");
        assert_eq!(LIBRARY_WIDTH_MIN, 110, "min");
        assert_eq!(LIBRARY_WIDTH_MAX, 440, "max");
    }

    #[test]
    fn the_library_width_is_clamped_or_defaulted_on_read() {
        for (value, want) in [
            ("300", 300),
            ("109", 110),
            ("110", 110),
            ("111", 111),
            ("439", 439),
            ("440", 440),
            ("441", 440),
            ("0", 110),
            ("-1", 220),
            ("1.5", 220),
            (r#""wide""#, 220),
            ("null", 220),
        ] {
            let json = format!(r#"{{"library_width":{value}}}"#);
            assert_eq!(load(&with(json.as_bytes())).library_width, want, "{value}");
        }
        assert_eq!(load(&with(b"{}")).library_width, 220, "missing");
    }

    #[test]
    fn the_library_width_is_saved_before_the_learned_vocabulary() {
        let files = FakeFiles::default();
        let s = Settings {
            library_width: 300,
            ..defaults()
        };
        save(&files, &s).unwrap();
        let bytes = files.read(SETTINGS).unwrap();
        assert!(
            bytes.ends_with(br#","library_width":300,"input_device":"","silence_split_ms":0,"keep_segments":false,"learned_vocabulary":"","model_folder":"","toggle_shortcut":"Opt+Cmd+D","pause_shortcut":"Opt+Cmd+P"}"#),
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        assert_eq!(load(&files), s, "read back");
    }

    #[test]
    fn the_input_device_is_kept_only_when_it_is_a_string() {
        assert_eq!(
            load(&with(
                br#"{"input_device":"AirPods Pro","silence_split_ms":0,"keep_segments":false,"learned_vocabulary":"","model_folder":"","toggle_shortcut":"Opt+Cmd+D"}"#
            ))
            .input_device,
            "AirPods Pro",
            "a name"
        );
        assert_eq!(
            load(&with(br#"{"input_device":" Mic "}"#)).input_device,
            " Mic ",
            "kept exactly"
        );
        assert_eq!(load(&with(b"{}")).input_device, "", "missing");
        for bytes in [&br#"{"input_device":42}"#[..], br#"{"input_device":null}"#] {
            assert_eq!(
                load(&with(bytes)).input_device,
                "",
                "{}",
                String::from_utf8_lossy(bytes)
            );
        }
    }

    #[test]
    fn the_input_device_is_saved_before_the_split() {
        let files = FakeFiles::default();
        let s = Settings {
            input_device: "AirPods Pro".to_string(),
            ..defaults()
        };
        save(&files, &s).unwrap();
        assert_eq!(
            files.read(SETTINGS).unwrap(),
            br#"{"summaries":true,"library_open":true,"vocabulary":"","cleanup":false,"summary_model":"","cleanup_model":"","library_width":220,"input_device":"AirPods Pro","silence_split_ms":0,"keep_segments":false,"learned_vocabulary":"","model_folder":"","toggle_shortcut":"Opt+Cmd+D","pause_shortcut":"Opt+Cmd+P"}"#,
            "bytes"
        );
        assert_eq!(load(&files), s, "read back");
    }

    #[test]
    fn the_silence_split_is_kept_only_when_it_is_off_or_in_range() {
        assert_eq!(
            (SILENCE_SPLIT_MIN_MS, SILENCE_SPLIT_MAX_MS),
            (1_000, 10_000),
            "the range"
        );
        for (stored, want) in [
            ("0", 0),
            ("1000", 1_000),
            ("1500", 1_500),
            ("2500", 2_500),
            ("3000", 3_000),
            ("4000", 4_000),
            ("10000", 10_000),
            ("999", 0),
            ("10001", 0),
            ("-1500", 0),
            ("1500.5", 0),
            ("\"1500\"", 0),
            ("null", 0),
        ] {
            let bytes = format!(r#"{{"silence_split_ms":{stored}}}"#);
            assert_eq!(
                load(&with(bytes.as_bytes())).silence_split_ms,
                want,
                "{stored}"
            );
        }
        assert_eq!(load(&with(b"{}")).silence_split_ms, 0, "missing");
        let files = FakeFiles::default();
        let s = Settings {
            silence_split_ms: 2_500,
            ..defaults()
        };
        save(&files, &s).unwrap();
        assert!(
            files
                .read(SETTINGS)
                .unwrap()
                .ends_with(br#","input_device":"","silence_split_ms":2500,"keep_segments":false,"learned_vocabulary":"","model_folder":"","toggle_shortcut":"Opt+Cmd+D","pause_shortcut":"Opt+Cmd+P"}"#),
            "saved before the learned vocabulary"
        );
        assert_eq!(load(&files), s, "read back");
    }

    #[test]
    fn keep_segments_is_true_only_for_the_boolean_true() {
        for (stored, want) in [
            (None, false),
            (Some("true"), true),
            (Some("false"), false),
            (Some("\"true\""), false),
            (Some("1"), false),
            (Some("null"), false),
        ] {
            let bytes = stored.map_or_else(
                || "{}".to_string(),
                |v| format!(r#"{{"keep_segments":{v}}}"#),
            );
            assert_eq!(
                load(&with(bytes.as_bytes())).keep_segments,
                want,
                "{stored:?}"
            );
        }
        assert!(!load(&FakeFiles::default()).keep_segments, "no file");
        assert!(!load(&with(b"not json")).keep_segments, "invalid file");
        let files = FakeFiles::default();
        let s = Settings {
            keep_segments: true,
            ..defaults()
        };
        save(&files, &s).unwrap();
        assert!(
            files.read(SETTINGS).unwrap().ends_with(
                br#","silence_split_ms":0,"keep_segments":true,"learned_vocabulary":"","model_folder":"","toggle_shortcut":"Opt+Cmd+D","pause_shortcut":"Opt+Cmd+P"}"#
            ),
            "saved before the learned vocabulary"
        );
        assert_eq!(load(&files), s, "read back");
        assert_eq!(
            load(&with(br#"{"keep_segments":true}"#)),
            Settings {
                keep_segments: true,
                ..defaults()
            },
            "no other key is affected"
        );
    }
}
