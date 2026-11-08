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
        Engine, EngineConfig, EngineError, Event, InputDevices, LibraryEntry, ModelSource, Phase,
        PortError, Recorder, Settings, ShutdownReport, Source, StreamRecorder, level_sink,
    };
    use tempfile::TempDir;

    use serde_json::{Value, json};

    use crate::common::{
        FakeCleaner, FakeDownloader, FakeSummarizer, FakeTranscriber, FakeWarmer, Fakes, PATIENCE,
        RecordingClipboard, closed_url, events, serve_once, until,
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
        assert_eq!(c.keep_warm_every, Duration::from_secs(240), "keep warm");
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

    /// Fakes whose cleanup model answers `reply`.
    fn cleaning(reply: Result<&str, &str>) -> Fakes {
        let cleaner = FakeCleaner::default();
        *cleaner.reply.lock().unwrap() = reply.map(str::to_string).map_err(str::to_string);
        Fakes {
            cleaner,
            ..Fakes::default()
        }
    }

    #[test]
    fn an_accepted_cleanup_is_the_transcript() {
        let dir = TempDir::new().unwrap();
        write(&dir, "settings.json", br#"{"cleanup":true}"#);
        write(&dir, "vocabulary.txt", b"Zed\n");
        let fakes = cleaning(Ok("Zed world"));
        let clipboard = RecordingClipboard::default();
        let (engine, _rx) = fakes.open(EngineConfig {
            clipboard: Some(Arc::new(clipboard.clone())),
            ..library_config(&dir)
        });
        let _recording = engine.record().unwrap();
        assert_eq!(
            engine.stop_and_wait(PATIENCE).unwrap(),
            "Zed world",
            "cleaned"
        );
        assert_eq!(*clipboard.writes.lock().unwrap(), ["Zed world"], "copied");
        assert_eq!(fakes.cleaner.calls.load(Ordering::SeqCst), 1, "asked once");
        assert_eq!(
            std::fs::read_to_string(
                dir.path()
                    .join("library/20261026-140309/transcript.raw.txt")
            )
            .unwrap(),
            "hello world",
            "the raw text is kept"
        );
    }

    #[test]
    fn a_failed_cleanup_keeps_the_raw_transcript() {
        let dir = TempDir::new().unwrap();
        write(&dir, "settings.json", br#"{"cleanup":true}"#);
        let fakes = cleaning(Err("down"));
        let (engine, _rx) = fakes.open(library_config(&dir));
        let _recording = engine.record().unwrap();
        assert_eq!(
            engine.stop_and_wait(PATIENCE).unwrap(),
            "hello world",
            "raw"
        );
        assert_eq!(fakes.cleaner.calls.load(Ordering::SeqCst), 1, "asked once");
    }

    #[test]
    fn cleanup_is_off_by_default() {
        let dir = TempDir::new().unwrap();
        let fakes = cleaning(Ok("Zed world"));
        let (engine, _rx) = fakes.open(library_config(&dir));
        let _recording = engine.record().unwrap();
        assert_eq!(
            engine.stop_and_wait(PATIENCE).unwrap(),
            "hello world",
            "raw"
        );
        assert_eq!(fakes.cleaner.calls.load(Ordering::SeqCst), 0, "never asked");
    }

    fn cleanup_label(dir: &TempDir) -> String {
        let text =
            std::fs::read_to_string(dir.path().join("library/20261026-140309/entry.json")).unwrap();
        let entry: serde_json::Value = serde_json::from_str(&text).unwrap();
        entry
            .get("cleanup")
            .and_then(|v| v.as_str())
            .unwrap()
            .to_string()
    }

    #[test]
    fn a_json_cleanup_reply_is_judged_by_its_fields() {
        let dir = TempDir::new().unwrap();
        write(&dir, "settings.json", br#"{"cleanup":true}"#);
        write(&dir, "vocabulary.txt", b"Zed\n");
        let fakes = cleaning(Ok(
            r#"{"replacements":[{"from":"Yukara","to":"Zed"}],"learned":[],"transcript":"Zed world"}"#,
        ));
        let (engine, _rx) = fakes.open(library_config(&dir));
        let _recording = engine.record().unwrap();
        assert_eq!(
            engine.stop_and_wait(PATIENCE).unwrap(),
            "Yukara world",
            "the bogus replacement is reverted"
        );
    }

    #[test]
    fn a_braced_reply_that_is_not_a_reply_is_literal_text() {
        let dir = TempDir::new().unwrap();
        write(&dir, "settings.json", br#"{"cleanup":true}"#);
        let fakes = cleaning(Ok(r#"{"transcript":3}"#));
        let (engine, _rx) = fakes.open(library_config(&dir));
        let _recording = engine.record().unwrap();
        assert_eq!(
            engine.stop_and_wait(PATIENCE).unwrap(),
            "hello world",
            "the raw text"
        );
        assert_eq!(cleanup_label(&dir), "rejected-cut-off", "literal text");
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

    /// The default learned list's absolute path in `dir`.
    fn default_learned(dir: &TempDir) -> String {
        dir.path()
            .join("learned.txt")
            .to_string_lossy()
            .into_owned()
    }

    /// The default model folder's absolute path in `dir`.
    fn default_model(dir: &TempDir) -> String {
        dir.path().join("models").to_string_lossy().into_owned()
    }

    fn saved(dir: &TempDir) -> String {
        std::fs::read_to_string(dir.path().join("settings.json")).unwrap()
    }

    /// The model log's lines, parsed.
    fn model_log(dir: &TempDir) -> Vec<Value> {
        std::fs::read_to_string(dir.path().join("model-log.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// The menu bar log's lines, parsed.
    fn menubar_log(dir: &TempDir) -> Vec<Value> {
        std::fs::read_to_string(dir.path().join("menubar-log.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn toggle_starts_recording_from_idle_ready_and_finished() {
        let dir = TempDir::new().unwrap();
        let (engine, rx) = Fakes::default().open(config(&dir));
        let idle = engine.state();
        assert_eq!(idle.phase, Phase::Idle, "idle");
        let recording = engine.toggle().unwrap();
        assert_eq!(recording.phase, Phase::Recording, "idle -> recording");
        assert!(recording.revision > idle.revision, "revision grew");

        // A second press is Done: the session transcribes, then finishes.
        let transcribing = engine.toggle().unwrap();
        assert_eq!(transcribing.phase, Phase::Transcribing, "recording -> done");
        assert!(transcribing.revision > recording.revision, "revision grew");
        let finished = until(&rx, |s| s.phase == Phase::Finished).unwrap();
        assert_eq!(finished.phase, Phase::Finished, "finished after the job");

        let again = engine.toggle().unwrap();
        assert_eq!(
            (again.phase, again.recorded_ms),
            (Phase::Recording, 0),
            "finished -> a new recording session"
        );
        assert!(again.revision > finished.revision, "revision grew");

        // Discard from recording is Ready; the toggle records at once.
        let ready = engine.act(Action::Discard, "").unwrap();
        assert_eq!(ready.phase, Phase::Ready, "ready");
        let started = engine.toggle().unwrap();
        assert_eq!(started.phase, Phase::Recording, "ready -> recording");
        assert!(started.revision > ready.revision, "revision grew");
    }

    #[test]
    fn toggle_resumes_a_paused_session() {
        let dir = TempDir::new().unwrap();
        let (engine, _rx) = Fakes::default().open(config(&dir));
        let _recording = engine.record().unwrap();
        let paused = engine.act(Action::Pause, "").unwrap();
        assert_eq!(paused.phase, Phase::Paused, "paused");
        let resumed = engine.toggle().unwrap();
        assert_eq!(
            resumed.phase,
            Phase::Recording,
            "paused -> recording (resume)"
        );
        assert!(resumed.revision > paused.revision, "revision grew");
    }

    #[test]
    fn toggle_does_nothing_while_transcribing() {
        let dir = TempDir::new().unwrap();
        let (transcriber, gate) = FakeTranscriber::default().gated();
        let fakes = Fakes::with(transcriber);
        let (engine, _rx) = fakes.open(config(&dir));
        let _recording = engine.record().unwrap();
        let done = engine.act(Action::Done, "").unwrap();
        assert_eq!(done.phase, Phase::Transcribing, "transcribing");
        let before = engine.state();
        assert_eq!(before.phase, Phase::Transcribing, "still transcribing");
        let after = engine.toggle().unwrap();
        assert_eq!(after.phase, Phase::Transcribing, "nothing");
        assert_eq!(after.revision, before.revision, "revision unchanged");
        let line = menubar_log(&dir).pop().unwrap();
        assert_eq!(line["toggle"], "nothing", "logged");
        assert_eq!(line["outcome"], "nothing", "logged");
        assert_eq!(line["phase"], "transcribing", "logged");
        assert_eq!(line["action"], Value::Null, "no action");
        let _opened = gate.send(());
    }

    #[test]
    fn toggle_does_nothing_on_a_failed_session() {
        let dir = TempDir::new().unwrap();
        write(&dir, "segments/segment-0000.wav", b"RIFF");
        let (engine, _rx) = Fakes::default().open(config(&dir));
        let before = engine.state();
        assert_eq!(before.phase, Phase::Failed, "failed");
        let after = engine.toggle().unwrap();
        assert_eq!(after.phase, Phase::Failed, "nothing");
        assert_eq!(after.revision, before.revision, "revision unchanged");
        assert!(engine.retry().is_ok(), "the failed slot is still retryable");
    }

    #[test]
    fn a_refused_toggle_is_logged_with_the_engine_sentence() {
        let dir = TempDir::new().unwrap();
        let model = dir.path().join("missing.bin");
        let fakes = Fakes::with(FakeTranscriber {
            model: Some(model),
            ..FakeTranscriber::default()
        });
        let (engine, _rx) = fakes.open(config(&dir));
        let err = engine.toggle().unwrap_err();
        assert!(
            matches!(err, EngineError::Refused(DictationError::ModelMissing(_))),
            "refused: {err}"
        );
        let line = menubar_log(&dir).pop().unwrap();
        assert_eq!(line["toggle"], "record", "logged");
        assert_eq!(line["outcome"], "refused", "logged");
        assert_eq!(line["error"], err.to_string(), "the engine's sentence");
        assert_eq!(line["phase_after"], "idle", "the phase before, unchanged");
    }

    #[test]
    fn toggle_after_shutdown_fails_in_every_phase() {
        let dir = TempDir::new().unwrap();
        write(&dir, "segments/segment-0000.wav", b"RIFF");
        let (engine, _rx) = Fakes::default().open(config(&dir));
        assert_eq!(engine.state().phase, Phase::Failed, "failed");
        let _report = engine.shutdown();
        assert_eq!(engine.toggle(), Err(EngineError::ShutDown), "refused");
        let line = menubar_log(&dir).pop().unwrap();
        assert_eq!(line["outcome"], "failed", "logged");
        assert_eq!(line["error"], "The engine has shut down.", "the sentence");
    }

    /// A press that does nothing in `phase` writes the pinned `pause` line.
    fn nothing_press(engine: &Engine, dir: &TempDir, phase: &str) {
        let before = engine.state();
        let after = engine.press_pause().unwrap();
        assert_eq!(after.phase, before.phase, "{phase}: phase unchanged");
        assert_eq!(
            after.revision, before.revision,
            "{phase}: revision unchanged"
        );
        assert_eq!(
            menubar_log(dir).pop().unwrap(),
            json!({
                "event": "pause",
                "phase": phase,
                "action": null,
                "outcome": "nothing",
                "error": null,
                "phase_after": phase
            }),
            "{phase}: logged"
        );
    }

    #[test]
    fn the_pause_shortcut_pauses_and_resumes() {
        let dir = TempDir::new().unwrap();
        let (engine, _rx) = Fakes::default().open(config(&dir));
        let _recording = engine.record().unwrap();
        let paused = engine.press_pause().unwrap();
        assert_eq!(paused.phase, Phase::Paused, "recording -> paused");
        assert_eq!(
            menubar_log(&dir).pop().unwrap(),
            json!({
                "event": "pause",
                "phase": "recording",
                "action": "pause",
                "outcome": "ok",
                "error": null,
                "phase_after": "paused"
            }),
            "the pause line"
        );
        let resumed = engine.press_pause().unwrap();
        assert_eq!(resumed.phase, Phase::Recording, "paused -> recording");
        assert!(resumed.revision > paused.revision, "revision grew");
        assert_eq!(
            menubar_log(&dir).pop().unwrap(),
            json!({
                "event": "pause",
                "phase": "paused",
                "action": "resume",
                "outcome": "ok",
                "error": null,
                "phase_after": "recording"
            }),
            "the resume line"
        );
    }

    #[test]
    fn the_pause_shortcut_does_nothing_in_every_other_phase() {
        let dir = TempDir::new().unwrap();
        let (idle, _idle_rx) = Fakes::default().open(config(&dir));
        nothing_press(&idle, &dir, "idle");

        let ready_dir = TempDir::new().unwrap();
        let (ready, _ready_rx) = Fakes::default().open(config(&ready_dir));
        let _ready_recording = ready.record().unwrap();
        let _ready = ready.act(Action::Discard, "").unwrap();
        nothing_press(&ready, &ready_dir, "ready");

        let transcribing_dir = TempDir::new().unwrap();
        let (transcriber, gate) = FakeTranscriber::default().gated();
        let (transcribing, _transcribing_rx) =
            Fakes::with(transcriber).open(config(&transcribing_dir));
        let _transcribing_recording = transcribing.record().unwrap();
        let done = transcribing.act(Action::Done, "").unwrap();
        assert_eq!(done.phase, Phase::Transcribing, "transcribing");
        nothing_press(&transcribing, &transcribing_dir, "transcribing");
        let _opened = gate.send(());

        let finished_dir = TempDir::new().unwrap();
        let (finished, _finished_rx) = Fakes::default().open(config(&finished_dir));
        let _finished_recording = finished.record().unwrap();
        assert_eq!(finished.stop_and_wait(PATIENCE).unwrap(), "hello world");
        nothing_press(&finished, &finished_dir, "finished");

        let failed_dir = TempDir::new().unwrap();
        write(&failed_dir, "segments/segment-0000.wav", b"RIFF");
        let (failed, _failed_rx) = Fakes::default().open(config(&failed_dir));
        assert_eq!(failed.state().phase, Phase::Failed, "failed");
        nothing_press(&failed, &failed_dir, "failed");
    }

    #[test]
    fn a_refused_pause_press_is_logged_with_the_engine_sentence() {
        let dir = TempDir::new().unwrap();
        let model = dir.path().join("model.bin");
        std::fs::write(&model, b"model").unwrap();
        let fakes = Fakes::with(FakeTranscriber {
            model: Some(model.clone()),
            ..FakeTranscriber::default()
        });
        let (engine, _rx) = fakes.open(config(&dir));
        let _recording = engine.record().unwrap();
        let paused = engine.act(Action::Pause, "").unwrap();
        assert_eq!(paused.phase, Phase::Paused, "paused");
        std::fs::remove_file(&model).unwrap();
        let err = engine.press_pause().unwrap_err();
        assert!(
            matches!(err, EngineError::Refused(DictationError::ModelMissing(_))),
            "refused: {err}"
        );
        assert_eq!(engine.state().phase, Phase::Paused, "still paused");
        assert_eq!(
            menubar_log(&dir).pop().unwrap(),
            json!({
                "event": "pause",
                "phase": "paused",
                "action": "resume",
                "outcome": "refused",
                "error": err.to_string(),
                "phase_after": "paused"
            }),
            "the refused resume line"
        );
    }

    #[test]
    fn a_pause_press_after_shutdown_fails_in_every_phase() {
        let dir = TempDir::new().unwrap();
        let (engine, _rx) = Fakes::default().open(config(&dir));
        let _report = engine.shutdown();
        assert_eq!(engine.press_pause(), Err(EngineError::ShutDown), "refused");
        assert_eq!(
            menubar_log(&dir).pop().unwrap(),
            json!({
                "event": "pause",
                "phase": "idle",
                "action": null,
                "outcome": "failed",
                "error": "The engine has shut down.",
                "phase_after": "idle"
            }),
            "logged"
        );
    }

    #[test]
    fn the_pause_accelerator_follows_the_saved_shortcut() {
        let dir = TempDir::new().unwrap();
        let (engine, _rx) = Fakes::default().open(config(&dir));
        assert_eq!(
            engine.pause_accelerator(),
            Some("Alt+Super+KeyP".to_string()),
            "the default"
        );
        let saved = engine
            .set_settings(Settings {
                pause_shortcut: "Opt+Cmd+Q".to_string(),
                ..engine.settings()
            })
            .unwrap();
        assert_eq!(saved.pause_shortcut, "Opt+Cmd+Q", "saved");
        assert_eq!(
            engine.pause_accelerator(),
            Some("Alt+Super+KeyQ".to_string()),
            "after the save"
        );
    }

    #[test]
    fn a_toggle_on_the_pause_chord_leaves_no_pause_accelerator() {
        let dir = TempDir::new().unwrap();
        write(&dir, "settings.json", br#"{"toggle_shortcut":"Opt+Cmd+P"}"#);
        let (engine, _rx) = Fakes::default().open(config(&dir));
        assert_eq!(engine.pause_accelerator(), None, "no second shortcut");
    }

    #[test]
    fn a_commands_snapshot_is_delivered_as_an_event() {
        let dir = TempDir::new().unwrap();
        let (engine, rx) = Fakes::default().open(config(&dir));
        let _recording = engine.record().unwrap();
        while rx.try_recv().is_ok() {}
        let answered = engine.act(Action::Pause, "").unwrap();
        assert_eq!(answered.phase, Phase::Paused, "paused");
        let delivered = until(&rx, |s| s.phase == Phase::Paused).unwrap();
        assert_eq!(
            delivered, answered,
            "the same snapshot, on the caller's thread"
        );
    }

    #[test]
    fn the_toggle_accelerator_follows_the_saved_shortcut() {
        let dir = TempDir::new().unwrap();
        let (engine, _rx) = Fakes::default().open(config(&dir));
        assert_eq!(engine.toggle_accelerator(), "Alt+Super+KeyD", "the default");
        let saved = engine
            .set_settings(Settings {
                toggle_shortcut: "Opt+Cmd+K".to_string(),
                ..engine.settings()
            })
            .unwrap();
        assert_eq!(saved.toggle_shortcut, "Opt+Cmd+K", "saved");
        assert_eq!(
            engine.toggle_accelerator(),
            "Alt+Super+KeyK",
            "after the save"
        );
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
            cleanup: false,
            summary_model: DEFAULT_SUMMARY_MODEL.to_string(),
            cleanup_model: DEFAULT_SUMMARY_MODEL.to_string(),
            library_width: 220,
            input_device: String::new(),
            silence_split_ms: 0,
            keep_segments: false,
            learned_vocabulary: default_learned(&dir),
            model_folder: default_model(&dir),
            toggle_shortcut: "Opt+Cmd+D".to_string(),
            pause_shortcut: "Opt+Cmd+P".to_string(),
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
            r#"{"summaries":false,"library_open":true,"vocabulary":"","cleanup":false,"summary_model":"","cleanup_model":"","library_width":220,"input_device":"","silence_split_ms":0,"keep_segments":false,"learned_vocabulary":"","model_folder":"","toggle_shortcut":"Opt+Cmd+D","pause_shortcut":"Opt+Cmd+P"}"#,
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
    fn settings_json_owns_keep_segments() {
        let dir = TempDir::new().unwrap();
        write(&dir, "settings.json", br#"{"keep_segments":true}"#);
        let (engine, _rx) = Fakes::default().open(config(&dir));
        let off = Settings {
            keep_segments: false,
            summaries: false,
            ..engine.settings()
        };
        let back = engine.set_settings(off).unwrap();
        assert!(back.keep_segments, "returned");
        assert!(engine.settings().keep_segments, "read back");
        assert!(!back.summaries, "the other keys are saved");
        assert!(
            saved(&dir)
                .ends_with(r#""keep_segments":true,"learned_vocabulary":"","model_folder":"","toggle_shortcut":"Opt+Cmd+D","pause_shortcut":"Opt+Cmd+P"}"#),
            "on disk, before the learned list's default"
        );
    }

    #[test]
    fn a_settings_change_cannot_turn_keep_segments_on() {
        let dir = TempDir::new().unwrap();
        let (engine, _rx) = Fakes::default().open(config(&dir));
        let on = Settings {
            keep_segments: true,
            ..engine.settings()
        };
        assert!(!engine.set_settings(on).unwrap().keep_segments, "returned");
        assert!(!engine.settings().keep_segments, "read back");
        assert!(
            saved(&dir)
                .ends_with(r#""keep_segments":false,"learned_vocabulary":"","model_folder":"","toggle_shortcut":"Opt+Cmd+D","pause_shortcut":"Opt+Cmd+P"}"#),
            "on disk, before the learned list's default"
        );
    }

    #[test]
    fn keep_segments_keeps_each_part_on_disk() {
        let dir = TempDir::new().unwrap();
        write(&dir, "settings.json", br#"{"keep_segments":true}"#);
        let (engine, _rx) = Fakes::default().open(library_config(&dir));
        let _recording = engine.record().unwrap();
        assert_eq!(engine.stop_and_wait(PATIENCE).unwrap(), "hello world");
        let kept = std::fs::read(
            dir.path()
                .join("library/20261026-140309/segments/segment-0000.wav"),
        )
        .unwrap();
        assert_eq!(&kept[..4], b"RIFF", "a WAV file");
        assert_eq!(kept.len(), 44 + 2 * 16_000, "one second of 16-bit PCM");
        assert!(
            !dir.path().join("segments/segment-0000.wav").exists(),
            "the source is removed"
        );
    }

    #[test]
    fn the_library_width_passes_through_and_is_clamped() {
        let dir = TempDir::new().unwrap();
        let (engine, _rx) = Fakes::default().open(config(&dir));
        let wide = |library_width| Settings {
            library_width,
            ..engine.settings()
        };
        assert_eq!(engine.set_settings(wide(300)).unwrap().library_width, 300);
        assert_eq!(engine.settings().library_width, 300, "read back");
        assert_eq!(engine.set_settings(wide(5000)).unwrap().library_width, 440);
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
            cleanup: false,
            summary_model: DEFAULT_SUMMARY_MODEL.to_string(),
            cleanup_model: DEFAULT_SUMMARY_MODEL.to_string(),
            library_width: 220,
            input_device: String::new(),
            silence_split_ms: 0,
            keep_segments: false,
            learned_vocabulary: String::new(),
            model_folder: String::new(),
            toggle_shortcut: "Opt+Cmd+D".to_string(),
            pause_shortcut: "Opt+Cmd+P".to_string(),
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
            cleanup: false,
            summary_model: DEFAULT_SUMMARY_MODEL.to_string(),
            cleanup_model: DEFAULT_SUMMARY_MODEL.to_string(),
            library_width: 220,
            input_device: String::new(),
            silence_split_ms: 0,
            keep_segments: false,
            learned_vocabulary: default_learned(&dir),
            model_folder: default_model(&dir),
            toggle_shortcut: "Opt+Cmd+D".to_string(),
            pause_shortcut: "Opt+Cmd+P".to_string(),
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
            cleanup: false,
            summary_model: DEFAULT_SUMMARY_MODEL.to_string(),
            cleanup_model: DEFAULT_SUMMARY_MODEL.to_string(),
            library_width: 220,
            input_device: String::new(),
            silence_split_ms: 0,
            keep_segments: false,
            learned_vocabulary: String::new(),
            model_folder: String::new(),
            toggle_shortcut: "Opt+Cmd+D".to_string(),
            pause_shortcut: "Opt+Cmd+P".to_string(),
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
                cleanup: false,
                summary_model: DEFAULT_SUMMARY_MODEL.to_string(),
                cleanup_model: DEFAULT_SUMMARY_MODEL.to_string(),
                library_width: 220,
                input_device: String::new(),
                silence_split_ms: 0,
                keep_segments: false,
                learned_vocabulary: String::new(),
                model_folder: String::new(),
                toggle_shortcut: "Opt+Cmd+D".to_string(),
                pause_shortcut: "Opt+Cmd+P".to_string(),
            })
            .unwrap();
        let back = engine
            .set_settings(Settings {
                summaries: true,
                library_open: true,
                vocabulary: default_vocabulary(&dir),
                cleanup: false,
                summary_model: DEFAULT_SUMMARY_MODEL.to_string(),
                cleanup_model: DEFAULT_SUMMARY_MODEL.to_string(),
                library_width: 220,
                input_device: String::new(),
                silence_split_ms: 0,
                keep_segments: false,
                learned_vocabulary: String::new(),
                model_folder: String::new(),
                toggle_shortcut: "Opt+Cmd+D".to_string(),
                pause_shortcut: "Opt+Cmd+P".to_string(),
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
                cleanup: false,
                summary_model: DEFAULT_SUMMARY_MODEL.to_string(),
                cleanup_model: DEFAULT_SUMMARY_MODEL.to_string(),
                library_width: 220,
                input_device: String::new(),
                silence_split_ms: 0,
                keep_segments: false,
                learned_vocabulary: String::new(),
                model_folder: String::new(),
                toggle_shortcut: "Opt+Cmd+D".to_string(),
                pause_shortcut: "Opt+Cmd+P".to_string(),
            })
            .unwrap();
        assert!(saved(&dir).contains(r#""vocabulary":"""#), "discarded");
    }

    #[test]
    fn the_learned_vocabulary_defaults_to_the_data_folder_and_is_absolute() {
        let dir = TempDir::new().unwrap();
        let (engine, _rx) = Fakes::default().open(config(&dir));
        assert_eq!(
            engine.settings().learned_vocabulary,
            default_learned(&dir),
            "no file"
        );
        assert!(
            engine.settings().learned_vocabulary.starts_with('/'),
            "absolute"
        );
    }

    #[test]
    fn the_learned_vocabulary_in_the_settings_is_answered() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "settings.json",
            br#"{"learned_vocabulary":"/x/learned.txt"}"#,
        );
        let (engine, _rx) = Fakes::default().open(config(&dir));
        assert_eq!(
            engine.settings().learned_vocabulary,
            "/x/learned.txt",
            "configured"
        );
    }

    #[test]
    fn a_relative_learned_vocabulary_path_is_the_default() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "settings.json",
            br#"{"learned_vocabulary":"learned.txt"}"#,
        );
        let (engine, _rx) = Fakes::default().open(config(&dir));
        assert_eq!(
            engine.settings().learned_vocabulary,
            default_learned(&dir),
            "the default"
        );
    }

    #[test]
    fn a_learned_vocabulary_path_is_saved_and_an_empty_one_keeps_it() {
        let dir = TempDir::new().unwrap();
        let (engine, _rx) = Fakes::default().open(config(&dir));
        let elsewhere = Settings {
            learned_vocabulary: "/x/learned.txt".to_string(),
            ..engine.settings()
        };
        assert_eq!(
            engine.set_settings(elsewhere.clone()),
            Ok(elsewhere),
            "saved"
        );
        assert_eq!(
            engine.settings().learned_vocabulary,
            "/x/learned.txt",
            "read back"
        );
        assert!(
            saved(&dir).contains(r#""learned_vocabulary":"/x/learned.txt""#),
            "on disk"
        );
        let unknown = Settings {
            summaries: false,
            learned_vocabulary: String::new(),
            ..engine.settings()
        };
        let back = engine.set_settings(unknown).unwrap();
        assert_eq!(
            back.learned_vocabulary, "/x/learned.txt",
            "'' keeps the saved path"
        );
        assert_eq!(engine.settings().learned_vocabulary, "/x/learned.txt");
    }

    #[test]
    fn the_default_learned_vocabulary_path_is_saved_as_empty() {
        let dir = TempDir::new().unwrap();
        let (engine, _rx) = Fakes::default().open(config(&dir));
        let _: Settings = engine
            .set_settings(Settings {
                learned_vocabulary: "/x/learned.txt".to_string(),
                ..engine.settings()
            })
            .unwrap();
        let back = engine
            .set_settings(Settings {
                learned_vocabulary: default_learned(&dir),
                ..engine.settings()
            })
            .unwrap();
        assert_eq!(back.learned_vocabulary, default_learned(&dir), "answered");
        assert!(
            saved(&dir).contains(r#""learned_vocabulary":"""#),
            "stored empty"
        );
        assert_eq!(
            engine.settings().learned_vocabulary,
            default_learned(&dir),
            "read back"
        );
    }

    #[test]
    fn a_relative_learned_vocabulary_path_is_written_verbatim_and_dropped_on_read() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "settings.json",
            br#"{"learned_vocabulary":"learned.txt"}"#,
        );
        let (engine, _rx) = Fakes::default().open(config(&dir));
        assert_eq!(
            engine.settings().learned_vocabulary,
            default_learned(&dir),
            "the default"
        );
        let _: Settings = engine
            .set_settings(Settings {
                learned_vocabulary: "terms.txt".to_string(),
                ..engine.settings()
            })
            .unwrap();
        assert!(
            saved(&dir).contains(r#""learned_vocabulary":"terms.txt""#),
            "written verbatim"
        );
        assert_eq!(
            engine.settings().learned_vocabulary,
            default_learned(&dir),
            "read back as the default"
        );
    }

    #[test]
    fn the_model_folder_defaults_to_the_data_folder_and_is_absolute() {
        let dir = TempDir::new().unwrap();
        let (engine, _rx) = Fakes::default().open(config(&dir));
        assert_eq!(
            engine.settings().model_folder,
            default_model(&dir),
            "no file"
        );
        assert!(engine.settings().model_folder.starts_with('/'), "absolute");
    }

    #[test]
    fn the_model_folder_in_the_settings_is_answered() {
        let dir = TempDir::new().unwrap();
        write(&dir, "settings.json", br#"{"model_folder":"/x/models"}"#);
        let (engine, _rx) = Fakes::default().open(config(&dir));
        assert_eq!(engine.settings().model_folder, "/x/models", "configured");
    }

    #[test]
    fn a_relative_model_folder_is_the_default() {
        let dir = TempDir::new().unwrap();
        write(&dir, "settings.json", br#"{"model_folder":"models"}"#);
        let (engine, _rx) = Fakes::default().open(config(&dir));
        assert_eq!(
            engine.settings().model_folder,
            default_model(&dir),
            "the default"
        );
    }

    #[test]
    fn a_model_folder_path_is_saved_and_an_empty_one_keeps_it() {
        let dir = TempDir::new().unwrap();
        let (engine, _rx) = Fakes::default().open(config(&dir));
        let elsewhere = Settings {
            model_folder: "/x/models".to_string(),
            ..engine.settings()
        };
        assert_eq!(
            engine.set_settings(elsewhere.clone()),
            Ok(elsewhere),
            "saved"
        );
        assert_eq!(engine.settings().model_folder, "/x/models", "read back");
        assert!(
            saved(&dir).contains(r#""model_folder":"/x/models""#),
            "on disk"
        );
        let unknown = Settings {
            model_folder: String::new(),
            ..engine.settings()
        };
        let back = engine.set_settings(unknown).unwrap();
        assert_eq!(back.model_folder, "/x/models", "'' keeps the saved path");
        assert_eq!(engine.settings().model_folder, "/x/models");
    }

    #[test]
    fn the_default_model_folder_is_saved_as_empty() {
        let dir = TempDir::new().unwrap();
        let (engine, _rx) = Fakes::default().open(config(&dir));
        let back = engine
            .set_settings(Settings {
                model_folder: default_model(&dir),
                ..engine.settings()
            })
            .unwrap();
        assert_eq!(back.model_folder, default_model(&dir), "answered");
        assert!(saved(&dir).contains(r#""model_folder":"""#), "stored empty");
        assert_eq!(
            engine.settings().model_folder,
            default_model(&dir),
            "read back"
        );
        let _: Settings = engine
            .set_settings(Settings {
                model_folder: "models".to_string(),
                ..engine.settings()
            })
            .unwrap();
        assert!(
            saved(&dir).contains(r#""model_folder":"models""#),
            "written verbatim"
        );
        assert_eq!(
            engine.settings().model_folder,
            default_model(&dir),
            "read back as the default"
        );
    }

    #[test]
    fn an_empty_model_folder_with_nothing_saved_is_logged() {
        let dir = TempDir::new().unwrap();
        let (engine, _rx) = Fakes::default().open(config(&dir));
        let _: Settings = engine
            .set_settings(Settings {
                model_folder: String::new(),
                ..engine.settings()
            })
            .unwrap();
        assert_eq!(
            model_log(&dir).last(),
            Some(&json!({
                "event": "settings",
                "requested": "",
                "stored": "",
                "answered": default_model(&dir),
                "dropped": false
            })),
            "the last line, exactly"
        );
    }

    #[test]
    fn a_shared_model_folder_is_logged_as_kept() {
        let dir = TempDir::new().unwrap();
        let (engine, _rx) = Fakes::default().open(config(&dir));
        let _: Settings = engine
            .set_settings(Settings {
                model_folder: "/x/models".to_string(),
                ..engine.settings()
            })
            .unwrap();
        assert_eq!(
            model_log(&dir).last(),
            Some(&json!({
                "event": "settings",
                "requested": "/x/models",
                "stored": "/x/models",
                "answered": "/x/models",
                "dropped": false
            })),
            "the last line, exactly"
        );
    }

    #[test]
    fn the_default_model_folder_is_logged_as_stored_empty() {
        let dir = TempDir::new().unwrap();
        let (engine, _rx) = Fakes::default().open(config(&dir));
        let _: Settings = engine
            .set_settings(Settings {
                model_folder: default_model(&dir),
                ..engine.settings()
            })
            .unwrap();
        assert_eq!(
            model_log(&dir).last(),
            Some(&json!({
                "event": "settings",
                "requested": default_model(&dir),
                "stored": "",
                "answered": default_model(&dir),
                "dropped": false
            })),
            "the last line, exactly"
        );
    }

    #[test]
    fn a_relative_model_folder_is_logged_as_dropped() {
        let dir = TempDir::new().unwrap();
        let (engine, _rx) = Fakes::default().open(config(&dir));
        let _: Settings = engine
            .set_settings(Settings {
                model_folder: "models".to_string(),
                ..engine.settings()
            })
            .unwrap();
        assert_eq!(
            model_log(&dir).last(),
            Some(&json!({
                "event": "settings",
                "requested": "models",
                "stored": "models",
                "answered": default_model(&dir),
                "dropped": true
            })),
            "the last line, exactly"
        );
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
                cleanup: false,
                summary_model: DEFAULT_SUMMARY_MODEL.to_string(),
                cleanup_model: DEFAULT_SUMMARY_MODEL.to_string(),
                library_width: 220,
                input_device: String::new(),
                silence_split_ms: 0,
                keep_segments: false,
                learned_vocabulary: String::new(),
                model_folder: String::new(),
                toggle_shortcut: "Opt+Cmd+D".to_string(),
                pause_shortcut: "Opt+Cmd+P".to_string(),
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

    #[test]
    fn open_learned_vocabulary_creates_and_opens_the_default_list() {
        let dir = TempDir::new().unwrap();
        let bin = TempDir::new().unwrap();
        let (stub, log) = editor(&bin, 0);
        let fakes = Fakes::default();
        let (engine, _rx) = fakes.open(EngineConfig {
            editor: stub,
            ..config(&dir)
        });
        engine.open_learned_vocabulary().unwrap();
        let file = dir.path().join("learned.txt");
        assert_eq!(std::fs::read(&file).unwrap(), b"", "created empty");
        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            format!("-t\n{}\n", file.display()),
            "opened as text"
        );
        let _report = engine.shutdown();
        std::fs::remove_file(&log).unwrap();
        engine.open_learned_vocabulary().unwrap();
        assert!(log.exists(), "still opens after shutdown");
    }

    #[test]
    fn open_learned_vocabulary_opens_the_configured_list() {
        let dir = TempDir::new().unwrap();
        let other = TempDir::new().unwrap();
        let bin = TempDir::new().unwrap();
        let (stub, log) = editor(&bin, 0);
        let fakes = Fakes::default();
        let (engine, _rx) = fakes.open(EngineConfig {
            editor: stub,
            ..config(&dir)
        });
        let file = other.path().join("words/learned.txt");
        let _: Settings = engine
            .set_settings(Settings {
                learned_vocabulary: file.to_string_lossy().into_owned(),
                ..engine.settings()
            })
            .unwrap();
        engine.open_learned_vocabulary().unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"", "created empty");
        assert!(!dir.path().join("learned.txt").exists(), "not the default");
        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            format!("-t\n{}\n", file.display()),
            "opened"
        );
    }

    #[test]
    fn a_failing_editor_is_an_editor_error_for_the_learned_list() {
        let dir = TempDir::new().unwrap();
        let bin = TempDir::new().unwrap();
        let (stub, _log) = editor(&bin, 3);
        let fakes = Fakes::default();
        let (engine, _rx) = fakes.open(EngineConfig {
            editor: stub,
            ..config(&dir)
        });
        let opened = engine.open_learned_vocabulary();
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

    const DEFAULT: &str = "qwen3.8:27b-mlx";

    /// The settings as answered with nothing saved, but with these models.
    fn with_models(dir: &TempDir, summary_model: &str, cleanup_model: &str) -> Settings {
        Settings {
            summaries: true,
            library_open: true,
            vocabulary: default_vocabulary(dir),
            cleanup: false,
            summary_model: summary_model.to_string(),
            cleanup_model: cleanup_model.to_string(),
            library_width: 220,
            input_device: String::new(),
            silence_split_ms: 0,
            keep_segments: false,
            learned_vocabulary: default_learned(dir),
            model_folder: default_model(dir),
            toggle_shortcut: "Opt+Cmd+D".to_string(),
            pause_shortcut: "Opt+Cmd+P".to_string(),
        }
    }

    #[test]
    fn with_no_model_saved_both_are_answered_as_the_default() {
        let dir = TempDir::new().unwrap();
        let (engine, _rx) = Fakes::default().open(config(&dir));
        assert_eq!(
            engine.settings(),
            with_models(&dir, DEFAULT, DEFAULT),
            "no file"
        );
        write(
            &dir,
            "settings.json",
            br#"{"summaries":true,"library_open":true,"vocabulary":"","cleanup":false,"summary_model":"","cleanup_model":""}"#,
        );
        assert_eq!(
            engine.settings(),
            with_models(&dir, DEFAULT, DEFAULT),
            "saved empty"
        );
        write(&dir, "settings.json", br#"{"summary_model":42}"#);
        assert_eq!(engine.settings().summary_model, DEFAULT, "not a string");
    }

    #[test]
    fn the_models_are_saved_kept_when_empty_and_the_default_stored_empty() {
        let dir = TempDir::new().unwrap();
        let (engine, _rx) = Fakes::default().open(config(&dir));
        let chosen = with_models(&dir, "gemma3:4b", "llama3:8b");
        assert_eq!(
            engine.set_settings(chosen.clone()),
            Ok(chosen.clone()),
            "saved"
        );
        assert_eq!(engine.settings(), chosen, "read back");
        assert!(
            saved(&dir).contains(r#""summary_model":"gemma3:4b","cleanup_model":"llama3:8b""#),
            "on disk: {}",
            saved(&dir)
        );
        assert_eq!(
            engine.set_settings(with_models(&dir, "", "")),
            Ok(chosen.clone()),
            "empty keeps both"
        );
        let back = engine
            .set_settings(with_models(&dir, DEFAULT, "llama3:8b"))
            .unwrap();
        assert_eq!(back, with_models(&dir, DEFAULT, "llama3:8b"), "answered");
        assert!(
            saved(&dir).contains(r#""summary_model":"","cleanup_model":"llama3:8b""#),
            "the default stored empty: {}",
            saved(&dir)
        );
        assert_eq!(engine.settings().summary_model, DEFAULT, "read back");
        let _: Settings = engine
            .set_settings(with_models(&dir, "gemma3:4b", DEFAULT))
            .unwrap();
        assert!(
            saved(&dir).contains(r#""summary_model":"gemma3:4b","cleanup_model":"""#),
            "each on its own: {}",
            saved(&dir)
        );
    }

    #[test]
    fn the_summary_asks_the_model_saved_after_the_engine_opened() {
        let dir = TempDir::new().unwrap();
        let fakes = Fakes::default();
        let (engine, rx) = fakes.open(library_config(&dir));
        let _: Settings = engine
            .set_settings(with_models(&dir, "gemma3:4b", DEFAULT))
            .unwrap();
        let _recording = engine.record().unwrap();
        let _text = engine.stop_and_wait(PATIENCE).unwrap();
        let _summarized = until(&rx, |s| s.library_revision == 2).unwrap();
        assert_eq!(
            *fakes.summarizer.models.lock().unwrap(),
            ["gemma3:4b"],
            "chosen"
        );
    }

    #[test]
    fn with_no_model_saved_the_summary_asks_for_the_default() {
        let dir = TempDir::new().unwrap();
        let fakes = Fakes::default();
        let (engine, rx) = fakes.open(library_config(&dir));
        let _recording = engine.record().unwrap();
        let _text = engine.stop_and_wait(PATIENCE).unwrap();
        let _summarized = until(&rx, |s| s.library_revision == 2).unwrap();
        assert_eq!(
            *fakes.summarizer.models.lock().unwrap(),
            [""],
            "the default"
        );
    }

    #[test]
    fn the_cleanup_asks_its_own_model() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "settings.json",
            br#"{"cleanup":true,"cleanup_model":"llama3:8b"}"#,
        );
        let fakes = cleaning(Ok("hello world"));
        let (engine, _rx) = fakes.open(library_config(&dir));
        let _recording = engine.record().unwrap();
        let _text = engine.stop_and_wait(PATIENCE).unwrap();
        assert_eq!(
            *fakes.cleaner.models.lock().unwrap(),
            ["llama3:8b"],
            "chosen"
        );
    }

    const TAGS: &str = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 42\r\nConnection: close\r\n\r\n{\"models\":[{\"name\":\"b:1\"},{\"name\":\"a:1\"}]}";

    /// An engine whose Ollama answers the tags request once.
    fn listing_engine(dir: &TempDir) -> Engine {
        Fakes::default()
            .open(EngineConfig {
                ollama_url: serve_once(TAGS),
                ..config(dir)
            })
            .0
    }

    #[test]
    fn the_installed_models_are_listed() {
        let dir = TempDir::new().unwrap();
        let engine = listing_engine(&dir);
        assert_eq!(engine.ollama_models().unwrap(), ["a:1", "b:1"], "sorted");
    }

    #[test]
    fn the_installed_models_are_listed_after_shutdown() {
        let dir = TempDir::new().unwrap();
        let engine = listing_engine(&dir);
        let _report = engine.shutdown();
        assert_eq!(
            engine.ollama_models().unwrap(),
            ["a:1", "b:1"],
            "after shutdown"
        );
    }

    #[test]
    fn no_ollama_is_an_ollama_error() {
        let dir = TempDir::new().unwrap();
        let (engine, _rx) = Fakes::default().open(EngineConfig {
            ollama_url: closed_url(),
            ..config(&dir)
        });
        let listed = engine.ollama_models();
        assert!(matches!(&listed, Err(EngineError::Ollama(_))), "{listed:?}");
        let text = listed.unwrap_err().to_string();
        assert!(text.starts_with("Local AI needs Ollama: "), "{text}");
        assert!(text.ends_with('.'), "{text}");
    }

    /// The keep-warm tests' heartbeat.
    const TICK: Duration = Duration::from_millis(10);

    /// How long a test waits to see that nothing follows.
    const QUIET: Duration = Duration::from_millis(50);

    /// A config renewing the keep-alive every `every`.
    fn warm_config(dir: &TempDir, every: Duration) -> EngineConfig {
        EngineConfig {
            keep_warm_every: every,
            ..config(dir)
        }
    }

    /// One recorded warmer call.
    fn call(kind: &str, model: &str) -> (String, String) {
        (kind.to_string(), model.to_string())
    }

    /// The calls once `want` accepts them, polled every 5 ms within
    /// [`PATIENCE`].
    fn calls_until(
        warmer: &FakeWarmer,
        want: impl Fn(&[(String, String)]) -> bool,
    ) -> Vec<(String, String)> {
        let started = Instant::now();
        loop {
            let seen = warmer.seen();
            if want(&seen) {
                return seen;
            }
            assert!(started.elapsed() < PATIENCE, "in time: {seen:?}");
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// How many times `c` was recorded.
    fn count(calls: &[(String, String)], c: &(String, String)) -> usize {
        calls.iter().filter(|x| *x == c).count()
    }

    /// The settings with cleanup `on` and asking `model`.
    fn cleanup(engine: &Engine, on: bool, model: &str) -> Settings {
        Settings {
            cleanup: on,
            cleanup_model: model.to_string(),
            library_width: 220,
            input_device: String::new(),
            silence_split_ms: 0,
            keep_segments: false,
            ..engine.settings()
        }
    }

    /// Writes settings with cleanup on, asking `model`.
    fn cleanup_on(dir: &TempDir, model: &str) {
        let json = format!(r#"{{"cleanup":true,"cleanup_model":"{model}"}}"#);
        write(dir, "settings.json", json.as_bytes());
    }

    #[test]
    fn with_cleanup_off_nothing_is_warmed() {
        let dir = TempDir::new().unwrap();
        let fakes = Fakes::default();
        let (engine, _rx) = fakes.open(warm_config(&dir, TICK));
        thread::sleep(QUIET);
        let _report = engine.shutdown();
        assert!(fakes.warmer.seen().is_empty(), "no call");
    }

    #[test]
    fn with_cleanup_on_the_model_is_warmed_and_renewed() {
        let dir = TempDir::new().unwrap();
        cleanup_on(&dir, "llama3:8b");
        let fakes = Fakes::default();
        let (engine, _rx) = fakes.open(warm_config(&dir, TICK));
        let warm = call("warm", "llama3:8b");
        let seen = calls_until(&fakes.warmer, |c| count(c, &warm) >= 3);
        assert_eq!(seen.first(), Some(&warm), "warmed first");
        assert!(seen.iter().all(|c| *c == warm), "no release: {seen:?}");
        let _report = engine.shutdown();
    }

    #[test]
    fn an_unset_cleanup_model_warms_the_default() {
        let dir = TempDir::new().unwrap();
        write(&dir, "settings.json", br#"{"cleanup":true}"#);
        let fakes = Fakes::default();
        let (engine, _rx) = fakes.open(warm_config(&dir, TICK));
        let warm = call("warm", DEFAULT_SUMMARY_MODEL);
        let seen = calls_until(&fakes.warmer, |c| count(c, &warm) >= 2);
        assert_eq!(seen.first(), Some(&warm), "the default");
        let _report = engine.shutdown();
        assert!(
            fakes.warmer.seen().iter().all(|(_, m)| !m.is_empty()),
            "never unnamed"
        );
    }

    #[test]
    fn turning_cleanup_off_releases_the_model() {
        let dir = TempDir::new().unwrap();
        cleanup_on(&dir, "a:1");
        let fakes = Fakes::default();
        let (engine, _rx) = fakes.open(warm_config(&dir, TICK));
        let _warmed = calls_until(&fakes.warmer, |c| !c.is_empty());
        let _off: Settings = engine.set_settings(cleanup(&engine, false, "a:1")).unwrap();
        let release = call("release", "a:1");
        let _released = calls_until(&fakes.warmer, |c| c.contains(&release));
        thread::sleep(QUIET);
        let seen = fakes.warmer.seen();
        assert_eq!(seen.last(), Some(&release), "nothing after: {seen:?}");
        let _report = engine.shutdown();
        assert_eq!(fakes.warmer.seen(), seen, "shutdown sends nothing");
    }

    #[test]
    fn changing_the_model_releases_the_old_and_warms_the_new() {
        let dir = TempDir::new().unwrap();
        cleanup_on(&dir, "a:1");
        let fakes = Fakes::default();
        let (engine, _rx) = fakes.open(warm_config(&dir, TICK));
        let _warmed = calls_until(&fakes.warmer, |c| !c.is_empty());
        let _b: Settings = engine.set_settings(cleanup(&engine, true, "b:1")).unwrap();
        let (release, warm_a, warm_b) = (
            call("release", "a:1"),
            call("warm", "a:1"),
            call("warm", "b:1"),
        );
        let seen = calls_until(&fakes.warmer, |c| {
            c.iter()
                .position(|x| *x == release)
                .is_some_and(|i| c.iter().skip(i).any(|x| *x == warm_b))
        });
        let at = seen.iter().position(|x| *x == release).unwrap();
        assert!(
            !seen.iter().skip(at).any(|x| *x == warm_a),
            "the old one is not warmed again: {seen:?}"
        );
        let _report = engine.shutdown();
    }

    #[test]
    fn turning_cleanup_on_warms_without_reopening() {
        let dir = TempDir::new().unwrap();
        let fakes = Fakes::default();
        let (engine, _rx) = fakes.open(warm_config(&dir, TICK));
        let _on: Settings = engine.set_settings(cleanup(&engine, true, "a:1")).unwrap();
        let seen = calls_until(&fakes.warmer, |c| !c.is_empty());
        assert_eq!(seen.first(), Some(&call("warm", "a:1")), "warmed");
        let _report = engine.shutdown();
    }

    #[test]
    fn shutdown_releases_the_warm_model_once() {
        let dir = TempDir::new().unwrap();
        cleanup_on(&dir, "a:1");
        let fakes = Fakes::default();
        let (engine, _rx) = fakes.open(warm_config(&dir, TICK));
        let _warmed = calls_until(&fakes.warmer, |c| !c.is_empty());
        let _report = engine.shutdown();
        thread::sleep(QUIET);
        let seen = fakes.warmer.seen();
        assert_eq!(seen.last(), Some(&call("release", "a:1")), "released");
        thread::sleep(QUIET);
        assert_eq!(fakes.warmer.seen(), seen, "nothing follows");
        let second = engine.shutdown();
        assert!(!second.first, "not the first");
        assert_eq!(fakes.warmer.seen(), seen, "a second shutdown sends nothing");
    }

    #[test]
    fn shutdown_never_waits_for_a_warm_in_flight() {
        let dir = TempDir::new().unwrap();
        cleanup_on(&dir, "a:1");
        let (warmer, gate) = FakeWarmer::default().gated_warm();
        let fakes = Fakes {
            warmer,
            ..Fakes::default()
        };
        let (engine, _rx) = fakes.open(warm_config(&dir, TICK));
        let _warming = calls_until(&fakes.warmer, |c| !c.is_empty());
        let report = engine.shutdown();
        assert!(report.first, "shut down while the warm is blocked");
        let (warm, release) = (call("warm", "a:1"), call("release", "a:1"));
        assert_eq!(
            fakes.warmer.seen(),
            [warm.clone(), release.clone()],
            "released"
        );
        gate.send(()).unwrap();
        let _again = calls_until(&fakes.warmer, |c| c.len() >= 3);
        thread::sleep(QUIET);
        assert_eq!(
            fakes.warmer.seen(),
            [warm, release.clone(), release],
            "the finished warm is released once more, and nothing is warmed"
        );
    }

    #[test]
    fn a_failing_warmer_is_retried_and_never_stops_dictation() {
        let dir = TempDir::new().unwrap();
        cleanup_on(&dir, "a:1");
        let fakes = Fakes::default();
        fakes.warmer.fail.store(true, Ordering::SeqCst);
        let (engine, _rx) = fakes.open(warm_config(&dir, TICK));
        let warm = call("warm", "a:1");
        let _retried = calls_until(&fakes.warmer, |c| count(c, &warm) >= 2);
        let _recording = engine.record().unwrap();
        assert_eq!(
            engine.stop_and_wait(PATIENCE),
            Ok("hello world".to_string()),
            "the transcript"
        );
        let _report = engine.shutdown();
    }

    #[test]
    fn dictation_never_waits_for_a_warm() {
        let dir = TempDir::new().unwrap();
        cleanup_on(&dir, "a:1");
        write(&dir, "vocabulary.txt", b"Zed\n");
        let (warmer, _gate) = FakeWarmer::default().gated_warm();
        let fakes = Fakes {
            warmer,
            ..cleaning(Ok("Zed world"))
        };
        let (engine, _rx) = fakes.open(EngineConfig {
            keep_warm_every: TICK,
            ..library_config(&dir)
        });
        let _warming = calls_until(&fakes.warmer, |c| !c.is_empty());
        let _recording = engine.record().unwrap();
        assert_eq!(
            engine.stop_and_wait(PATIENCE),
            Ok("Zed world".to_string()),
            "cleaned while the warm is blocked"
        );
        let _report = engine.shutdown();
    }

    #[test]
    fn saving_the_same_settings_warms_nothing_more() {
        let dir = TempDir::new().unwrap();
        cleanup_on(&dir, "a:1");
        let fakes = Fakes::default();
        let (engine, _rx) = fakes.open(warm_config(&dir, Duration::from_secs(60)));
        let _warmed = calls_until(&fakes.warmer, |c| !c.is_empty());
        let _same: Settings = engine.set_settings(engine.settings()).unwrap();
        thread::sleep(QUIET);
        assert_eq!(fakes.warmer.seen(), [call("warm", "a:1")], "one warm");
        let _report = engine.shutdown();
    }

    #[test]
    fn dropping_the_engine_stops_keeping_warm() {
        let dir = TempDir::new().unwrap();
        cleanup_on(&dir, "a:1");
        let fakes = Fakes::default();
        let base = Arc::strong_count(&fakes.warmer.calls);
        let (engine, rx) = fakes.open(warm_config(&dir, TICK));
        let _warmed = calls_until(&fakes.warmer, |c| !c.is_empty());
        let _launched = until(&rx, |_| true).unwrap();
        drop(engine);
        let started = Instant::now();
        while Arc::strong_count(&fakes.warmer.calls) != base {
            assert!(started.elapsed() < PATIENCE, "the thread let go in time");
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            fakes.warmer.seen().iter().all(|(kind, _)| kind == "warm"),
            "no release: {:?}",
            fakes.warmer.seen()
        );
    }

    #[test]
    fn shutdown_during_a_release_warms_nothing_after() {
        let dir = TempDir::new().unwrap();
        cleanup_on(&dir, "a:1");
        let (warmer, gate) = FakeWarmer::default().gated_release();
        let fakes = Fakes {
            warmer,
            ..Fakes::default()
        };
        let (engine, _rx) = fakes.open(warm_config(&dir, TICK));
        let _warmed = calls_until(&fakes.warmer, |c| !c.is_empty());
        let _b: Settings = engine.set_settings(cleanup(&engine, true, "b:1")).unwrap();
        let release = call("release", "a:1");
        let _releasing = calls_until(&fakes.warmer, |c| c.contains(&release));
        let _report = engine.shutdown();
        gate.send(()).unwrap();
        thread::sleep(QUIET);
        assert!(
            !fakes.warmer.seen().contains(&call("warm", "b:1")),
            "never warmed: {:?}",
            fakes.warmer.seen()
        );
    }

    #[test]
    fn turning_cleanup_off_mid_warm_releases_at_once() {
        let dir = TempDir::new().unwrap();
        cleanup_on(&dir, "a:1");
        let (warmer, gate) = FakeWarmer::default().gated_warm();
        let fakes = Fakes {
            warmer,
            ..Fakes::default()
        };
        let (engine, _rx) = fakes.open(warm_config(&dir, Duration::from_secs(60)));
        let _warming = calls_until(&fakes.warmer, |c| !c.is_empty());
        let _off: Settings = engine.set_settings(cleanup(&engine, false, "a:1")).unwrap();
        gate.send(()).unwrap();
        let release = call("release", "a:1");
        let _released = calls_until(&fakes.warmer, |c| c.contains(&release));
        let _report = engine.shutdown();
    }

    #[test]
    fn changing_the_model_mid_warm_switches_at_once() {
        let dir = TempDir::new().unwrap();
        cleanup_on(&dir, "a:1");
        let (warmer, gate) = FakeWarmer::default().gated_warm();
        let fakes = Fakes {
            warmer,
            ..Fakes::default()
        };
        let (engine, _rx) = fakes.open(warm_config(&dir, Duration::from_secs(60)));
        let _warming = calls_until(&fakes.warmer, |c| !c.is_empty());
        let _b: Settings = engine.set_settings(cleanup(&engine, true, "b:1")).unwrap();
        gate.send(()).unwrap();
        let (release, warm_b) = (call("release", "a:1"), call("warm", "b:1"));
        let seen = calls_until(&fakes.warmer, |c| c.contains(&warm_b));
        assert_eq!(
            seen,
            [call("warm", "a:1"), release, warm_b],
            "released, then the new one"
        );
        drop(gate);
        let _report = engine.shutdown();
    }

    #[test]
    fn settings_after_shutdown_warm_nothing() {
        let dir = TempDir::new().unwrap();
        let fakes = Fakes::default();
        let (engine, _rx) = fakes.open(warm_config(&dir, TICK));
        let _report = engine.shutdown();
        assert_eq!(
            engine.set_settings(cleanup(&engine, true, "a:1")),
            Err(EngineError::ShutDown),
            "refused"
        );
        thread::sleep(QUIET);
        assert!(fakes.warmer.seen().is_empty(), "no call");
    }

    #[test]
    fn the_input_devices_are_listed_from_the_recorder() {
        let dir = TempDir::new().unwrap();
        let fakes = Fakes::default();
        let (engine, _rx) = fakes.open(config(&dir));
        assert_eq!(
            engine.input_devices(),
            Ok(InputDevices {
                default_device: Some("Fake Mic".to_string()),
                others: vec!["Other Mic".to_string()],
            }),
            "listed"
        );
        *fakes.recorder.inputs_error.lock().unwrap() = Some("Core Audio is busy".to_string());
        let listed = engine.input_devices();
        assert_eq!(
            listed,
            Err(EngineError::Microphones("Core Audio is busy".to_string())),
            "an error"
        );
        assert_eq!(
            listed.unwrap_err().to_string(),
            "Could not list the microphones: Core Audio is busy.",
            "sentence"
        );
    }

    #[test]
    fn the_chosen_input_device_is_opened_by_record() {
        let dir = TempDir::new().unwrap();
        let fakes = Fakes::default();
        let (engine, _rx) = fakes.open(config(&dir));
        let _saved = engine
            .set_settings(Settings {
                input_device: "AirPods Pro".to_string(),
                silence_split_ms: 0,
                keep_segments: false,
                ..engine.settings()
            })
            .unwrap();
        let recording = engine.record().unwrap();
        assert_eq!(
            *fakes.recorder.opened.lock().unwrap(),
            ["AirPods Pro"],
            "asked for the choice"
        );
        assert_eq!(
            recording.microphone.as_deref(),
            Some("AirPods Pro"),
            "the snapshot names it"
        );
    }

    #[expect(
        clippy::cast_possible_truncation,
        clippy::as_conversions,
        reason = "a test sine in -0.5..0.5 scaled to i16 fits"
    )]
    fn sine_i16(samples: usize) -> Vec<i16> {
        (0..samples)
            .map(|i| {
                let i = f32::from(u16::try_from(i).unwrap());
                let x = 0.5 * (2.0 * std::f32::consts::PI * 440.0 * i / 16_000.0).sin();
                (x * 32767.0).round() as i16
            })
            .collect()
    }

    #[test]
    fn open_uses_the_configs_stream_recorder_and_wires_its_level() {
        let dir = TempDir::new().unwrap();
        let (tx, rx) = mpsc::channel();
        let mut c = config(&dir);
        c.model = ModelSource::Local(dir.path().join("absent.bin"));
        let stream = StreamRecorder::new();
        c.recorder = Some(stream.clone());
        let _engine = Engine::open(c, events(Mutex::new(tx)));
        let _name = Recorder::start(&stream, "", 0).unwrap();
        for chunk in sine_i16(16_000).chunks(4_000) {
            stream.push_pcm16(chunk);
        }
        let started = Instant::now();
        loop {
            match rx.recv_timeout(PATIENCE.saturating_sub(started.elapsed())) {
                Ok(Event::Level(l)) if l > 0.0 => break,
                Ok(_) => {}
                Err(e) => panic!("no level: {e}"),
            }
        }
    }

    #[test]
    fn a_session_records_pauses_and_finishes_from_pushed_audio() {
        let dir = TempDir::new().unwrap();
        let fakes = Fakes::with(FakeTranscriber::answering(&[Ok("one"), Ok("two")]));
        let stream = StreamRecorder::new();
        let (engine, rx) = fakes.open_with_stream(config(&dir), stream.clone());
        let second = sine_i16(16_000);
        assert_eq!(engine.record().unwrap().phase, Phase::Recording, "record");
        for chunk in second.chunks(4_000) {
            stream.push_pcm16(chunk);
        }
        assert_eq!(engine.pause().unwrap().phase, Phase::Paused, "pause");
        let before = stream.dropped_samples();
        stream.push_pcm16(&second[..4_000]);
        assert_eq!(stream.dropped_samples() - before, 4_000, "paused drops");
        assert_eq!(engine.resume().unwrap().phase, Phase::Recording, "resume");
        for chunk in second.chunks(4_000) {
            stream.push_pcm16(chunk);
        }
        assert_eq!(engine.stop().unwrap().phase, Phase::Transcribing, "stop");
        assert_eq!(
            engine.wait(PATIENCE),
            Ok("one\n\ntwo".to_string()),
            "two paragraphs"
        );
        assert!(
            rx.try_iter()
                .any(|e| matches!(e, Event::Level(l) if l > 0.0)),
            "a level arrived"
        );
    }

    #[test]
    fn the_stream_recorders_label_is_the_microphone() {
        let dir = TempDir::new().unwrap();
        let (engine, _rx) = Fakes::default().open_with_stream(config(&dir), StreamRecorder::new());
        let choose = |name: &str| Settings {
            input_device: name.to_string(),
            silence_split_ms: 0,
            keep_segments: false,
            ..engine.settings()
        };
        let _saved = engine.set_settings(choose("Pixel 9 mic")).unwrap();
        let recording = engine.record().unwrap();
        assert_eq!(recording.microphone.as_deref(), Some("Pixel 9 mic"));
        assert_eq!(
            engine.input_devices().unwrap().default_device.as_deref(),
            Some("Pixel 9 mic")
        );
        let _paused = engine.pause().unwrap();
        let _cleared = engine.set_settings(choose("")).unwrap();
        let resumed = engine.resume().unwrap();
        assert_eq!(
            resumed.microphone.as_deref(),
            Some("Remote microphone"),
            "the default label"
        );
    }

    #[test]
    fn an_empty_input_device_is_saved_and_answered_as_empty() {
        let dir = TempDir::new().unwrap();
        let (engine, _rx) = Fakes::default().open(config(&dir));
        let choose = |name: &str| Settings {
            input_device: name.to_string(),
            silence_split_ms: 0,
            keep_segments: false,
            ..engine.settings()
        };
        assert_eq!(
            engine
                .set_settings(choose("AirPods Pro"))
                .unwrap()
                .input_device,
            "AirPods Pro",
            "chosen"
        );
        assert_eq!(
            engine.set_settings(choose("")).unwrap().input_device,
            "",
            "the Mac default"
        );
        assert_eq!(engine.settings().input_device, "", "read back");
        assert!(
            saved(&dir).contains(r#""input_device":"""#),
            "{}",
            saved(&dir)
        );
    }

    // The silence split.

    fn one_second() -> scriber_engine::Captured {
        scriber_engine::Captured {
            samples: vec![0.5; 16_000],
            channels: 1,
            sample_rate: 16_000,
        }
    }

    #[test]
    fn the_silence_split_is_a_setting_passed_to_the_recorder() {
        let dir = TempDir::new().unwrap();
        let fakes = Fakes::default();
        let (engine, _rx) = fakes.open(config(&dir));
        let split = |ms| Settings {
            silence_split_ms: ms,
            ..engine.settings()
        };
        assert_eq!(
            engine.set_settings(split(500)).unwrap().silence_split_ms,
            0,
            "out of range"
        );
        assert_eq!(engine.settings().silence_split_ms, 0, "read back");
        assert_eq!(
            engine.set_settings(split(2_500)).unwrap().silence_split_ms,
            2_500,
            "kept"
        );
        assert_eq!(engine.settings().silence_split_ms, 2_500, "read back");
        let _recording = engine.record().unwrap();
        assert_eq!(*fakes.recorder.splits.lock().unwrap(), [2_500], "passed");
    }

    #[test]
    fn a_segment_closed_at_a_silence_transcribes_while_recording_goes_on() {
        let dir = TempDir::new().unwrap();
        let (transcriber, gate) = FakeTranscriber::answering(&[Ok("One."), Ok("Two.")]).gated();
        let fakes = Fakes::with(transcriber);
        let (engine, rx) = fakes.open(config(&dir));
        let _recording = engine.record().unwrap();
        fakes.recorder.close_at_silence(one_second());
        let split = until(&rx, |s| s.segments == 1).unwrap();
        assert_eq!(
            (
                split.phase,
                split.segments,
                split.pending,
                split.recorded_ms
            ),
            (Phase::Recording, 1, 1, 1_000),
            "recording goes on"
        );
        assert_eq!(
            (
                fakes.recorder.starts.load(Ordering::SeqCst),
                fakes.recorder.stops.load(Ordering::SeqCst)
            ),
            (1, 0),
            "the device was never stopped"
        );
        gate.send(()).unwrap();
        gate.send(()).unwrap();
        assert_eq!(
            engine.stop_and_wait(PATIENCE),
            Ok("One.\n\nTwo.".to_string()),
            "both parts"
        );
        assert_eq!(fakes.recorder.stops.load(Ordering::SeqCst), 1, "one stop");
    }

    #[test]
    fn a_split_after_the_engine_is_gone_does_nothing() {
        let dir = TempDir::new().unwrap();
        let fakes = Fakes::default();
        let (engine, rx) = fakes.open(config(&dir));
        let _report = engine.shutdown();
        drop(engine);
        let deadline = Instant::now() + PATIENCE;
        while !fakes.transcriber.dropped.is_set() {
            assert!(Instant::now() < deadline, "dropped in time");
            thread::sleep(Duration::from_millis(10));
        }
        while rx.try_recv().is_ok() {}
        fakes.recorder.close_at_silence(one_second());
        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "no event"
        );
    }
}
