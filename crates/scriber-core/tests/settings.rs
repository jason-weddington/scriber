//! The user's settings in `settings.json`.

#[expect(dead_code, reason = "these tests need only the fake data folder")]
mod common;

#[cfg(test)]
mod tests {
    use scriber_core::model::Settings;
    use scriber_core::ports::Files as _;
    use scriber_core::settings::{SETTINGS, load, save};
    use serde_json::{Value, json};

    use crate::common::FakeFiles;

    const DEFAULTS: Settings = Settings {
        summaries: true,
        library_open: true,
    };

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
        assert_eq!(load(&FakeFiles::default()), DEFAULTS, "missing");
        let files = with(br#"{"summaries":false,"library_open":false}"#);
        files.fail_read.set(true);
        assert_eq!(load(&files), DEFAULTS, "unreadable");
    }

    #[test]
    fn an_invalid_file_means_the_defaults() {
        for bytes in [&b"not json"[..], b"[false]", b"false", b"", &[0xff, 0xfe]] {
            assert_eq!(load(&with(bytes)), DEFAULTS, "{bytes:?}");
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
                },
            ),
            (
                br#"{"library_open":false}"#,
                Settings {
                    summaries: true,
                    library_open: false,
                },
            ),
            (
                br#"{"summaries":"no","library_open":false}"#,
                Settings {
                    summaries: true,
                    library_open: false,
                },
            ),
            (
                br#"{"summaries":false,"library_open":0,"extra":1}"#,
                Settings {
                    summaries: false,
                    library_open: true,
                },
            ),
            (br#"{"libraryOpen":false}"#, DEFAULTS),
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
        };
        save(&files, &off).unwrap();
        assert_eq!(load(&files), off, "read back");
        let written: Value = serde_json::from_slice(&files.read(SETTINGS).unwrap()).unwrap();
        assert_eq!(
            written,
            json!({"summaries": false, "library_open": false}),
            "snake_case on disk"
        );
        assert_eq!(
            files.read(SETTINGS).unwrap(),
            br#"{"summaries":false,"library_open":false}"#,
            "compact"
        );
        assert!(
            files.read("settings.json.partial").is_err(),
            "no partial file left"
        );
        save(&files, &DEFAULTS).unwrap();
        assert_eq!(load(&files), DEFAULTS, "overwritten");
    }

    #[test]
    fn a_failed_save_is_an_error_and_keeps_the_old_file() {
        let files = with(br#"{"summaries":false}"#);
        files.fail_rename.set(true);
        assert!(save(&files, &DEFAULTS).is_err(), "the rename failed");
        assert!(!load(&files).summaries, "the old file stays");
        files.fail_rename.set(false);
        files.fail_write.set(true);
        assert!(save(&files, &DEFAULTS).is_err(), "the write failed");
    }
}
