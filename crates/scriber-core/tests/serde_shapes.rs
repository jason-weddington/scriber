//! The JSON shapes of the bridge contract (`docs/design.md`), pinned.
//!
//! `fixtures/bridge-shapes.json` is the one example of each shape. These
//! tests prove the Rust types read it and write it back byte-for-byte
//! equivalent; the frontend's mock-honesty test
//! (`apps/desktop/tests/unit/mock.spec.ts`) proves the browser mock answers
//! with the same shapes. A field renamed on either side fails one of them.

#[cfg(test)]
mod tests {
    use serde::Serialize;
    use serde::de::DeserializeOwned;
    use serde_json::{Value, json};

    use scriber_core::model::{
        Action, Download, InputDevices, LibraryEntry, Phase, Settings, Snapshot,
    };

    const FIXTURE: &str = include_str!("fixtures/bridge-shapes.json");

    fn fixture(key: &str) -> Value {
        let all: Value = serde_json::from_str(FIXTURE).unwrap();
        all.get(key).cloned().unwrap()
    }

    /// Reads the fixture's `key` as `T`, checks it is `want`, and checks that
    /// writing it back gives the fixture's JSON exactly.
    fn round_trip<T>(key: &str, want: &T)
    where
        T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        let json = fixture(key);
        let got: T = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(&got, want, "{key}: reads as the expected value");
        assert_eq!(
            serde_json::to_value(&got).unwrap(),
            json,
            "{key}: writes back unchanged"
        );
    }

    #[test]
    fn snapshot() {
        round_trip(
            "snapshot",
            &Snapshot {
                revision: 12,
                phase: Phase::Finished,
                recorded_ms: 83_500,
                segments: 3,
                pending: 0,
                text: "First thought.\n\nSecond thought.".to_string(),
                error: None,
                notice: Some("Copied to the clipboard.".to_string()),
                model_error: None,
                download: None,
                library_revision: 3,
                microphone: Some("Jabra Evolve 65".to_string()),
                learned: vec!["tightbeam".to_string()],
            },
        );
        round_trip(
            "snapshotFailed",
            &Snapshot {
                revision: 7,
                phase: Phase::Failed,
                recorded_ms: 4_000,
                segments: 2,
                pending: 0,
                text: String::new(),
                error: Some(
                    "Part 2 of 2 could not be transcribed: model exploded. Retry to try again."
                        .to_string(),
                ),
                notice: None,
                model_error: Some(
                    "Could not download the speech model: the connection was reset.".to_string(),
                ),
                download: None,
                library_revision: 0,
                microphone: None,
                learned: vec![],
            },
        );
        round_trip(
            "snapshotDownloading",
            &Snapshot {
                revision: 1,
                phase: Phase::Idle,
                recorded_ms: 0,
                segments: 0,
                pending: 0,
                text: String::new(),
                error: None,
                notice: None,
                model_error: None,
                download: Some(Download {
                    received_bytes: 812_277_637,
                    total_bytes: 1_624_555_275,
                }),
                library_revision: 0,
                microphone: None,
                learned: vec![],
            },
        );
    }

    #[test]
    fn library_entry_and_settings() {
        round_trip(
            "libraryEntry",
            &LibraryEntry {
                id: "20261026-140309".to_string(),
                title: "2026-10-26 14:03:09".to_string(),
                summary: "A note about the board.".to_string(),
            },
        );
        round_trip(
            "settings",
            &Settings {
                summaries: true,
                library_open: true,
                vocabulary: "/data/vocabulary.txt".to_string(),
                cleanup: true,
                summary_model: "gemma3:4b".to_string(),
                cleanup_model: "qwen3.8:27b-mlx".to_string(),
                library_width: 300,
                input_device: "AirPods Pro".to_string(),
            },
        );
    }

    #[test]
    fn input_devices() {
        round_trip(
            "inputDevices",
            &InputDevices {
                default_device: Some("Jabra Evolve 65".to_string()),
                others: vec![
                    "AirPods Pro".to_string(),
                    "MacBook Pro Microphone".to_string(),
                ],
            },
        );
        round_trip(
            "inputDevicesNone",
            &InputDevices {
                default_device: None,
                others: vec![],
            },
        );
        let (extra, missing) = altered("inputDevices", "others");
        assert!(
            serde_json::from_value::<InputDevices>(extra).is_err(),
            "input devices: unknown field"
        );
        assert!(
            serde_json::from_value::<InputDevices>(missing).is_err(),
            "input devices: missing others"
        );
        let (_, no_default) = altered("inputDevices", "defaultDevice");
        assert_eq!(
            serde_json::from_value::<InputDevices>(no_default)
                .unwrap()
                .default_device,
            None,
            "a missing defaultDevice reads as none"
        );
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn input_devices_are_listed_once_sorted_without_the_default() {
        let jabra = InputDevices::new(
            Some("Jabra Evolve 65".to_string()),
            [
                "MacBook Pro Microphone",
                "Jabra Evolve 65",
                "AirPods Pro",
                "AirPods Pro",
                "",
            ]
            .map(String::from),
        );
        assert_eq!(
            jabra.default_device.as_deref(),
            Some("Jabra Evolve 65"),
            "default kept"
        );
        assert_eq!(
            jabra.others,
            names(&["AirPods Pro", "MacBook Pro Microphone"]),
            "no default, no empty, no duplicate"
        );
        assert_eq!(
            InputDevices::new(None, ["zed", "Apple", "banana"].map(String::from)).others,
            names(&["Apple", "banana", "zed"]),
            "case-insensitive order"
        );
        assert_eq!(
            InputDevices::new(None, ["mic", "Mic", "mic"].map(String::from)).others,
            names(&["Mic", "mic"]),
            "ties by byte order, then once each"
        );
        assert_eq!(
            InputDevices::new(Some(String::new()), ["Mic".to_string()]),
            InputDevices {
                default_device: None,
                others: names(&["Mic"]),
            },
            "an empty default is none"
        );
    }

    /// The fixture's `key` with `extra` added, and with `drop` removed.
    fn altered(key: &str, drop: &str) -> (Value, Value) {
        let mut extra = fixture(key);
        let _: Option<Value> = extra
            .as_object_mut()
            .unwrap()
            .insert("extra".to_string(), json!(1));
        let mut missing = fixture(key);
        let _: Option<Value> = missing.as_object_mut().unwrap().remove(drop);
        (extra, missing)
    }

    #[test]
    fn library_entry_and_settings_refuse_unknown_and_missing_fields() {
        let (extra, missing) = altered("libraryEntry", "summary");
        assert!(
            serde_json::from_value::<LibraryEntry>(extra).is_err(),
            "entry: unknown field"
        );
        assert!(
            serde_json::from_value::<LibraryEntry>(missing).is_err(),
            "entry: missing field"
        );
        let (settings_extra, settings_missing) = altered("settings", "libraryOpen");
        assert!(
            serde_json::from_value::<Settings>(settings_extra).is_err(),
            "settings: unknown field"
        );
        assert!(
            serde_json::from_value::<Settings>(settings_missing).is_err(),
            "settings: missing field"
        );
        let (_, no_vocabulary) = altered("settings", "vocabulary");
        assert!(
            serde_json::from_value::<Settings>(no_vocabulary).is_err(),
            "settings: missing vocabulary"
        );
        let (_, no_cleanup) = altered("settings", "cleanup");
        assert!(
            serde_json::from_value::<Settings>(no_cleanup).is_err(),
            "settings: missing cleanup"
        );
        let (_, no_summary_model) = altered("settings", "summaryModel");
        assert!(
            serde_json::from_value::<Settings>(no_summary_model).is_err(),
            "settings: missing summaryModel"
        );
        let (_, no_cleanup_model) = altered("settings", "cleanupModel");
        assert!(
            serde_json::from_value::<Settings>(no_cleanup_model).is_err(),
            "settings: missing cleanupModel"
        );
        let (_, no_library_width) = altered("settings", "libraryWidth");
        assert!(
            serde_json::from_value::<Settings>(no_library_width).is_err(),
            "settings: missing libraryWidth"
        );
        let (_, no_input_device) = altered("settings", "inputDevice");
        assert!(
            serde_json::from_value::<Settings>(no_input_device).is_err(),
            "settings: missing inputDevice"
        );
        for bad in [json!(300.5), json!(-1)] {
            let mut settings = fixture("settings");
            let _: Option<Value> = settings
                .as_object_mut()
                .unwrap()
                .insert("libraryWidth".to_string(), bad.clone());
            assert!(
                serde_json::from_value::<Settings>(settings).is_err(),
                "settings: libraryWidth {bad}"
            );
        }
    }

    #[test]
    fn every_phase_is_its_lowercase_name() {
        for (phase, name) in [
            (Phase::Idle, "idle"),
            (Phase::Ready, "ready"),
            (Phase::Recording, "recording"),
            (Phase::Paused, "paused"),
            (Phase::Transcribing, "transcribing"),
            (Phase::Finished, "finished"),
            (Phase::Failed, "failed"),
        ] {
            assert_eq!(serde_json::to_value(phase).unwrap(), json!(name), "{name}");
        }
    }

    #[test]
    fn every_action_is_its_camel_case_name() {
        for (action, name) in [
            (Action::New, "new"),
            (Action::Record, "record"),
            (Action::Pause, "pause"),
            (Action::Resume, "resume"),
            (Action::Done, "done"),
            (Action::Discard, "discard"),
            (Action::Retry, "retry"),
            (Action::Copy, "copy"),
            (Action::DownloadModel, "downloadModel"),
            (Action::UndoLearn, "undoLearn"),
        ] {
            assert_eq!(serde_json::to_value(action).unwrap(), json!(name), "{name}");
            assert_eq!(
                serde_json::from_value::<Action>(json!(name)).unwrap(),
                action,
                "{name} reads back"
            );
        }
        assert!(
            serde_json::from_value::<Action>(json!("Pause")).is_err(),
            "names are exact"
        );
    }

    #[test]
    fn unknown_fields_are_refused() {
        let mut snapshot = fixture("snapshot");
        let _: Option<Value> = snapshot
            .as_object_mut()
            .unwrap()
            .insert("extra".to_string(), json!(1));
        assert!(
            serde_json::from_value::<Snapshot>(snapshot).is_err(),
            "a field the contract does not name is an error, not dropped"
        );
    }

    #[test]
    fn missing_fields_are_refused() {
        assert!(
            serde_json::from_value::<Snapshot>(json!({ "phase": "idle", "recordedMs": 0 }))
                .is_err(),
            "every field is required"
        );
    }
}
