//! The speech model at launch: its warm-up when it is installed, and its
//! download when it is missing. Against fake ports.

#[expect(dead_code, reason = "these tests need no segment helpers")]
mod common;

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    use std::cell::RefCell;
    use std::sync::Mutex;

    use scriber_core::dictation::{
        DOWNLOADING, Dictation, DictationError, download_failed, model_missing, work,
    };
    use scriber_core::download::{MODEL_BYTES, MODEL_SHA256, MODEL_URL, PARTIAL, Source};
    use scriber_core::model::{Action, Download, Phase};
    use scriber_core::store::MODEL;

    use crate::common::World;

    const PATH: &str = "/models/m.bin";

    /// A failure: its reason, the source, and how to break the world.
    type Case = (&'static str, Source, fn(&World));

    /// A world whose model is missing, with a network serving `blocks`.
    fn missing(blocks: &[&[u8]]) -> World {
        let w = World::default();
        *w.transcriber.missing.borrow_mut() = Some(PATH.to_string());
        *w.downloader.blocks.borrow_mut() = blocks.iter().map(|b| b.to_vec()).collect();
        w
    }

    /// A source that `blocks`, joined, satisfy.
    fn source(blocks: &[&[u8]]) -> Source {
        let body = blocks.concat();
        Source {
            url: "https://example.test/m.bin".to_string(),
            bytes: u64::try_from(body.len()).unwrap(),
            sha256: format!("{:x}", Sha256::digest(&body)),
        }
    }

    /// Launches against `source` and runs the download it queued.
    fn launch_and_download(w: &World, source: Source) -> Dictation {
        let p = w.ports();
        let mut d = Dictation::launch_from(source, &p);
        let job = w.queue.jobs.borrow_mut().remove(0);
        let _snapshot = d.complete(w.run(&job), &p);
        d
    }

    fn file(w: &World, path: &str) -> Option<Vec<u8>> {
        w.files.files.borrow().get(path).cloned()
    }

    #[test]
    fn the_pinned_model() {
        assert_eq!(
            (MODEL_URL, MODEL_BYTES, MODEL_SHA256, PARTIAL),
            (
                "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin",
                1_624_555_275,
                "1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69",
                "models/ggml-large-v3-turbo.bin.part",
            ),
            "url, size, sha256, partial path"
        );
        assert_eq!(
            Source::default(),
            Source {
                url: MODEL_URL.to_string(),
                bytes: MODEL_BYTES,
                sha256: MODEL_SHA256.to_string(),
            },
            "the default source is the model"
        );
    }

    #[test]
    fn launch_queues_the_warm_up_and_otherwise_recovers_like_recover() {
        let w = World::default();
        let p = w.ports();
        let d = Dictation::launch(&p);
        assert_eq!(
            d.snapshot(&p),
            Dictation::recover(&p).snapshot(&p),
            "the same state as recover"
        );
        let jobs: Vec<_> = w.queue.jobs.borrow_mut().drain(..).collect();
        assert_eq!(jobs.len(), 1, "one job: the warm-up");
        let _outcome = w.run(&jobs[0]);
        assert_eq!(w.transcriber.warms.get(), 1, "it loads the model");
        assert!(
            w.transcriber.calls.borrow().is_empty(),
            "and transcribes nothing"
        );
        assert!(w.downloader.urls.borrow().is_empty(), "nothing downloaded");
    }

    #[test]
    fn launch_finds_the_last_session_on_disk() {
        let w = World::default();
        let p = w.ports();
        let mut d = Dictation::launch(&p);
        d.new_session(&p).unwrap();
        d.record(&p).unwrap();
        let _job = d.pause(&p).unwrap();
        let relaunched = Dictation::launch(&p).snapshot(&p);
        assert_eq!(
            (relaunched.phase, relaunched.segments),
            (Phase::Failed, 1),
            "the unfinished session is offered for Retry"
        );
    }

    #[test]
    fn a_queue_that_refuses_the_warm_up_changes_nothing() {
        let w = World::default();
        w.queue.fail.set(true);
        let p = w.ports();
        assert_eq!(
            Dictation::launch(&p).snapshot(&p),
            Dictation::recover(&p).snapshot(&p),
            "launched all the same"
        );
    }

    #[test]
    fn the_warm_up_outcome_never_touches_a_session_whether_or_not_it_loaded() {
        for error in [None, Some("the model is corrupt".to_string())] {
            let w = World::default();
            *w.transcriber.warm_error.borrow_mut() = error.clone();
            let p = w.ports();
            let mut d = Dictation::launch(&p);
            let warm = w.queue.jobs.borrow_mut().remove(0);
            d.new_session(&p).unwrap();
            d.record(&p).unwrap();
            let _segment = d.pause(&p).unwrap();
            let before = d.snapshot(&p);
            assert_eq!(before.pending, 1, "a segment is pending");
            let after = d.complete(w.run(&warm), &p);
            assert_eq!(
                after, before,
                "{error:?}: nothing changed, not even the revision"
            );
        }
    }

    #[test]
    fn a_missing_model_downloads_the_pinned_model_at_launch() {
        let w = missing(&[]);
        let p = w.ports();
        let d = Dictation::launch(&p);
        let s = d.snapshot(&p);
        assert_eq!(
            (s.model_error, s.download),
            (
                None,
                Some(Download {
                    received_bytes: 0,
                    total_bytes: MODEL_BYTES
                })
            ),
            "downloading, not an error"
        );
        let job = w.queue.jobs.borrow_mut().remove(0);
        let _outcome = w.run(&job);
        assert_eq!(
            *w.downloader.urls.borrow(),
            [MODEL_URL],
            "from the pinned URL"
        );
        assert_eq!(w.transcriber.warms.get(), 0, "not warmed yet");
    }

    #[test]
    fn record_waits_for_the_download() {
        let w = missing(&[]);
        let p = w.ports();
        let mut d = Dictation::launch(&p);
        d.new_session(&p).unwrap();
        assert_eq!(
            d.record(&p),
            Err(DictationError::ModelMissing(DOWNLOADING.to_string())),
            "refused while downloading"
        );
        assert_eq!(
            DOWNLOADING, "The speech model is still downloading.",
            "sentence"
        );
        assert!(
            w.recorder.calls.borrow().is_empty(),
            "the microphone never started"
        );
    }

    #[test]
    fn a_verified_download_is_renamed_into_place_and_warmed() {
        let blocks: &[&[u8]] = &[b"hello", b" ", b"world"];
        let w = missing(blocks);
        w.downloader.interrupt.set(true);
        let p = w.ports();
        let mut d = Dictation::launch_from(source(blocks), &p);
        let before = d.snapshot(&p).revision;
        let job = w.queue.jobs.borrow_mut().remove(0);
        let outcome = w.run(&job);
        assert_eq!(
            file(&w, MODEL).as_deref(),
            Some(b"hello world".as_slice()),
            "the model is in place"
        );
        assert_eq!(file(&w, PARTIAL), None, "the partial file is gone");
        *w.transcriber.missing.borrow_mut() = None;
        let s = d.complete(outcome, &p);
        assert_eq!(
            (s.model_error, s.download, s.notice),
            (None, None, None),
            "ready, with no notice"
        );
        assert_eq!(s.revision, before + 1, "a change");
        let warm = w.queue.jobs.borrow_mut().remove(0);
        let _warmed = w.run(&warm);
        assert_eq!(w.transcriber.warms.get(), 1, "then warmed up");
        d.new_session(&p).unwrap();
        assert_eq!(d.record(&p), Ok(()), "and Record works");
    }

    #[test]
    fn progress_is_reported_once_per_whole_percent() {
        let body = [b'x'; 300];
        let blocks: Vec<&[u8]> = body.chunks(1).collect();
        let w = missing(&blocks);
        let _d = launch_and_download(&w, source(&blocks));
        let want: Vec<u64> = (1..=100).map(|n| n * 3).collect();
        assert_eq!(*w.progress.borrow(), want, "every third byte is a percent");
    }

    #[test]
    fn progress_moves_the_snapshot_only_while_downloading() {
        let w = missing(&[]);
        let p = w.ports();
        let mut d = Dictation::launch_from(source(&[b"abcd"]), &p);
        let before = d.snapshot(&p).revision;
        let s = d.progress(2, &p);
        assert_eq!(
            (s.download, s.revision),
            (
                Some(Download {
                    received_bytes: 2,
                    total_bytes: 4
                }),
                before + 1
            ),
            "moved"
        );
        let job = w.queue.jobs.borrow_mut().remove(0);
        let failed = d.complete(w.run(&job), &p);
        let late = d.progress(3, &p);
        assert_eq!(late, failed, "ignored once the download is over");
    }

    #[test]
    fn a_failed_download_leaves_no_partial_file_and_offers_a_retry() {
        let hello: &[&[u8]] = &[b"hello", b" ", b"world"];
        let other = Source {
            sha256: "0".repeat(64),
            ..source(hello)
        };
        let longer = Source {
            bytes: 12,
            ..source(hello)
        };
        let shorter = Source {
            bytes: 10,
            ..source(hello)
        };
        let cases: [Case; 8] = [
            ("http status 404", source(hello), |w| {
                *w.downloader.refuse.borrow_mut() = Some("http status 404".to_string());
            }),
            ("the connection was reset", source(hello), |w| {
                *w.downloader.error.borrow_mut() = Some("the connection was reset".to_string());
            }),
            ("it stopped after 11 of 12 bytes", longer, |_| {}),
            ("it is larger than expected", shorter, |_| {}),
            ("its checksum does not match", other, |_| {}),
            ("disk full", source(hello), |w| {
                w.files.fail_append.set(true);
            }),
            ("disk full", source(hello), |w| w.files.fail_write.set(true)),
            ("read-only volume", source(hello), |w| {
                w.files.fail_rename.set(true);
            }),
        ];
        for (reason, src, setup) in cases {
            let w = missing(hello);
            setup(&w);
            let p = w.ports();
            let d = launch_and_download(&w, src);
            let s = d.snapshot(&p);
            let want = download_failed(reason);
            assert_eq!(
                (s.model_error.as_deref(), s.download),
                (Some(want.as_str()), None),
                "{reason}"
            );
            assert_eq!(file(&w, PARTIAL), None, "{reason}: no partial file");
            assert_eq!(file(&w, MODEL), None, "{reason}: no model");
        }
        assert_eq!(
            download_failed("x"),
            "Could not download the speech model: x.",
            "sentence"
        );
    }

    #[test]
    fn retry_downloads_again_after_a_failure_and_record_says_why_until_then() {
        let hello: &[&[u8]] = &[b"hello world"];
        let w = missing(hello);
        *w.downloader.error.borrow_mut() = Some("offline".to_string());
        let p = w.ports();
        let mut d = launch_and_download(&w, source(hello));
        d.new_session(&p).unwrap();
        assert_eq!(
            d.record(&p),
            Err(DictationError::ModelMissing(download_failed("offline"))),
            "record says why"
        );
        *w.downloader.error.borrow_mut() = None;
        let s = d.apply(Action::DownloadModel, "", &p).unwrap();
        assert_eq!(
            (s.model_error, s.download.map(|got| got.received_bytes)),
            (None, Some(0)),
            "downloading again"
        );
        assert_eq!(
            d.apply(Action::DownloadModel, "", &p),
            Err(DictationError::NoDownload),
            "not twice at once"
        );
        let job = w.queue.jobs.borrow_mut().remove(0);
        let outcome = w.run(&job);
        assert_eq!(
            file(&w, MODEL).as_deref(),
            Some(b"hello world".as_slice()),
            "in place"
        );
        let done = d.complete(outcome.clone(), &p);
        assert_eq!(done.download, None, "done");
        assert_eq!(d.complete(outcome, &p), done, "a second outcome is ignored");
    }

    #[test]
    fn download_is_refused_while_the_model_is_installed() {
        let w = World::default();
        let p = w.ports();
        let mut d = Dictation::launch(&p);
        let before = d.snapshot(&p);
        let e = d.apply(Action::DownloadModel, "", &p).unwrap_err();
        assert_eq!(e, DictationError::NoDownload, "refused");
        assert_eq!(
            e.to_string(),
            "The speech model is already installed or downloading.",
            "sentence"
        );
        assert_eq!(d.snapshot(&p), before, "nothing changed");
        assert_eq!(
            Action::DownloadModel.to_string(),
            "download the speech model",
            "name"
        );
    }

    #[test]
    fn a_queue_that_refuses_the_download_fails_it_at_once() {
        let w = missing(&[]);
        w.queue.fail.set(true);
        let p = w.ports();
        let s = Dictation::launch(&p).snapshot(&p);
        assert_eq!(
            s.model_error,
            Some(download_failed(
                "it could not be queued: the worker stopped"
            )),
            "offered again"
        );
    }

    #[test]
    fn a_missing_model_that_is_not_downloading_says_where_it_belongs() {
        let w = missing(&[]);
        let p = w.ports();
        let d = Dictation::recover(&p);
        assert_eq!(
            d.snapshot(&p).model_error,
            Some(model_missing(PATH)),
            "the model-missing state, not a crash"
        );
    }

    #[test]
    fn work_reports_each_progress_snapshot_then_the_outcome() {
        let body = [b'x'; 200];
        let blocks: Vec<&[u8]> = body.chunks(50).collect();
        let w = missing(&blocks);
        let p = w.ports();
        let shared = Mutex::new(Dictation::launch_from(source(&blocks), &p));
        let job = w.queue.jobs.borrow_mut().remove(0);
        let reports = RefCell::new(Vec::new());
        work(&shared, &job, &p, &|s| reports.borrow_mut().push(s));
        let received: Vec<_> = reports
            .borrow()
            .iter()
            .map(|s| s.download.map(|d| d.received_bytes))
            .collect();
        assert_eq!(
            received,
            [Some(50), Some(100), Some(150), Some(200), None],
            "each quarter, then done"
        );
        assert_eq!(
            reports.borrow().last(),
            Some(&shared.lock().unwrap().snapshot(&p)),
            "the last report is the session as it stands"
        );
    }

    #[test]
    fn launch_local_with_the_model_installed_queues_only_the_warm_up() {
        let w = World::default();
        let p = w.ports();
        let d = Dictation::launch_local(&p);
        assert_eq!(
            d.snapshot(&p),
            Dictation::recover(&p).snapshot(&p),
            "the same state as recover"
        );
        let jobs: Vec<_> = w.queue.jobs.borrow_mut().drain(..).collect();
        assert_eq!(jobs.len(), 1, "one job: the warm-up");
        let _outcome = w.run(&jobs[0]);
        assert_eq!(w.transcriber.warms.get(), 1, "it loads the model");
        assert!(w.downloader.urls.borrow().is_empty(), "nothing downloaded");
    }

    #[test]
    fn launch_local_refuses_a_download_as_local_even_when_installed() {
        let w = World::default();
        let p = w.ports();
        let mut d = Dictation::launch_local(&p);
        assert_eq!(
            d.apply(Action::DownloadModel, "", &p),
            Err(DictationError::LocalModel),
            "local, not NoDownload"
        );
    }

    #[test]
    fn launch_local_with_the_model_missing_queues_nothing() {
        let w = missing(&[]);
        let p = w.ports();
        let mut d = Dictation::launch_local(&p);
        assert!(w.queue.jobs.borrow().is_empty(), "no job queued");
        let s = d.snapshot(&p);
        assert_eq!(
            (s.model_error, s.download),
            (Some(model_missing(PATH)), None),
            "says where the model belongs, and does not download it"
        );
        assert_eq!(
            d.apply(Action::DownloadModel, "", &p),
            Err(DictationError::LocalModel),
            "refused"
        );
        assert!(w.queue.jobs.borrow().is_empty(), "still no job");
    }

    #[test]
    fn a_recovered_session_has_no_download_source() {
        let w = missing(&[]);
        let p = w.ports();
        assert_eq!(
            Dictation::recover(&p).apply(Action::DownloadModel, "", &p),
            Err(DictationError::LocalModel),
            "refused"
        );
    }

    #[test]
    fn the_local_model_sentence() {
        assert_eq!(
            DictationError::LocalModel.to_string(),
            "The speech model is a local file, so there is nothing to download.",
            "sentence"
        );
    }
}
