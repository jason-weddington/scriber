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

    use scriber_core::model::{AppInfo, ExtensionSummary, ScanProgress, ScanSummary};

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
    fn app_info() {
        round_trip(
            "appInfo",
            &AppInfo {
                version: "1.2.3".to_string(),
            },
        );
    }

    #[test]
    fn scan_progress() {
        round_trip(
            "scanProgress",
            &ScanProgress {
                files: 2,
                bytes: 1200,
                folders: 1,
            },
        );
    }

    #[test]
    fn scan_summary() {
        round_trip(
            "scanSummary",
            &ScanSummary {
                root: "/Users/me/Pictures".to_string(),
                files: 3,
                total_bytes: 1500,
                unreadable_folders: 1,
                extensions: vec![
                    ExtensionSummary {
                        extension: "jpg".to_string(),
                        files: 2,
                        bytes: 1200,
                    },
                    ExtensionSummary {
                        extension: String::new(),
                        files: 1,
                        bytes: 300,
                    },
                ],
                elapsed_ms: 42,
            },
        );
    }

    #[test]
    fn unknown_fields_are_refused() {
        let mut progress = fixture("scanProgress");
        let _: Option<Value> = progress
            .as_object_mut()
            .unwrap()
            .insert("extra".to_string(), json!(1));
        assert!(
            serde_json::from_value::<ScanProgress>(progress).is_err(),
            "a field the contract does not name is an error, not dropped"
        );
    }

    #[test]
    fn missing_fields_are_refused() {
        assert!(
            serde_json::from_value::<ScanProgress>(json!({ "files": 1, "bytes": 2 })).is_err(),
            "every field is required"
        );
    }
}
