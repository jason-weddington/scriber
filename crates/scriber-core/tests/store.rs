//! The data folder's layout.

#[expect(dead_code, reason = "these tests need only the fake data folder")]
mod common;

#[cfg(test)]
mod tests {
    use scriber_core::ports::PortError;
    use scriber_core::store::{
        LEARNED, LEARNING_LOG, LEARNING_LOG_LINES, MODEL_LOG, MODEL_LOG_LINES, learned,
        learned_path, log_learning, log_model, save_learned,
    };
    use scriber_core::store::{
        MODEL, MODEL_FILE, MODEL_FOLDER, SEGMENTS, STARTED, TRANSCRIPT, VOCABULARY, clear, load,
        model_folder, model_path, remove_segments, save, save_transcript, segment_index,
        segment_path, stored, transcript, vocabulary, vocabulary_path,
    };

    use scriber_core::ports::Files as _;

    use crate::common::FakeFiles;

    #[test]
    fn the_layout_is_pinned() {
        assert_eq!(SEGMENTS, "segments", "folder");
        assert_eq!(VOCABULARY, "vocabulary.txt", "glossary");
        assert_eq!(MODEL, "models/ggml-large-v3-turbo.bin", "model");
        assert_eq!(MODEL_FOLDER, "models", "model folder");
        assert_eq!(MODEL_FILE, "ggml-large-v3-turbo.bin", "model file");
        assert_eq!(TRANSCRIPT, "segments/transcript.txt", "transcript");
        assert_eq!(STARTED, "segments/started.txt", "session start");
        assert_eq!(segment_path(7), "segments/segment-0007.wav", "zero-padded");
        assert_eq!(
            segment_path(12_345),
            "segments/segment-12345.wav",
            "wider when needed"
        );
    }

    #[test]
    fn segment_names_read_back() {
        for i in [0, 7, 9_999, 12_345, u32::MAX] {
            let path = segment_path(i);
            let name = path.strip_prefix("segments/").unwrap();
            assert_eq!(segment_index(name), Some(i), "{i}");
        }
    }

    #[test]
    fn other_files_are_not_segments() {
        for name in [
            "segment-.wav",
            "segment-+1.wav",
            "segment- 1.wav",
            "segment-1.wav.tmp",
            "segment-1.WAV",
            "take-1.wav",
            ".DS_Store",
            "segment-99999999999.wav",
        ] {
            assert_eq!(segment_index(name), None, "{name}");
        }
    }

    #[test]
    fn segments_save_load_list_and_clear() {
        let files = FakeFiles::default();
        save(&files, 2, b"two").unwrap();
        save(&files, 0, b"zero").unwrap();
        files.put_segment(10, b"ten".to_vec());
        let _: Option<_> = files
            .files
            .borrow_mut()
            .insert("segments/notes.txt".to_string(), b"keep".to_vec());
        files.set_vocabulary(Some("talos"));
        save_transcript(&files, "said").unwrap();
        files.write(STARTED, b"2026-10-26 08:00:00").unwrap();
        assert_eq!(
            transcript(&files).as_deref(),
            Some("said"),
            "transcript kept"
        );
        assert_eq!(load(&files, 2).unwrap(), b"two", "read back");
        assert_eq!(
            stored(&files).unwrap(),
            [0, 2, 10],
            "ascending, other files ignored"
        );
        clear(&files).unwrap();
        assert_eq!(stored(&files).unwrap(), Vec::<u32>::new(), "all gone");
        assert_eq!(transcript(&files), None, "the transcript too");
        assert!(files.read(STARTED).is_err(), "and the session's start");
        let left: Vec<String> = files.files.borrow().keys().cloned().collect();
        assert_eq!(
            left,
            ["segments/notes.txt", "vocabulary.txt"],
            "nothing else touched"
        );
    }

    #[test]
    fn failures_pass_through() {
        let files = FakeFiles::default();
        files.put_segment(0, b"x".to_vec());
        files.fail_list.set(true);
        assert_eq!(
            clear(&files),
            Err(PortError("permission denied".to_string())),
            "an unreadable folder"
        );
        files.fail_list.set(false);
        files.fail_remove.set(true);
        assert_eq!(
            clear(&files),
            Err(PortError("busy".to_string())),
            "a stuck file"
        );
        assert_eq!(files.segments(), 1, "still there");
        assert!(load(&files, 5).is_err(), "a missing segment");
    }

    #[test]
    fn the_glossary_is_read_as_utf8_text() {
        let files = FakeFiles::default();
        assert_eq!(vocabulary(&files), None, "missing");
        files.set_vocabulary(Some("talos\nagent-gtd"));
        assert_eq!(
            vocabulary(&files).as_deref(),
            Some("talos\nagent-gtd"),
            "read"
        );
        let _: Option<_> = files
            .files
            .borrow_mut()
            .insert(VOCABULARY.to_string(), vec![0xff, 0xfe]);
        assert_eq!(vocabulary(&files), None, "not UTF-8");
    }

    #[test]
    fn the_glossary_is_vocabulary_txt_by_default() {
        let files = FakeFiles::default();
        assert_eq!(vocabulary_path(&files), "vocabulary.txt", "default");
        files.set_vocabulary(Some("talos"));
        assert_eq!(vocabulary(&files).as_deref(), Some("talos"), "read");
    }

    #[test]
    fn the_glossary_path_in_the_settings_wins() {
        let files = FakeFiles::default();
        files.set_vocabulary(Some("talos"));
        files
            .write("settings.json", br#"{"vocabulary":"/elsewhere/terms.txt"}"#)
            .unwrap();
        assert_eq!(
            vocabulary_path(&files),
            "/elsewhere/terms.txt",
            "configured"
        );
        assert_eq!(
            vocabulary(&files),
            None,
            "missing, and vocabulary.txt ignored"
        );
        files.write("/elsewhere/terms.txt", b"pulumi").unwrap();
        assert_eq!(vocabulary(&files).as_deref(), Some("pulumi"), "read");
    }

    #[test]
    fn a_transcript_that_is_not_utf8_is_none() {
        let files = FakeFiles::default();
        let _: Option<_> = files
            .files
            .borrow_mut()
            .insert(TRANSCRIPT.to_string(), vec![0xff]);
        assert_eq!(transcript(&files), None, "not UTF-8");
    }

    #[test]
    fn the_transcript_is_written_then_renamed_into_place() {
        let files = FakeFiles::default();
        save_transcript(&files, "said").unwrap();
        let held = files.files.borrow();
        assert_eq!(
            held.get("segments/transcript.txt").map(Vec::as_slice),
            Some(b"said".as_slice()),
            "the whole transcript"
        );
        assert!(
            !held.contains_key("segments/transcript.txt.partial"),
            "no partial file left"
        );
    }

    #[test]
    fn a_transcript_that_cannot_be_renamed_into_place_is_an_error() {
        let files = FakeFiles::default();
        files.fail_rename.set(true);
        assert!(
            save_transcript(&files, "said").is_err(),
            "the rename failed"
        );
        assert!(
            !files.files.borrow().contains_key("segments/transcript.txt"),
            "no transcript"
        );
    }

    #[test]
    fn remove_segments_keeps_the_transcript_and_the_start() {
        let files = FakeFiles::default();
        save(&files, 0, b"zero").unwrap();
        save(&files, 3, b"three").unwrap();
        save_transcript(&files, "said").unwrap();
        files.write(STARTED, b"2026-10-26 08:00:00").unwrap();
        remove_segments(&files);
        let left: Vec<String> = files.files.borrow().keys().cloned().collect();
        assert_eq!(
            left,
            ["segments/started.txt", "segments/transcript.txt"],
            "only the segments' audio is gone"
        );
    }

    #[test]
    fn remove_segments_ignores_failures() {
        let files = FakeFiles::default();
        save(&files, 0, b"zero").unwrap();
        files.fail_remove.set(true);
        remove_segments(&files);
        assert_eq!(files.segments(), 1, "a stuck file stays");
        files.fail_remove.set(false);
        files.fail_list.set(true);
        remove_segments(&files);
        assert_eq!(files.segments(), 1, "an unreadable folder keeps its files");
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn the_learned_path_is_learned_txt_by_default() {
        let files = FakeFiles::default();
        assert_eq!(learned_path(&files), "learned.txt", "default");
        files.write("settings.json", b"{}").unwrap();
        assert_eq!(learned_path(&files), "learned.txt", "empty settings");
        files
            .write(
                "settings.json",
                br#"{"learned_vocabulary":"/x/learned.txt"}"#,
            )
            .unwrap();
        assert_eq!(learned_path(&files), "/x/learned.txt", "configured");
        files
            .write("settings.json", br#"{"learned_vocabulary":"learned.txt"}"#)
            .unwrap();
        assert_eq!(
            learned_path(&files),
            "learned.txt",
            "a relative value was dropped by settings::load"
        );
    }

    #[test]
    fn the_learned_path_in_the_settings_wins() {
        let files = FakeFiles::default();
        files.write(LEARNED, b"default\n").unwrap();
        files
            .write(
                "settings.json",
                br#"{"learned_vocabulary":"/x/learned.txt"}"#,
            )
            .unwrap();
        files.write("/x/learned.txt", b"configured\n").unwrap();
        assert_eq!(
            learned(&files),
            Some(strings(&["configured"])),
            "the configured file, not learned.txt"
        );
        files
            .write("settings.json", br#"{"learned_vocabulary":""}"#)
            .unwrap();
        assert_eq!(
            learned(&files),
            Some(strings(&["default"])),
            "back to the default"
        );
    }

    #[test]
    fn the_learned_list_is_written_where_the_settings_say() {
        let files = FakeFiles::default();
        files
            .write(
                "settings.json",
                br#"{"learned_vocabulary":"/x/learned.txt"}"#,
            )
            .unwrap();
        files.write("/x/learned.txt", b"old\n").unwrap();
        files.fail_rename.set(true);
        assert!(
            save_learned(&files, &strings(&["new"])).is_err(),
            "the rename failed"
        );
        assert_eq!(
            files.read("/x/learned.txt").unwrap(),
            b"old\n",
            "the old file is untouched"
        );
        assert_eq!(
            files.read("/x/learned.txt.partial").unwrap(),
            b"new\n",
            "written beside the configured path"
        );
        assert!(
            files.read(LEARNED).is_err(),
            "no learned.txt at the default path"
        );
        files.fail_rename.set(false);
        save_learned(&files, &strings(&["new"])).unwrap();
        assert_eq!(
            files.read("/x/learned.txt").unwrap(),
            b"new\n",
            "renamed into place"
        );
        assert!(
            files.read("/x/learned.txt.partial").is_err(),
            "no partial file left"
        );
        assert!(
            files.read(LEARNED).is_err(),
            "still no learned.txt at the default path"
        );
    }

    #[test]
    fn the_learned_list_is_read_leniently() {
        assert_eq!(LEARNED, "learned.txt", "path");
        assert_eq!(LEARNING_LOG, "learning-log.jsonl", "log path");
        assert_eq!(LEARNING_LOG_LINES, 200, "log length");
        let files = FakeFiles::default();
        assert_eq!(learned(&files), Some(vec![]), "no file");
        files
            .write(LEARNED, b"tightbeam\n# note\n\n Scriber \n")
            .unwrap();
        assert_eq!(
            learned(&files),
            Some(strings(&["tightbeam", "Scriber"])),
            "read like the glossary"
        );
        files.fail_read.set(true);
        assert_eq!(learned(&files), Some(vec![]), "unreadable counts as empty");
        files.fail_read.set(false);
        files.write(LEARNED, &[0xff]).unwrap();
        assert_eq!(learned(&files), None, "not UTF-8");
    }

    #[test]
    fn the_learned_list_is_written_then_renamed_into_place() {
        let files = FakeFiles::default();
        save_learned(&files, &strings(&["tightbeam", "Scriber"])).unwrap();
        assert_eq!(
            files.read(LEARNED).unwrap(),
            b"tightbeam\nScriber\n",
            "one term per line"
        );
        assert!(
            !files.files.borrow().contains_key("learned.txt.partial"),
            "no partial file left"
        );
        save_learned(&files, &[]).unwrap();
        assert_eq!(
            files.read(LEARNED).unwrap(),
            b"",
            "an empty list is 0 bytes"
        );
        files.write(LEARNED, b"old\n").unwrap();
        files.fail_rename.set(true);
        assert!(
            save_learned(&files, &strings(&["new"])).is_err(),
            "the rename failed"
        );
        assert_eq!(files.read(LEARNED).unwrap(), b"old\n", "unchanged");
    }

    #[test]
    fn the_learning_log_keeps_the_newest_lines() {
        let files = FakeFiles::default();
        log_learning(&files, "first");
        assert_eq!(files.read(LEARNING_LOG).unwrap(), b"first\n", "one line");
        let old: String = (0..200)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        files.write(LEARNING_LOG, old.as_bytes()).unwrap();
        log_learning(&files, "new");
        let text = String::from_utf8(files.read(LEARNING_LOG).unwrap()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 200, "keeps 200");
        assert_eq!(lines.first().copied(), Some("line 1"), "the oldest dropped");
        assert_eq!(lines.last().copied(), Some("new"), "the newest last");
    }

    #[test]
    fn the_model_folder_and_path_default_to_the_data_folder() {
        let files = FakeFiles::default();
        assert_eq!(model_folder(&files), "models", "no file");
        assert_eq!(
            model_path(&files),
            "models/ggml-large-v3-turbo.bin",
            "no file"
        );
    }

    #[test]
    fn the_model_folder_and_path_follow_the_settings() {
        let files = FakeFiles::default();
        files
            .write("settings.json", br#"{"model_folder":"/x/models"}"#)
            .unwrap();
        assert_eq!(model_folder(&files), "/x/models", "configured");
        assert_eq!(
            model_path(&files),
            "/x/models/ggml-large-v3-turbo.bin",
            "configured"
        );
        files
            .write("settings.json", br#"{"model_folder":"/x/models/"}"#)
            .unwrap();
        assert_eq!(
            model_path(&files),
            "/x/models/ggml-large-v3-turbo.bin",
            "one slash"
        );
        files
            .write("settings.json", br#"{"model_folder":"models"}"#)
            .unwrap();
        assert_eq!(model_folder(&files), "models", "relative is the default");
        assert_eq!(
            model_path(&files),
            "models/ggml-large-v3-turbo.bin",
            "relative is the default"
        );
    }

    #[test]
    fn the_model_log_keeps_the_newest_lines() {
        assert_eq!(MODEL_LOG, "model-log.jsonl", "log path");
        assert_eq!(MODEL_LOG_LINES, 200, "log length");
        let files = FakeFiles::default();
        log_model(&files, "first");
        assert_eq!(files.read(MODEL_LOG).unwrap(), b"first\n", "one line");
        let old: String = (0..200)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        files.write(MODEL_LOG, old.as_bytes()).unwrap();
        log_model(&files, "new");
        let text = String::from_utf8(files.read(MODEL_LOG).unwrap()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 200, "keeps 200");
        assert_eq!(lines.first().copied(), Some("line 1"), "the oldest dropped");
        assert_eq!(lines.last().copied(), Some("new"), "the newest last");
    }
}
