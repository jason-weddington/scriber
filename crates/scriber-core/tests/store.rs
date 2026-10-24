//! The data folder's layout.

#[expect(dead_code, reason = "these tests need only the fake data folder")]
mod common;

#[cfg(test)]
mod tests {
    use scriber_core::ports::PortError;
    use scriber_core::store::{
        MODEL, SEGMENTS, TRANSCRIPT, VOCABULARY, clear, load, save, save_transcript, segment_index,
        segment_path, stored, transcript, vocabulary,
    };

    use crate::common::FakeFiles;

    #[test]
    fn the_layout_is_pinned() {
        assert_eq!(SEGMENTS, "segments", "folder");
        assert_eq!(VOCABULARY, "vocabulary.txt", "glossary");
        assert_eq!(MODEL, "models/ggml-large-v3-turbo.bin", "model");
        assert_eq!(TRANSCRIPT, "segments/transcript.txt", "transcript");
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
    fn a_transcript_that_is_not_utf8_is_none() {
        let files = FakeFiles::default();
        let _: Option<_> = files
            .files
            .borrow_mut()
            .insert(TRANSCRIPT.to_string(), vec![0xff]);
        assert_eq!(transcript(&files), None, "not UTF-8");
    }
}
