//! The engine over fake adapters: sessions, waiting, the model's download,
//! recovery and the threading around them. No microphone, model or network.

mod common;

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex, mpsc};
    use std::thread;
    use std::time::{Duration, Instant};

    use scriber_engine::{
        Action, Clipboard, DEFAULT_OLLAMA_URL, DEFAULT_SUMMARY_MODEL, DictationError, Download,
        Engine, EngineConfig, EngineError, Event, LibraryEntry, ModelSource, Phase, PortError,
        Settings, ShutdownReport, Source, level_sink,
    };
    use tempfile::TempDir;

    use crate::common::{
        FakeDownloader, FakeSummarizer, FakeTranscriber, Fakes, PATIENCE, RecordingClipboard,
        events, until,
    };

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
                EngineError::ShutDown.to_string(),
            ],
            [
                "The speech model is already installed or downloading.",
                "it broke",
                "The transcript was not ready in time.",
                "There is no stopped session to wait for.",
                "The engine has shut down.",
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

    /// Waits, within [`PATIENCE`], until `count` reaches `want`.
    fn reaches(count: &AtomicU32, want: u32) {
        let started = Instant::now();
        while count.load(Ordering::SeqCst) != want {
            assert!(started.elapsed() < PATIENCE, "reached {want} in time");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn shutdown_frees_the_model_once_and_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let fakes = Fakes::default();
        let (engine, _rx) = fakes.open(config(&dir));
        reaches(&fakes.transcriber.warms, 1);
        let clone = engine.clone();
        let later = ShutdownReport {
            first: false,
            paused_recording: false,
            freed_model: false,
        };
        assert_eq!(
            engine.shutdown(),
            ShutdownReport {
                first: true,
                paused_recording: false,
                freed_model: true,
            },
            "the first call frees the model"
        );
        assert_eq!(engine.shutdown(), later, "a second call");
        assert_eq!(clone.shutdown(), later, "a call on a clone");
        assert_eq!(
            fakes.transcriber.unloads.load(Ordering::SeqCst),
            1,
            "freed once"
        );
    }

    #[test]
    fn actions_after_shutdown_are_refused() {
        let dir = TempDir::new().unwrap();
        let fakes = Fakes::default();
        let (engine, _rx) = fakes.open(config(&dir));
        reaches(&fakes.transcriber.warms, 1);
        let _ready = engine.act(Action::New, "").unwrap();
        let before = engine.state();
        let _report = engine.shutdown();
        let refused = Err(EngineError::ShutDown);
        assert_eq!(engine.record(), refused, "record");
        assert_eq!(engine.pause(), refused, "pause");
        assert_eq!(engine.resume(), refused, "resume");
        assert_eq!(engine.stop(), refused, "stop");
        assert_eq!(engine.discard(), refused, "discard");
        assert_eq!(engine.retry(), refused, "retry");
        assert_eq!(engine.download_model(), refused, "download");
        assert_eq!(engine.act(Action::Copy, "x"), refused, "copy");
        assert_eq!(
            engine.wait(Duration::ZERO),
            Err(EngineError::ShutDown),
            "wait"
        );
        assert_eq!(
            engine.stop_and_wait(Duration::ZERO),
            Err(EngineError::ShutDown),
            "stop and wait"
        );
        assert_eq!(engine.state(), before, "unchanged, and still answers");
    }

    #[test]
    fn shutdown_while_recording_keeps_the_audio() {
        let dir = TempDir::new().unwrap();
        let fakes = Fakes::default();
        let (engine, _rx) = fakes.open(config(&dir));
        reaches(&fakes.transcriber.warms, 1);
        let _recording = engine.record().unwrap();
        let asked = fakes.transcriber.asks.load(Ordering::SeqCst);
        assert_eq!(
            engine.shutdown(),
            ShutdownReport {
                first: true,
                paused_recording: true,
                freed_model: true,
            },
            "paused, then freed"
        );
        assert_eq!(
            fakes.recorder.stops.load(Ordering::SeqCst),
            1,
            "the microphone stopped"
        );
        let s = engine.state();
        assert_eq!(
            (s.phase, s.segments, s.pending),
            (Phase::Paused, 1, 0),
            "paused, its segment's job refused"
        );
        assert!(
            dir.path().join("segments/segment-0000.wav").is_file(),
            "the audio is kept"
        );
        thread::sleep(Duration::from_millis(200));
        assert_eq!(
            fakes.transcriber.asks.load(Ordering::SeqCst),
            asked,
            "nothing transcribed"
        );
        let (relaunched, _events) = Fakes::default().open(config(&dir));
        assert_eq!(
            relaunched.state().phase,
            Phase::Failed,
            "the next launch offers a retry"
        );
    }

    #[test]
    fn shutdown_waits_for_a_transcription_in_flight() {
        let dir = TempDir::new().unwrap();
        let (transcriber, entered) = FakeTranscriber::default().entering();
        let (transcriber, gate) = transcriber.gated();
        let fakes = Fakes::with(transcriber);
        let (engine, rx) = fakes.open(config(&dir));
        reaches(&fakes.transcriber.warms, 1);
        let _recording = engine.record().unwrap();
        let _stopped = engine.stop().unwrap();
        entered.recv_timeout(PATIENCE).unwrap();
        let (report_tx, report_rx) = mpsc::channel();
        let shutting = {
            let engine = engine.clone();
            thread::spawn(move || report_tx.send(engine.shutdown()).unwrap())
        };
        thread::sleep(Duration::from_millis(200));
        assert!(report_rx.try_recv().is_err(), "shutdown waits");
        assert_eq!(
            fakes.transcriber.unloads.load(Ordering::SeqCst),
            0,
            "the model is still loaded"
        );
        gate.send(()).unwrap();
        let report = report_rx.recv_timeout(PATIENCE).unwrap();
        assert!(report.freed_model, "then frees it");
        assert_eq!(
            fakes.transcriber.unloads.load(Ordering::SeqCst),
            1,
            "freed once"
        );
        let _finished = until(&rx, |s| s.phase == Phase::Finished).unwrap();
        shutting.join().unwrap();
    }

    #[test]
    fn a_download_finishing_after_shutdown_never_warms() {
        let dir = TempDir::new().unwrap();
        let (mut fakes, c) = downloading(&dir);
        let (downloader, gate) = fakes.downloader.clone().gated();
        fakes.downloader = downloader;
        let (engine, rx) = fakes.open(c);
        // No event arrives before the gated request answers, so wait for
        // the request itself.
        reaches(&fakes.downloader.opens, 1);
        assert!(engine.state().download.is_some(), "downloading");
        let report = engine.shutdown();
        assert!(!report.freed_model, "nothing was loaded");
        gate.send(()).unwrap();
        let _done = until(&rx, |s| s.download.is_none()).unwrap();
        thread::sleep(Duration::from_millis(200));
        let t = &fakes.transcriber;
        assert_eq!(
            (
                t.asks.load(Ordering::SeqCst),
                t.warms.load(Ordering::SeqCst),
                t.unloads.load(Ordering::SeqCst)
            ),
            (0, 0, 0),
            "never warmed, never loaded"
        );
    }

    fn library_config(dir: &TempDir) -> EngineConfig {
        EngineConfig {
            library: true,
            ..config(dir)
        }
    }

    #[test]
    fn the_library_is_off_by_default_with_ollama_defaults() {
        let c = EngineConfig::new("/data");
        assert!(!c.library, "off");
        assert_eq!(c.ollama_url, "http://127.0.0.1:11434", "url");
        assert_eq!(c.summary_model, "qwen3.8:27b-mlx", "model");
        assert_eq!(
            (DEFAULT_OLLAMA_URL, DEFAULT_SUMMARY_MODEL),
            ("http://127.0.0.1:11434", "qwen3.8:27b-mlx"),
            "the constants"
        );
        assert_eq!(
            [
                EngineError::NotInLibrary.to_string(),
                EngineError::Settings("disk full".into()).to_string(),
                EngineError::Editor("no text editor".into()).to_string(),
            ],
            [
                "That recording is no longer in the library.",
                "Could not save the settings: disk full.",
                "Could not open the vocabulary file: no text editor.",
            ],
            "sentences"
        );
    }

    #[test]
    fn a_finished_session_is_listed_then_summarized() {
        let dir = TempDir::new().unwrap();
        let fakes = Fakes::default();
        let (engine, rx) = fakes.open(library_config(&dir));
        let _recording = engine.record().unwrap();
        assert_eq!(
            engine.stop_and_wait(PATIENCE).unwrap(),
            "hello world",
            "text"
        );
        let listed = engine.library();
        assert_eq!(listed.len(), 1, "one entry: {listed:?}");
        assert_eq!(
            listed.first().unwrap().title,
            "2026-10-26 14:03:09",
            "titled by the clock"
        );
        let _summarized = until(&rx, |s| s.library_revision == 2).unwrap();
        assert_eq!(
            engine.library(),
            [LibraryEntry {
                id: "20261026-140309".to_string(),
                title: "2026-10-26 14:03:09".to_string(),
                summary: "A dictated note.".to_string(),
            }],
            "the summary"
        );
        assert_eq!(
            engine.library_transcript("20261026-140309").unwrap(),
            "hello world",
            "the transcript"
        );
    }

    #[test]
    fn a_slow_summary_never_holds_up_the_next_session() {
        let dir = TempDir::new().unwrap();
        let (summarizer, gate) = FakeSummarizer::gated();
        let fakes = Fakes {
            summarizer,
            ..Fakes::default()
        };
        let (engine, _rx) = fakes.open(library_config(&dir));
        for _ in 0..2 {
            let _recording = engine.record().unwrap();
            assert_eq!(
                engine.stop_and_wait(PATIENCE).unwrap(),
                "hello world",
                "text"
            );
        }
        let listed = engine.library();
        assert_eq!(
            listed.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            ["20261026-140309-2", "20261026-140309"],
            "both listed while the summaries wait"
        );
        assert!(
            listed.iter().all(|e| e.summary == "hello world"),
            "with their excerpts: {listed:?}"
        );
        drop(gate);
    }

    #[test]
    fn with_the_library_off_nothing_is_kept() {
        let dir = TempDir::new().unwrap();
        let fakes = Fakes::default();
        let (engine, _rx) = fakes.open(config(&dir));
        let _recording = engine.record().unwrap();
        let _text = engine.stop_and_wait(PATIENCE).unwrap();
        assert_eq!(engine.library(), [], "nothing listed");
        assert!(!dir.path().join("library").exists(), "no library folder");
        assert_eq!(
            engine.library_transcript("20261026-140309"),
            Err(EngineError::NotInLibrary),
            "no transcript"
        );
    }

    #[test]
    fn an_id_outside_the_library_is_refused() {
        let dir = TempDir::new().unwrap();
        let fakes = Fakes::default();
        let (engine, _rx) = fakes.open(library_config(&dir));
        write(&dir, "segments/transcript.txt", b"current");
        for id in ["../segments", "20261026-140309"] {
            assert_eq!(
                engine.library_transcript(id),
                Err(EngineError::NotInLibrary),
                "{id}"
            );
            assert_eq!(
                engine.library_copy(id),
                Err(EngineError::NotInLibrary),
                "{id}"
            );
        }
    }

    #[test]
    fn library_copy_puts_the_transcript_on_the_clipboard() {
        let dir = TempDir::new().unwrap();
        let board = RecordingClipboard::default();
        let fakes = Fakes::default();
        let (engine, _rx) = fakes.open(EngineConfig {
            clipboard: Some(Arc::new(board.clone())),
            ..library_config(&dir)
        });
        write(
            &dir,
            "library/20261026-090000/transcript.txt",
            b"kept words",
        );
        engine.library_copy("20261026-090000").unwrap();
        assert_eq!(*board.writes.lock().unwrap(), ["kept words"], "copied");
    }

    #[test]
    fn a_library_copy_the_clipboard_refuses_is_an_error() {
        struct Broken;
        impl Clipboard for Broken {
            fn write(&self, _text: &str) -> Result<(), PortError> {
                Err(PortError("pasteboard unavailable".to_string()))
            }
        }
        let dir = TempDir::new().unwrap();
        let fakes = Fakes::default();
        let (engine, _rx) = fakes.open(EngineConfig {
            clipboard: Some(Arc::new(Broken)),
            ..library_config(&dir)
        });
        write(
            &dir,
            "library/20261026-090000/transcript.txt",
            b"kept words",
        );
        assert_eq!(
            engine.library_copy("20261026-090000"),
            Err(EngineError::Refused(DictationError::Clipboard(PortError(
                "pasteboard unavailable".to_string()
            )))),
            "refused"
        );
    }

    /// The default glossary's absolute path in `dir`.
    fn default_vocabulary(dir: &TempDir) -> String {
        dir.path()
            .join("vocabulary.txt")
            .to_string_lossy()
            .into_owned()
    }

    fn saved(dir: &TempDir) -> String {
        std::fs::read_to_string(dir.path().join("settings.json")).unwrap()
    }

    #[test]
    fn settings_round_trip_through_the_data_folder() {
        let dir = TempDir::new().unwrap();
        let fakes = Fakes::default();
        let (engine, _rx) = fakes.open(config(&dir));
        let defaults = Settings {
            summaries: true,
            library_open: true,
            vocabulary: default_vocabulary(&dir),
        };
        assert_eq!(engine.settings(), defaults, "the defaults");
        let off = Settings {
            summaries: false,
            ..defaults.clone()
        };
        assert_eq!(engine.set_settings(off.clone()), Ok(off.clone()), "saved");
        assert_eq!(engine.settings(), off, "read back");
        assert_eq!(
            saved(&dir),
            r#"{"summaries":false,"library_open":true,"vocabulary":""}"#,
            "snake_case on disk, the default glossary as empty"
        );
        let _report = engine.shutdown();
        assert_eq!(
            engine.set_settings(defaults),
            Err(EngineError::ShutDown),
            "refused after shutdown"
        );
        assert_eq!(engine.settings(), off, "still readable after shutdown");
    }

    #[test]
    fn settings_that_cannot_be_saved_are_an_error() {
        let dir = TempDir::new().unwrap();
        let fakes = Fakes::default();
        let (engine, _rx) = fakes.open(config(&dir));
        std::fs::create_dir_all(dir.path().join("settings.json")).unwrap();
        let saved = engine.set_settings(Settings {
            summaries: false,
            library_open: false,
            vocabulary: String::new(),
        });
        assert!(
            matches!(&saved, Err(EngineError::Settings(_))),
            "an error: {saved:?}"
        );
    }

    #[test]
    fn the_editor_is_macos_open_by_default() {
        assert_eq!(
            EngineConfig::new("/data").editor,
            PathBuf::from("/usr/bin/open"),
            "editor"
        );
    }

    #[test]
    fn a_vocabulary_path_is_saved_and_an_empty_one_keeps_it() {
        let dir = TempDir::new().unwrap();
        let fakes = Fakes::default();
        let (engine, _rx) = fakes.open(config(&dir));
        let elsewhere = Settings {
            summaries: true,
            library_open: true,
            vocabulary: "/x/terms.txt".to_string(),
        };
        assert_eq!(
            engine.set_settings(elsewhere.clone()),
            Ok(elsewhere.clone()),
            "saved"
        );
        assert_eq!(engine.settings().vocabulary, "/x/terms.txt", "read back");
        assert!(
            saved(&dir).contains(r#""vocabulary":"/x/terms.txt""#),
            "on disk"
        );
        let unknown = Settings {
            summaries: false,
            library_open: true,
            vocabulary: String::new(),
        };
        let kept = Settings {
            summaries: false,
            ..elsewhere
        };
        assert_eq!(engine.set_settings(unknown), Ok(kept.clone()), "kept");
        assert_eq!(engine.settings(), kept, "read back");
    }

    #[test]
    fn the_default_vocabulary_path_is_saved_as_empty() {
        let dir = TempDir::new().unwrap();
        let fakes = Fakes::default();
        let (engine, _rx) = fakes.open(config(&dir));
        let _: Settings = engine
            .set_settings(Settings {
                summaries: true,
                library_open: true,
                vocabulary: "/x/terms.txt".to_string(),
            })
            .unwrap();
        let back = engine
            .set_settings(Settings {
                summaries: true,
                library_open: true,
                vocabulary: default_vocabulary(&dir),
            })
            .unwrap();
        assert_eq!(back.vocabulary, default_vocabulary(&dir), "answered");
        assert!(saved(&dir).contains(r#""vocabulary":"""#), "stored empty");
        assert_eq!(
            engine.settings().vocabulary,
            default_vocabulary(&dir),
            "read back"
        );
    }

    #[test]
    fn a_relative_vocabulary_path_is_the_default_and_dropped_on_save() {
        let dir = TempDir::new().unwrap();
        write(&dir, "settings.json", br#"{"vocabulary":"terms.txt"}"#);
        let fakes = Fakes::default();
        let (engine, _rx) = fakes.open(config(&dir));
        assert_eq!(
            engine.settings().vocabulary,
            default_vocabulary(&dir),
            "the default"
        );
        let _: Settings = engine
            .set_settings(Settings {
                summaries: true,
                library_open: true,
                vocabulary: String::new(),
            })
            .unwrap();
        assert!(saved(&dir).contains(r#""vocabulary":"""#), "discarded");
    }

    /// An editor stub in `dir` that writes its arguments, one per line, to
    /// `argv.txt` there, then exits with `status`.
    fn editor(dir: &TempDir, status: u8) -> (PathBuf, PathBuf) {
        use std::os::unix::fs::PermissionsExt as _;
        let log = dir.path().join("argv.txt");
        let path = dir.path().join("open");
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nfor a in \"$@\"; do echo \"$a\"; done > '{}'\necho nope >&2\nexit {status}\n",
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        (path, log)
    }

    #[test]
    fn open_vocabulary_creates_and_opens_the_default_glossary() {
        let dir = TempDir::new().unwrap();
        let bin = TempDir::new().unwrap();
        let (stub, log) = editor(&bin, 0);
        let fakes = Fakes::default();
        let (engine, _rx) = fakes.open(EngineConfig {
            editor: stub,
            ..config(&dir)
        });
        engine.open_vocabulary().unwrap();
        let file = dir.path().join("vocabulary.txt");
        assert_eq!(std::fs::read(&file).unwrap(), b"", "created empty");
        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            format!("-t\n{}\n", file.display()),
            "opened as text"
        );
        let _report = engine.shutdown();
        std::fs::remove_file(&log).unwrap();
        engine.open_vocabulary().unwrap();
        assert!(log.exists(), "still opens after shutdown");
    }

    #[test]
    fn open_vocabulary_opens_the_configured_glossary() {
        let dir = TempDir::new().unwrap();
        let other = TempDir::new().unwrap();
        let bin = TempDir::new().unwrap();
        let (stub, log) = editor(&bin, 0);
        let fakes = Fakes::default();
        let (engine, _rx) = fakes.open(EngineConfig {
            editor: stub,
            ..config(&dir)
        });
        let file = other.path().join("words/terms.txt");
        let _: Settings = engine
            .set_settings(Settings {
                summaries: true,
                library_open: true,
                vocabulary: file.to_string_lossy().into_owned(),
            })
            .unwrap();
        engine.open_vocabulary().unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"", "created empty");
        assert!(
            !dir.path().join("vocabulary.txt").exists(),
            "not the default"
        );
        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            format!("-t\n{}\n", file.display()),
            "opened"
        );
    }

    #[test]
    fn a_failing_editor_is_an_editor_error() {
        let dir = TempDir::new().unwrap();
        let bin = TempDir::new().unwrap();
        let (stub, _log) = editor(&bin, 3);
        let fakes = Fakes::default();
        let (engine, _rx) = fakes.open(EngineConfig {
            editor: stub,
            ..config(&dir)
        });
        let opened = engine.open_vocabulary();
        assert!(
            matches!(&opened, Err(EngineError::Editor(_))),
            "an error: {opened:?}"
        );
        let text = opened.unwrap_err().to_string();
        assert!(
            text.starts_with("Could not open the vocabulary file: "),
            "{text}"
        );
        assert!(text.contains("nope"), "with the reason: {text}");
    }
}
