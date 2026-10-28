//! The recording library: titles and ids, excerpts and summaries, the
//! archive a finished session makes, listing, and the encode and summary
//! jobs.

#[expect(dead_code, reason = "these tests need no segment or glossary helpers")]
mod common;

#[cfg(test)]
mod tests {
    use scriber_core::audio::decode_wav;
    use scriber_core::dictation::{Dictation, Job};
    use scriber_core::library::{
        base_id, clean_summary, entry_path, excerpt, list, title, transcript, valid_id,
    };
    use scriber_core::model::{LibraryEntry, Phase, Snapshot};
    use scriber_core::ports::{Files as _, LocalTime};
    use serde_json::{Value, json};

    use crate::common::{World, at, speech};

    const ID: &str = "20261026-140309";

    #[test]
    fn titles_and_ids() {
        let t = LocalTime {
            year: 2026,
            month: 1,
            day: 5,
            hour: 9,
            minute: 3,
            second: 7,
        };
        assert_eq!(title(&t), "2026-01-05 09:03:07", "zero-padded");
        assert_eq!(base_id(&title(&t)), "20260105-090307", "id");
        let last = LocalTime {
            year: 2026,
            month: 12,
            day: 31,
            hour: 23,
            minute: 59,
            second: 59,
        };
        assert_eq!(title(&last), "2026-12-31 23:59:59", "the last second");
    }

    #[test]
    fn only_entry_ids_are_valid() {
        for id in ["20261026-090000", "20261026-090000-2", "20261026-090000-17"] {
            assert!(valid_id(id), "{id}");
        }
        for id in [
            "../segments",
            "20261026-090000/x",
            "",
            "20261026_090000",
            "20261026-09000",
            "2026102-090000",
            "202610260-090000",
            "20261026-0900000",
            "20261026-090000-",
            "20261026-090000-a",
            "20261026-090000-2-3",
            "2026102a-090000",
            "20261026-09000a",
            "20261026",
            "-",
        ] {
            assert!(!valid_id(id), "{id:?}");
        }
    }

    #[test]
    fn excerpts() {
        let a = |n| "a".repeat(n);
        let b = |n| "b".repeat(n);
        for (text, want) in [
            ("  Hello\n\n world  ".to_string(), "Hello world".to_string()),
            (
                "abcdefghij abcdefghij abcdefgh".to_string(),
                "abcdefghij abcdefghij abcdefgh".to_string(),
            ),
            (
                "This is part 1 of the dictation.".to_string(),
                "This is part 1 of the...".to_string(),
            ),
            (
                "Okay, here's what I need. First, check the board; then, once it's done, let me know."
                    .to_string(),
                "Okay, here's what I need...".to_string(),
            ),
            (
                format!("{} {}", a(19), b(20)),
                format!("{} {}...", a(19), b(10)),
            ),
            (format!("{} {}", a(20), b(20)), format!("{}...", a(20))),
            (
                "Supercalifragilisticexpialidocious is long".to_string(),
                "Supercalifragilisticexpialidoc...".to_string(),
            ),
            (
                "Abcdefghijklmnopqrstuvwxyzabc, more".to_string(),
                "Abcdefghijklmnopqrstuvwxyzabc...".to_string(),
            ),
            ("é".repeat(31), format!("{}...", "é".repeat(30))),
            (format!("{}😀bbb", a(29)), format!("{}😀...", a(29))),
            (
                format!("{} ,;:-. .-x", a(21)),
                format!("{}...", a(21)),
            ),
            (String::new(), String::new()),
        ] {
            assert_eq!(excerpt(&text), want, "{text:?}");
        }
    }

    #[test]
    fn summaries_are_cleaned() {
        for (raw, want) in [
            ("  A short note.\n", Some("A short note.")),
            ("\"Quoted.\"", Some("Quoted.")),
            ("\u{201c}Curly.\u{201d}", Some("Curly.")),
            ("\" Spaced \"", Some("Spaced")),
            ("\"\"", None),
            ("\"", Some("\"")),
            ("   ", None),
            ("", None),
            ("\"Only one", Some("\"Only one")),
            ("Only one\"", Some("Only one\"")),
            (
                "\u{201d}Backwards\u{201c}",
                Some("\u{201d}Backwards\u{201c}"),
            ),
        ] {
            assert_eq!(clean_summary(raw).as_deref(), want, "{raw:?}");
        }
    }

    fn put(w: &World, path: &str, bytes: &[u8]) {
        w.files.write(path, bytes).unwrap();
    }

    fn entry_json(w: &World, id: &str) -> Value {
        serde_json::from_slice(&w.files.read(&entry_path(id, "entry.json")).unwrap()).unwrap()
    }

    fn put_entry(w: &World, id: &str, title: &str, summary: &str) {
        let json = json!({"id": id, "title": title, "summary": summary});
        put(
            w,
            &entry_path(id, "entry.json"),
            json.to_string().as_bytes(),
        );
    }

    fn entry(id: &str, title: &str, summary: &str) -> LibraryEntry {
        LibraryEntry {
            id: id.to_string(),
            title: title.to_string(),
            summary: summary.to_string(),
        }
    }

    #[test]
    fn the_list_is_newest_first() {
        let w = World::default();
        put_entry(&w, "20261026-090000", "2026-10-26 09:00:00", "b");
        put_entry(&w, "20261027-080000", "2026-10-27 08:00:00", "c");
        put_entry(&w, "20261025-230000", "2026-10-25 23:00:00", "a");
        put_entry(&w, "20261026-090000-2", "2026-10-26 09:00:00", "b2");
        put_entry(&w, "20261026-090000-10", "2026-10-26 09:00:00", "b10");
        assert_eq!(
            list(&w.files),
            [
                entry("20261027-080000", "2026-10-27 08:00:00", "c"),
                entry("20261026-090000-10", "2026-10-26 09:00:00", "b10"),
                entry("20261026-090000-2", "2026-10-26 09:00:00", "b2"),
                entry("20261026-090000", "2026-10-26 09:00:00", "b"),
                entry("20261025-230000", "2026-10-25 23:00:00", "a"),
            ],
            "by time, then by suffix as a number"
        );
    }

    #[test]
    fn entries_that_cannot_be_read_are_skipped() {
        let w = World::default();
        put_entry(&w, ID, "2026-10-26 14:03:09", "kept");
        put(
            &w,
            "library/20261026-100000/transcript.txt",
            b"no entry.json",
        );
        put(&w, "library/20261026-110000/entry.json", b"not json");
        put(&w, "library/20261026-120000/entry.json", b"[1]");
        put(
            &w,
            "library/20261026-130000/entry.json",
            br#"{"title":"t"}"#,
        );
        put(
            &w,
            "library/20261026-130001/entry.json",
            br#"{"title":1,"summary":"s"}"#,
        );
        put(
            &w,
            "library/20261026-130002/entry.json",
            br#"{"title":"t","summary":null}"#,
        );
        put_entry(&w, "notes", "2026-10-26 14:03:09", "not an id");
        put(
            &w,
            "library/20261026-150000/entry.json",
            br#"{"id":"other","title":"2026-10-26 15:00:00","summary":"s","x":1}"#,
        );
        assert_eq!(
            list(&w.files),
            [
                entry("20261026-150000", "2026-10-26 15:00:00", "s"),
                entry(ID, "2026-10-26 14:03:09", "kept"),
            ],
            "the id is the folder's, extra keys are ignored"
        );
        w.files.fail_list.set(true);
        assert_eq!(list(&w.files), [], "an unreadable library lists empty");
    }

    #[test]
    fn a_transcript_is_read_by_id() {
        let w = World::default();
        put(&w, &entry_path(ID, "transcript.txt"), b"said");
        put(&w, "segments/transcript.txt", b"current");
        assert_eq!(transcript(&w.files, ID).as_deref(), Some("said"), "read");
        assert_eq!(transcript(&w.files, "../segments"), None, "not an id");
        assert_eq!(
            transcript(&w.files, "20261026-140310"),
            None,
            "not in the library"
        );
        put(&w, &entry_path(ID, "transcript.txt"), &[0xff]);
        assert_eq!(transcript(&w.files, ID), None, "not UTF-8");
    }

    /// A library-on session of `parts` segments, each heard as "Part N.",
    /// driven to finished; the clock reads 14:03:09 throughout.
    fn session(w: &World, d: &mut Dictation, parts: usize) {
        let p = w.library_ports();
        d.new_session(&p).unwrap();
        d.record(&p).unwrap();
        let mut jobs = Vec::new();
        for n in 1..=parts {
            if n > 1 {
                d.resume(&p).unwrap();
            }
            w.transcriber
                .answers
                .borrow_mut()
                .push_back(Ok(format!("Part {n}.")));
            jobs.push(d.pause(&p).unwrap().unwrap());
        }
        assert!(d.done(&p).unwrap().is_none(), "paused");
        for job in &jobs {
            let _: Snapshot = d.complete(w.run(job), &p);
        }
    }

    fn library_jobs(w: &World) -> Vec<String> {
        w.queue
            .jobs
            .borrow()
            .iter()
            .map(|j| format!("{j:?}"))
            .collect()
    }

    fn ids(w: &World) -> Vec<String> {
        list(&w.files).into_iter().map(|e| e.id).collect()
    }

    #[test]
    fn a_finished_session_is_archived() {
        let w = World::default();
        let mut d = Dictation::default();
        session(&w, &mut d, 2);
        let s = d.snapshot(&w.library_ports());
        assert_eq!(s.phase, Phase::Finished, "finished");
        assert_eq!(s.library_revision, 1, "one entry added");
        let audio = decode_wav(&w.files.read(&entry_path(ID, "audio.wav")).unwrap()).unwrap();
        assert_eq!(audio, speech(32_000), "both segments, in order");
        assert_eq!(
            w.files.read(&entry_path(ID, "transcript.txt")).unwrap(),
            b"Part 1.\n\nPart 2.",
            "the transcript"
        );
        assert_eq!(
            w.files.read(&entry_path(ID, "entry.json")).unwrap(),
            br#"{"id":"20261026-140309","title":"2026-10-26 14:03:09","summary":"Part 1. Part 2.","summary_source":"excerpt","cleanup":"off","duration_ms":2000}"#,
            "entry.json, keys in order"
        );
        assert!(
            w.files.read(&entry_path(ID, "entry.json.partial")).is_err(),
            "no partial entry"
        );
        assert_eq!(w.files.segments(), 0, "the segments' audio is gone");
        assert!(
            w.files.read("segments/transcript.txt").is_ok(),
            "the transcript stays"
        );
        assert_eq!(
            list(&w.files),
            [entry(ID, "2026-10-26 14:03:09", "Part 1. Part 2.")],
            "listed"
        );
        let jobs = library_jobs(&w);
        assert_eq!(jobs.len(), 2, "an encode and a summary: {jobs:?}");
        assert!(jobs.first().unwrap().contains("Encode"), "{jobs:?}");
        assert!(jobs.last().unwrap().contains("Summary"), "{jobs:?}");
    }

    #[test]
    fn colliding_ids_take_a_suffix() {
        let w = World::default();
        let mut d = Dictation::default();
        for _ in 0..3 {
            session(&w, &mut d, 1);
        }
        assert_eq!(
            ids(&w),
            ["20261026-140309-3", "20261026-140309-2", ID],
            "-2, then -3"
        );
        assert_eq!(
            entry_json(&w, "20261026-140309-3").get("id"),
            Some(&json!("20261026-140309-3")),
            "the file names its own id"
        );
        assert_eq!(
            d.snapshot(&w.library_ports()).library_revision,
            3,
            "kept across New"
        );
    }

    #[test]
    fn an_unreadable_segment_is_skipped() {
        let w = World::default();
        let mut d = Dictation::default();
        let p = w.library_ports();
        d.new_session(&p).unwrap();
        d.record(&p).unwrap();
        let job = d.pause(&p).unwrap().unwrap();
        d.resume(&p).unwrap();
        let job2 = d.pause(&p).unwrap().unwrap();
        w.files.put_segment(1, b"not a wav".to_vec());
        assert!(d.done(&p).unwrap().is_none(), "paused");
        let _: Snapshot = d.complete(w.run(&job), &p);
        let _: Snapshot = d.complete(w.run(&job2), &p);
        let audio = decode_wav(&w.files.read(&entry_path(ID, "audio.wav")).unwrap()).unwrap();
        assert_eq!(audio.len(), 16_000, "only the readable segment");
    }

    #[test]
    fn a_session_with_no_audio_kept_writes_no_wav() {
        let w = World::default();
        let mut d = Dictation::default();
        let p = w.library_ports();
        d.new_session(&p).unwrap();
        d.record(&p).unwrap();
        w.files.fail_write.set(true);
        let job = d.pause(&p).unwrap().unwrap();
        w.files.fail_write.set(false);
        assert!(d.done(&p).unwrap().is_none(), "paused");
        let _: Snapshot = d.complete(w.run(&job), &p);
        assert!(
            w.files.read(&entry_path(ID, "audio.wav")).is_err(),
            "no audio"
        );
        assert_eq!(
            entry_json(&w, ID).get("duration_ms"),
            Some(&json!(0)),
            "no duration"
        );
        let jobs = library_jobs(&w);
        assert_eq!(jobs.len(), 1, "a summary only: {jobs:?}");
        assert!(jobs.first().unwrap().contains("Summary"), "{jobs:?}");
    }

    #[test]
    fn a_failed_archive_keeps_the_segments() {
        for fail in ["write", "rename", "list"] {
            let w = World::default();
            let mut d = Dictation::default();
            let p = w.library_ports();
            d.new_session(&p).unwrap();
            d.record(&p).unwrap();
            let job = d.pause(&p).unwrap().unwrap();
            assert!(d.done(&p).unwrap().is_none(), "paused");
            match fail {
                "write" => w.files.fail_write.set(true),
                "rename" => w.files.fail_rename.set(true),
                _ => w.files.fail_list.set(true),
            }
            let s = d.complete(w.run(&job), &p);
            w.files.fail_write.set(false);
            w.files.fail_rename.set(false);
            w.files.fail_list.set(false);
            assert_eq!(w.files.segments(), 1, "{fail}: the segment stays");
            assert!(
                w.files.read(&entry_path(ID, "entry.json")).is_err(),
                "{fail}: no entry.json"
            );
            assert_eq!(s.library_revision, 0, "{fail}: not bumped");
            assert_eq!(s.phase, Phase::Finished, "{fail}: finished");
            assert_eq!(s.text, "segment 0", "{fail}: the text");
            assert_eq!(
                s.notice.as_deref(),
                Some("Copied to the clipboard."),
                "{fail}: the notice"
            );
            assert!(library_jobs(&w).is_empty(), "{fail}: nothing queued");
        }
    }

    #[test]
    fn with_summaries_off_only_the_encode_is_queued() {
        let w = World::default();
        put(&w, "settings.json", br#"{"summaries":false}"#);
        let mut d = Dictation::default();
        session(&w, &mut d, 1);
        let jobs = library_jobs(&w);
        assert_eq!(jobs.len(), 1, "one job: {jobs:?}");
        assert!(jobs.first().unwrap().contains("Encode"), "{jobs:?}");
    }

    #[test]
    fn an_empty_transcript_is_not_archived() {
        let w = World::default();
        let mut d = Dictation::default();
        let p = w.library_ports();
        d.new_session(&p).unwrap();
        d.record(&p).unwrap();
        w.transcriber
            .answers
            .borrow_mut()
            .push_back(Ok(String::new()));
        let job = d.pause(&p).unwrap().unwrap();
        assert!(d.done(&p).unwrap().is_none(), "paused");
        let s = d.complete(w.run(&job), &p);
        assert_eq!(s.phase, Phase::Finished, "finished");
        assert!(
            w.files
                .files
                .borrow()
                .keys()
                .all(|k| !k.starts_with("library/")),
            "nothing under library/"
        );
        assert_eq!(s.library_revision, 0, "not bumped");
        assert_eq!(w.files.segments(), 1, "the segment stays");
    }

    /// The queued library jobs, after one archived session.
    fn archived(w: &World) -> (Dictation, Job, Job) {
        let mut d = Dictation::default();
        session(w, &mut d, 1);
        let mut jobs = w.queue.jobs.borrow_mut();
        let summary = jobs.pop().unwrap();
        let encode = jobs.pop().unwrap();
        (d, encode, summary)
    }

    #[test]
    fn an_encode_replaces_the_wav() {
        let w = World::default();
        let (mut d, encode, _) = archived(&w);
        let before = d.snapshot(&w.library_ports());
        let s = d.complete(w.run_with(&encode, &w.library_ports()), &w.library_ports());
        assert_eq!(s, before, "an encode changes nothing in the session");
        assert_eq!(
            *w.encoder.calls.borrow(),
            [(
                format!("library/{ID}/audio.wav"),
                format!("library/{ID}/audio.m4a")
            )],
            "the paths"
        );
        assert!(
            w.files.read(&entry_path(ID, "audio.wav")).is_err(),
            "the wav is gone"
        );
    }

    #[test]
    fn a_failed_encode_keeps_the_wav() {
        let w = World::default();
        let (mut d, encode, _) = archived(&w);
        w.encoder.fail.set(true);
        put(&w, &entry_path(ID, "audio.m4a"), b"partial");
        let before = d.snapshot(&w.library_ports());
        let s = d.complete(w.run_with(&encode, &w.library_ports()), &w.library_ports());
        assert_eq!(s, before, "a failed encode changes nothing in the session");
        assert!(
            w.files.read(&entry_path(ID, "audio.wav")).is_ok(),
            "the wav stays"
        );
        assert!(
            w.files.read(&entry_path(ID, "audio.m4a")).is_err(),
            "a partial m4a is removed"
        );
    }

    #[test]
    fn a_wav_that_cannot_be_removed_stays_beside_the_m4a() {
        let w = World::default();
        let (_, encode, _) = archived(&w);
        w.files.fail_remove.set(true);
        let _outcome = w.run_with(&encode, &w.library_ports());
        assert!(
            w.files.read(&entry_path(ID, "audio.wav")).is_ok(),
            "the wav stays"
        );
    }

    #[test]
    fn a_summary_replaces_the_excerpt() {
        let w = World::default();
        let (mut d, _, summary) = archived(&w);
        let mut entry = entry_json(&w, ID);
        let _: Option<Value> = entry
            .as_object_mut()
            .unwrap()
            .insert("x".to_string(), json!(1));
        put(
            &w,
            &entry_path(ID, "entry.json"),
            entry.to_string().as_bytes(),
        );
        w.summarizer
            .answers
            .borrow_mut()
            .push_back(Ok("\"A note about the board.\"".to_string()));
        let before = d.snapshot(&w.library_ports());
        let s = d.complete(w.run_with(&summary, &w.library_ports()), &w.library_ports());
        assert_eq!(
            *w.summarizer.transcripts.borrow(),
            ["Part 1."],
            "the transcript"
        );
        assert_eq!(
            entry_json(&w, ID),
            json!({
                "id": ID,
                "title": "2026-10-26 14:03:09",
                "summary": "A note about the board.",
                "summary_source": "ollama",
                "cleanup": "off",
                "duration_ms": 1000,
                "x": 1,
            }),
            "only the summary changed"
        );
        assert!(
            w.files.read(&entry_path(ID, "entry.json.partial")).is_err(),
            "no partial entry"
        );
        assert_eq!(
            s.library_revision,
            before.library_revision + 1,
            "the library changed"
        );
        assert_eq!(s.revision, before.revision + 1, "so did the snapshot");
    }

    #[test]
    fn a_summary_keeps_the_cleanup_keys() {
        let w = World::default();
        let (mut d, _, summary) = archived(&w);
        let mut entry = entry_json(&w, ID);
        let _: Option<Value> = entry
            .as_object_mut()
            .unwrap()
            .insert("cleanup_error".to_string(), json!("down"));
        put(
            &w,
            &entry_path(ID, "entry.json"),
            entry.to_string().as_bytes(),
        );
        let _: Snapshot = d.complete(w.run_with(&summary, &w.library_ports()), &w.library_ports());
        let after = entry_json(&w, ID);
        assert_eq!(
            after.get("summary_source"),
            Some(&json!("ollama")),
            "summarized"
        );
        assert_eq!(after.get("cleanup"), Some(&json!("off")), "cleanup kept");
        assert_eq!(
            after.get("cleanup_error"),
            Some(&json!("down")),
            "cleanup_error kept"
        );
    }

    #[test]
    fn a_failed_summary_keeps_the_excerpt() {
        for answer in [
            Err("connection refused".to_string()),
            Ok(String::new()),
            Ok("\"\"".to_string()),
        ] {
            let w = World::default();
            let (mut d, _, summary) = archived(&w);
            let original = w.files.read(&entry_path(ID, "entry.json")).unwrap();
            w.summarizer.answers.borrow_mut().push_back(answer.clone());
            let before = d.snapshot(&w.library_ports());
            let s = d.complete(w.run_with(&summary, &w.library_ports()), &w.library_ports());
            assert_eq!(s, before, "{answer:?}: nothing changed");
            assert_eq!(
                w.files.read(&entry_path(ID, "entry.json")).unwrap(),
                original,
                "{answer:?}: the entry is untouched"
            );
        }
    }

    #[test]
    fn a_summary_that_cannot_be_written_keeps_the_entry() {
        for (fail, entry) in [
            ("read", None),
            ("write", None),
            ("rename", None),
            ("none", Some(&b"[1]"[..])),
            ("none", Some(b"not json")),
        ] {
            let w = World::default();
            let (mut d, _, summary) = archived(&w);
            if let Some(bytes) = entry {
                put(&w, &entry_path(ID, "entry.json"), bytes);
            }
            let original = w.files.read(&entry_path(ID, "entry.json")).unwrap();
            match fail {
                "read" => w.files.fail_read.set(true),
                "write" => w.files.fail_write.set(true),
                "rename" => w.files.fail_rename.set(true),
                _ => {}
            }
            let before = d.snapshot(&w.library_ports());
            let s = d.complete(w.run_with(&summary, &w.library_ports()), &w.library_ports());
            w.files.fail_read.set(false);
            assert_eq!(s, before, "{fail} {entry:?}: nothing changed");
            assert_eq!(
                w.files.read(&entry_path(ID, "entry.json")).unwrap(),
                original,
                "{fail} {entry:?}: the entry is untouched"
            );
        }
    }

    #[test]
    fn a_summary_after_new_still_updates_its_entry() {
        let w = World::default();
        let (mut d, _, summary) = archived(&w);
        let p = w.library_ports();
        d.new_session(&p).unwrap();
        let before = d.snapshot(&p);
        let s = d.complete(w.run_with(&summary, &p), &p);
        assert_eq!(
            list(&w.files),
            [entry(ID, "2026-10-26 14:03:09", "A summary.")],
            "the entry is summarized"
        );
        assert_eq!(s.library_revision, before.library_revision + 1, "bumped");
        assert_eq!(s.phase, Phase::Ready, "the new session is untouched");
    }

    #[test]
    fn library_jobs_without_the_library_do_nothing() {
        let w = World::default();
        let (mut d, encode, summary) = archived(&w);
        let before = d.snapshot(&w.ports());
        for job in [&encode, &summary] {
            let s = d.complete(w.run(job), &w.ports());
            assert_eq!(s, before, "{job:?}: nothing changed");
        }
        assert!(w.encoder.calls.borrow().is_empty(), "no encode");
        assert!(w.summarizer.transcripts.borrow().is_empty(), "no summary");
        assert_eq!(
            entry_json(&w, ID).get("summary_source"),
            Some(&json!("excerpt")),
            "the excerpt stays"
        );
    }

    #[test]
    fn the_clock_is_read_once_per_session() {
        let w = World::default();
        w.clock.script(&[at(9, 0, 0), at(9, 0, 1), at(9, 0, 2)]);
        let mut d = Dictation::default();
        session(&w, &mut d, 3);
        assert_eq!(ids(&w), ["20261026-090000"], "titled at the first Record");
    }

    #[test]
    fn library_jobs_for_an_id_outside_the_library_do_nothing() {
        let w = World::default();
        // A clock past year 9999 gives an id that is not an entry's.
        w.clock.script(&[LocalTime {
            year: 10_000,
            ..at(9, 0, 0)
        }]);
        let (mut d, encode, summary) = archived(&w);
        let before = d.snapshot(&w.library_ports());
        for job in [&encode, &summary] {
            let s = d.complete(w.run_with(job, &w.library_ports()), &w.library_ports());
            assert_eq!(s, before, "{job:?}: nothing changed");
        }
        assert!(w.encoder.calls.borrow().is_empty(), "no encode");
        assert!(w.summarizer.transcripts.borrow().is_empty(), "no summary");
    }
}
