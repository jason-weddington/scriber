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

    use scriber_core::model::{Action, Phase, Snapshot};

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
                    "Model missing: run scripts/fetch-model.sh to download it to /models/ggml-large-v3-turbo.bin."
                        .to_string(),
                ),
            },
        );
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
    fn every_action_is_its_lowercase_name() {
        for (action, name) in [
            (Action::New, "new"),
            (Action::Record, "record"),
            (Action::Pause, "pause"),
            (Action::Resume, "resume"),
            (Action::Done, "done"),
            (Action::Discard, "discard"),
            (Action::Retry, "retry"),
            (Action::Copy, "copy"),
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
