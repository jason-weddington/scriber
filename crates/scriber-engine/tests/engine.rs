//! The engine over fake adapters: sessions, waiting, the model's download,
//! recovery and the threading around them. No microphone, model or network.

mod common;

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::path::Path;
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex, mpsc};
    use std::thread;
    use std::time::{Duration, Instant};

    use scriber_engine::{
        Action, Clipboard, DictationError, Download, Engine, EngineConfig, EngineError, Event,
        ModelSource, Phase, PortError, Source, level_sink,
    };
    use tempfile::TempDir;

    use crate::common::{FakeDownloader, FakeTranscriber, Fakes, PATIENCE, events, until};

    /// The SHA-256 of b"abcd".
    const ABCD: &str = "88d4266fd4e6338d13b845fcf289579d209c897823b9217da3e161936f031589";

    fn config(dir: &TempDir) -> EngineConfig {
        EngineConfig::new(dir.path())
    }

    fn write(dir: &TempDir, path: &str, bytes: &[u8]) {
        let file = dir.path().join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, bytes).unwrap();
    }

    #[test]
    fn the_engine_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Engine>();
    }

    #[test]
    fn the_default_model_is_the_pinned_download() {
        assert_eq!(
            ModelSource::default(),
            ModelSource::Download(Source::default()),
            "large-v3-turbo from Hugging Face"
        );
        let c = EngineConfig::new("/data");
        assert_eq!(
            (c.data_dir, c.model, c.clipboard.is_none()),
            (
                Path::new("/data").to_path_buf(),
                ModelSource::default(),
                true
            ),
            "the default model and no clipboard"
        );
    }

    #[test]
    fn the_error_sentences() {
        assert_eq!(
            [
                EngineError::Refused(DictationError::NoDownload).to_string(),
                EngineError::Failed("it broke".into()).to_string(),
                EngineError::Timeout.to_string(),
                EngineError::NotStopped.to_string(),
            ],
            [
                "The speech model is already installed or downloading.",
                "it broke",
                "The transcript was not ready in time.",
                "There is no stopped session to wait for.",
            ],
            "sentences"
        );
    }

    #[test]
    fn record_then_stop_answers_with_the_transcript() {
        let dir = TempDir::new().unwrap();
        let fakes = Fakes::default();
        let (engine, rx) = fakes.open(config(&dir));
        let _recording = engine.record().unwrap();
        assert_eq!(
            engine.stop_and_wait(PATIENCE),
            Ok("hello world".to_string()),
            "the text"
        );
        assert_eq!(
            (
                fakes.recorder.starts.load(Ordering::SeqCst),
                fakes.recorder.stops.load(Ordering::SeqCst)
            ),
            (1, 1),
            "one start, one stop"
        );
        let s = engine.state();
        assert_eq!(s.segments, 1, "one segment");
        assert_eq!(
            s.notice,
            Some("Copied to the clipboard.".to_string()),
            "the notice, even with no clipboard"
        );
        let finished = until(&rx, |f| f.phase == Phase::Finished).unwrap();
        assert_eq!(finished.text, "hello world", "reported as an event");
    }

    #[test]
    fn a_given_clipboard_gets_the_transcript() {
        struct Board(Mutex<Vec<String>>);
        impl Clipboard for Board {
            fn write(&self, text: &str) -> Result<(), PortError> {
                self.0.lock().unwrap().push(text.to_string());
                Ok(())
            }
        }
        let dir = TempDir::new().unwrap();
        let board = Arc::new(Board(Mutex::default()));
        let mut c = config(&dir);
        let shared: Arc<dyn Clipboard + Send + Sync> = board.clone();
        c.clipboard = Some(shared);
        let (engine, _rx) = Fakes::default().open(c);
        let _recording = engine.record().unwrap();
        let _text = engine.stop_and_wait(PATIENCE).unwrap();
        assert_eq!(
            *board.0.lock().unwrap(),
            ["hello world"],
            "copied to the given clipboard"
        );
    }

    #[test]
    fn pause_and_resume_join_the_segments() {
        let dir = TempDir::new().unwrap();
        let fakes = Fakes::with(FakeTranscriber::answering(&[Ok("one"), Ok("two")]));
        let (engine, _rx) = fakes.open(config(&dir));
        assert_eq!(engine.record().unwrap().phase, Phase::Recording, "record");
        assert_eq!(engine.pause().unwrap().phase, Phase::Paused, "pause");
        assert_eq!(engine.resume().unwrap().phase, Phase::Recording, "resume");
        assert_eq!(engine.stop().unwrap().phase, Phase::Transcribing, "stop");
        assert_eq!(
            engine.wait(PATIENCE),
            Ok("one\n\ntwo".to_string()),
            "two paragraphs"
        );
    }

    #[test]
    fn record_from_finished_starts_a_new_session() {
        let dir = TempDir::new().unwrap();
        let (engine, _rx) = Fakes::default().open(config(&dir));
        let _recording = engine.record().unwrap();
        let _text = engine.stop_and_wait(PATIENCE).unwrap();
        let s = engine.record().unwrap();
        assert_eq!(
            (s.phase, s.segments),
            (Phase::Recording, 0),
            "a new session"
        );
    }

    #[test]
    fn wait_needs_a_stopped_session_and_times_out() {
        let dir = TempDir::new().unwrap();
        let (transcriber, _gate) = FakeTranscriber::default().gated();
        let (engine, _rx) = Fakes::with(transcriber).open(config(&dir));
        let _ready = engine.act(Action::New, "").unwrap();
        assert_eq!(engine.wait(PATIENCE), Err(EngineError::NotStopped), "ready");
        let _recording = engine.record().unwrap();
        assert_eq!(
            engine.wait(PATIENCE),
            Err(EngineError::NotStopped),
            "recording"
        );
        assert_eq!(
            engine.stop().unwrap().phase,
            Phase::Transcribing,
            "transcribing"
        );
        let started = Instant::now();
        assert_eq!(
            engine.wait(Duration::from_millis(100)),
            Err(EngineError::Timeout),
            "not in time"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "and returns at the deadline"
        );
    }

    #[test]
    fn a_failed_session_is_reported_and_can_be_retried() {
        let dir = TempDir::new().unwrap();
        let fakes = Fakes::with(FakeTranscriber::answering(&[Err("boom")]));
        let (engine, _rx) = fakes.open(config(&dir));
        let _recording = engine.record().unwrap();
        let _stopped = engine.stop().unwrap();
        assert_eq!(
            engine.wait(PATIENCE),
            Err(EngineError::Failed(
                "Part 1 of 1 could not be transcribed: boom. Retry to try again.".into()
            )),
            "the session's error"
        );
        assert_eq!(
            engine.retry().unwrap().phase,
            Phase::Transcribing,
            "retried"
        );
        assert_eq!(
            engine.wait(PATIENCE),
            Ok("hello world".to_string()),
            "and transcribed"
        );
    }

    #[test]
    fn discard_stops_the_recording_and_empties_the_session() {
        let dir = TempDir::new().unwrap();
        let fakes = Fakes::default();
        let (engine, _rx) = fakes.open(config(&dir));
        let _recording = engine.record().unwrap();
        let s = engine.discard().unwrap();
        assert_eq!((s.phase, s.segments), (Phase::Ready, 0), "empty and ready");
        assert_eq!(fakes.recorder.stops.load(Ordering::SeqCst), 1, "stopped");
    }

    /// Fakes that download b"abcd" into `dir`'s model path.
    fn downloading(dir: &TempDir) -> (Fakes, EngineConfig) {
        let fakes = Fakes {
            transcriber: FakeTranscriber {
                model: Some(dir.path().join("models/ggml-large-v3-turbo.bin")),
                ..FakeTranscriber::default()
            },
            downloader: FakeDownloader {
                body: b"abcd".to_vec(),
                ..FakeDownloader::default()
            },
            ..Fakes::default()
        };
        let mut c = config(dir);
        c.model = ModelSource::Download(Source {
            url: "fake://model".into(),
            bytes: 4,
            sha256: ABCD.into(),
        });
        (fakes, c)
    }

    #[test]
    fn a_missing_model_downloads_at_launch_and_warms_up() {
        let dir = TempDir::new().unwrap();
        let (fakes, c) = downloading(&dir);
        let (engine, rx) = fakes.open(c);
        let _progress = until(&rx, |s| {
            matches!(s.download, Some(Download { total_bytes: 4, .. }))
        })
        .unwrap();
        let _done = until(&rx, |s| s.download.is_none()).unwrap();
        // The warm-up it queued reports when it is done.
        let _warm = until(&rx, |_| true).unwrap();
        let s = engine.state();
        assert_eq!((s.download, s.model_error), (None, None), "installed");
        assert_eq!(
            std::fs::read(dir.path().join("models/ggml-large-v3-turbo.bin")).unwrap(),
            b"abcd",
            "the verified model"
        );
        assert_eq!(fakes.transcriber.warms.load(Ordering::SeqCst), 1, "warmed");
        assert_eq!(
            fakes.downloader.opens.load(Ordering::SeqCst),
            1,
            "one request"
        );
        assert_eq!(engine.record().unwrap().phase, Phase::Recording, "usable");
    }

    #[test]
    fn the_launch_download_never_races_the_launch() {
        for round in 0..20 {
            let dir = TempDir::new().unwrap();
            let (fakes, c) = downloading(&dir);
            let (engine, rx) = fakes.open(c);
            let s = until(&rx, |s| s.download.is_none()).unwrap();
            assert_eq!(
                (s.download, s.model_error),
                (None, None),
                "round {round}: installed"
            );
            drop(engine);
        }
    }

    #[test]
    fn a_local_model_is_never_downloaded() {
        let dir = TempDir::new().unwrap();
        let absent = dir.path().join("absent.bin");
        let fakes = Fakes::with(FakeTranscriber {
            model: Some(absent.clone()),
            ..FakeTranscriber::default()
        });
        let mut c = config(&dir);
        c.model = ModelSource::Local(absent.clone());
        let (engine, _rx) = fakes.open(c);
        let missing = format!("The speech model is missing from {}.", absent.display());
        assert_eq!(engine.state().model_error, Some(missing.clone()), "missing");
        assert_eq!(
            engine.download_model(),
            Err(EngineError::Refused(DictationError::LocalModel)),
            "nothing to download"
        );
        assert_eq!(
            engine.record(),
            Err(EngineError::Refused(DictationError::ModelMissing(missing))),
            "cannot record"
        );
        assert_eq!(engine.state().phase, Phase::Ready, "left ready");
        assert_eq!(
            fakes.downloader.opens.load(Ordering::SeqCst),
            0,
            "no request"
        );
    }

    #[test]
    fn the_recorder_reports_the_level_through_the_events() {
        let dir = TempDir::new().unwrap();
        let (engine, rx) = Fakes::default().open(config(&dir));
        let _recording = engine.record().unwrap();
        let deadline = Instant::now() + PATIENCE;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if rx.recv_timeout(left).unwrap() == Event::Level(0.25) {
                break;
            }
        }
        let (tx, levels) = mpsc::channel();
        level_sink(events(Mutex::new(tx)))(0.5);
        assert_eq!(
            levels.try_iter().collect::<Vec<_>>(),
            [Event::Level(0.5)],
            "one level"
        );
    }

    #[test]
    fn a_finished_session_is_recovered_until_the_next_record() {
        let dir = TempDir::new().unwrap();
        write(&dir, "segments/transcript.txt", b"old text");
        write(&dir, "segments/segment-0000.wav", b"RIFF");
        let fakes = Fakes::with(FakeTranscriber::answering(&[Ok("new")]));
        let (engine, _rx) = fakes.open(config(&dir));
        assert_eq!(engine.state().phase, Phase::Finished, "finished");
        assert_eq!(
            engine.wait(Duration::ZERO),
            Ok("old text".to_string()),
            "the old text"
        );
        let _recording = engine.record().unwrap();
        assert_eq!(
            engine.stop_and_wait(PATIENCE),
            Ok("new".to_string()),
            "the new text"
        );
    }

    #[test]
    fn record_never_wipes_a_failed_session() {
        let dir = TempDir::new().unwrap();
        write(&dir, "segments/segment-0000.wav", b"RIFF");
        let (engine, _rx) = Fakes::default().open(config(&dir));
        assert_eq!(engine.state().phase, Phase::Failed, "failed");
        assert_eq!(
            engine.record(),
            Err(EngineError::Refused(DictationError::Illegal {
                action: Action::Record,
                phase: Phase::Failed,
            })),
            "refused"
        );
        assert!(
            dir.path().join("segments/segment-0000.wav").is_file(),
            "the audio is kept"
        );
    }

    #[test]
    fn concurrent_waiters_all_get_the_transcript() {
        let dir = TempDir::new().unwrap();
        let (transcriber, gate) = FakeTranscriber::default().gated();
        let (engine, _rx) = Fakes::with(transcriber).open(config(&dir));
        let _recording = engine.record().unwrap();
        let _stopped = engine.stop().unwrap();
        let waiters: Vec<_> = (0..2)
            .map(|_| {
                let engine = engine.clone();
                thread::spawn(move || engine.wait(PATIENCE))
            })
            .collect();
        // Both are waiting before the transcript arrives, so only the
        // notification can wake them before the timeout.
        thread::sleep(Duration::from_millis(200));
        let started = Instant::now();
        gate.send(()).unwrap();
        for waiter in waiters {
            assert_eq!(
                waiter.join().unwrap(),
                Ok("hello world".to_string()),
                "each waiter"
            );
        }
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "woken, not timed out"
        );
    }

    #[test]
    fn dropping_the_last_handle_drops_the_adapters() {
        let dir = TempDir::new().unwrap();
        let fakes = Fakes::default();
        let (engine, rx) = fakes.open(config(&dir));
        let _warm = until(&rx, |_| true).unwrap();
        let second = engine.clone();
        drop(engine);
        assert!(!fakes.transcriber.dropped.is_set(), "a handle remains");
        drop(second);
        let deadline = Instant::now() + PATIENCE;
        while !fakes.transcriber.dropped.is_set() {
            assert!(Instant::now() < deadline, "dropped in time");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn open_with_a_missing_local_model_touches_nothing() {
        let dir = TempDir::new().unwrap();
        let absent = dir.path().join("absent.bin");
        let mut c = config(&dir);
        c.model = ModelSource::Local(absent.clone());
        let engine = Engine::open(c, Arc::new(|_| {}));
        let s = engine.state();
        assert_eq!(
            (s.phase, s.model_error),
            (
                Phase::Idle,
                Some(format!(
                    "The speech model is missing from {}.",
                    absent.display()
                ))
            ),
            "idle, the model missing"
        );
    }

    #[test]
    fn open_downloads_over_http_and_reports_a_failure() {
        let dir = TempDir::new().unwrap();
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let mut c = config(&dir);
        c.model = ModelSource::Download(Source {
            url: format!("http://127.0.0.1:{port}/m"),
            bytes: 4,
            sha256: ABCD.into(),
        });
        let (tx, rx) = mpsc::channel();
        let _engine = Engine::open(c, events(Mutex::new(tx)));
        let failed = until(&rx, |s| s.model_error.is_some()).unwrap();
        let error = failed.model_error.unwrap();
        assert!(
            error.starts_with("Could not download the speech model: "),
            "{error}"
        );
    }
}
