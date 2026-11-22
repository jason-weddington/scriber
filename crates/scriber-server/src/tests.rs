//! In-process WebSocket loopback, HTTP, lifecycle, concurrency and diagnostics
//! tests for `scriber-server`.

mod fakes;

use std::collections::HashMap;
use std::future::Future;
use std::net::{SocketAddr, TcpListener as StdTcpListener};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::Duration;

use scriber_core::model::{Action, LibraryEntry, Phase, Settings, Snapshot};
use scriber_engine::{DataDir, Event, ModelSource, ShutdownReport, Source, StreamRecorder};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::{Notify, oneshot};

use crate::diagnostics::{AudioCounts, DiagnosticEvent, Diagnostics};
use crate::server::{
    BoxWrite, CLOSE_INVALID_PAYLOAD, CLOSE_MESSAGE_TOO_BIG, CLOSE_NORMAL, CLOSE_PROTOCOL_ERROR,
    CLOSE_UNAUTHORIZED, DICTATION_EVENT, EventHub, LEVEL_EVENT, MAX_WS_PAYLOAD, NOT_AVAILABLE_ERR,
    NoopHooks, SERVER_VERSION, SUBSCRIBER_QUEUE_CAPACITY, ServerHooks, ServerState, WsReadError,
    WsReader, action_name, constant_time_eq, dispatch_text_rpc, format_filtered_event,
    handle_connection, handle_pcm, phase_name, read_static_asset, router, run_blocking,
    websocket_accept_key, ws_upgrade, ws_write_frame,
};
use crate::tests::fakes::{
    ClientFrame, FakeDownloader, FakeEngineBundle, FakeTranscriber, TestWsClient,
    closed_loopback_url, http_get, loud_pcm16, pcm16_le_bytes, spawn_http_stub,
};
use crate::{
    AuthMode, DEFAULT_BIND, DEFAULT_DATA_SUBPATH, DEFAULT_WEB_DIR, OsSignals, ServerConfig,
    StartupError, main as entry_main, open_production_engine, persist_model_folder,
    production_engine_config, run_cli, run_lifecycle, run_production, select_shutdown_trigger,
    serve_axum,
};

const WIRE_FIXTURE: &str = include_str!("../tests/fixtures/wire.json");
const BRIDGE_FIXTURE: &str = include_str!("../../scriber-core/tests/fixtures/bridge-shapes.json");
const TEST_TOKEN: &str = "top-secret-scriber-token-999";

struct TestServer {
    addr: SocketAddr,
    stop_tx: Option<oneshot::Sender<&'static str>>,
    handle: Option<tokio::task::JoinHandle<Result<ShutdownReport, StartupError>>>,
    diagnostics: Arc<Mutex<Vec<DiagnosticEvent>>>,
}

impl TestServer {
    async fn start(
        config: ServerConfig,
        bundle: FakeEngineBundle,
        hooks: Arc<dyn ServerHooks>,
    ) -> Self {
        let (diag, events) = Diagnostics::recording();
        let (bound_tx, bound_rx) = oneshot::channel();
        let (stop_tx, stop_rx) = oneshot::channel();
        let handle = tokio::spawn(async move {
            run_lifecycle(
                config,
                diag,
                hooks,
                Box::new(move |data_dir, recorder, ev| bundle.open(data_dir, recorder, ev)),
                Box::new(move |addr| {
                    let _sent = bound_tx.send(addr);
                }),
                Box::pin(async move { stop_rx.await.unwrap_or("signal") }),
                Box::new(|l, a, s| Box::pin(serve_axum(l, a, s))),
            )
            .await
        });
        let addr = bound_rx.await.expect("server bound address");
        Self {
            addr,
            stop_tx: Some(stop_tx),
            handle: Some(handle),
            diagnostics: events,
        }
    }

    async fn stop(&mut self, trigger: &'static str) -> ShutdownReport {
        if let Some(tx) = self.stop_tx.take() {
            let _sent = tx.send(trigger);
        }
        self.handle
            .take()
            .unwrap()
            .await
            .expect("server task joined")
            .expect("server exited cleanly")
    }

    fn events(&self) -> Vec<DiagnosticEvent> {
        self.diagnostics
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

fn base_config(dir: &TempDir, web_dir: PathBuf, auth: AuthMode) -> ServerConfig {
    ServerConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        auth,
        data_dir: dir.path().join("data"),
        model_folder: None,
        web_dir,
        model_source: ModelSource::default(),
    }
}

type EnvLookup = Box<dyn Fn(&str) -> Option<String>>;

fn env_map(pairs: &[(&str, &str)]) -> EnvLookup {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    Box::new(move |key: &str| map.get(key).cloned())
}

async fn finish_done(client: &mut TestWsClient, id: u32) -> Value {
    let (done_rep, done_evs) = client
        .call(id, "dictation_act", json!({ "action": "done", "text": "" }))
        .await;
    if done_rep["ok"]["phase"] == "finished" {
        return done_rep["ok"].clone();
    }
    if let Some(ev) = done_evs.iter().find(|e| {
        e.get("event").and_then(Value::as_str) == Some(DICTATION_EVENT)
            && e["payload"]["phase"] == "finished"
    }) {
        return ev["payload"].clone();
    }
    for _ in 0..100 {
        let msg = client.recv_json().await;
        if msg.get("event").and_then(Value::as_str) == Some(DICTATION_EVENT)
            && msg["payload"]["phase"] == "finished"
        {
            return msg["payload"].clone();
        }
    }
    panic!("did not observe finished phase");
}

// ---------------------------------------------------------------------------
// AC 2 & AC 3: Config parsing, defaults, overrides, desktop isolation, auth
// ---------------------------------------------------------------------------

#[test]
fn config_defaults_overrides_and_desktop_isolation() {
    let cwd = Path::new("/work/repo");
    let cfg = ServerConfig::from_env(
        &*env_map(&[("HOME", "/Users/alice"), ("SCRIBER_TOKEN", "s3cr3t")]),
        cwd,
    )
    .expect("default config parses");
    assert_eq!(
        cfg.bind,
        DEFAULT_BIND.parse::<SocketAddr>().unwrap(),
        "default bind is 127.0.0.1:8742"
    );
    assert_eq!(
        cfg.data_dir,
        PathBuf::from("/Users/alice").join(DEFAULT_DATA_SUBPATH),
        "default data dir under HOME"
    );
    assert_ne!(
        cfg.data_dir,
        PathBuf::from("/Users/alice/Library/Application Support/com.jasonweddington.scriber"),
        "default data dir is isolated from desktop app folder"
    );
    assert_eq!(
        cfg.web_dir,
        cwd.join(DEFAULT_WEB_DIR),
        "default web dir is cwd-relative apps/desktop/dist-web"
    );
    assert_eq!(
        cfg.auth,
        AuthMode::Token("s3cr3t".to_string()),
        "token auth mode"
    );
    assert_eq!(cfg.auth.as_str(), "token", "token auth label");
    assert_eq!(
        cfg.model_folder, None,
        "no model folder override by default"
    );

    let overridden = ServerConfig::from_env(
        &*env_map(&[
            ("SCRIBER_TOKEN", "override-secret"),
            ("SCRIBER_ALLOW_NO_TOKEN", "1"),
            ("SCRIBER_BIND", "127.0.0.1:9999"),
            ("SCRIBER_DATA", "/srv/scriber-data"),
            ("SCRIBER_WEB_DIR", "custom/web"),
            ("SCRIBER_MODEL_FOLDER", "/shared/models"),
        ]),
        cwd,
    )
    .expect("overridden config parses");
    assert_eq!(
        overridden.bind,
        "127.0.0.1:9999".parse::<SocketAddr>().unwrap(),
        "SCRIBER_BIND override"
    );
    assert_eq!(
        overridden.data_dir,
        PathBuf::from("/srv/scriber-data"),
        "SCRIBER_DATA override"
    );
    assert_eq!(
        overridden.web_dir,
        PathBuf::from("/work/repo/custom/web"),
        "SCRIBER_WEB_DIR override"
    );
    assert_eq!(
        overridden.model_folder.as_deref(),
        Some("/shared/models"),
        "SCRIBER_MODEL_FOLDER override"
    );
    assert_eq!(
        overridden.auth,
        AuthMode::Token("override-secret".to_string()),
        "nonempty SCRIBER_TOKEN wins over SCRIBER_ALLOW_NO_TOKEN=1"
    );
}

#[test]
fn config_token_requirements_and_validation_errors() {
    let cwd = Path::new("/work");
    let missing = ServerConfig::from_env(&*env_map(&[("HOME", "/Users/a")]), cwd).unwrap_err();
    assert_eq!(
        (missing.stage, missing.reason),
        ("config", "missing_token"),
        "missing token fails before engine startup"
    );
    assert!(
        missing.to_string().contains("SCRIBER_TOKEN"),
        "display mentions SCRIBER_TOKEN"
    );

    let empty = ServerConfig::from_env(
        &*env_map(&[("HOME", "/Users/a"), ("SCRIBER_TOKEN", "")]),
        cwd,
    )
    .unwrap_err();
    assert_eq!(empty.reason, "missing_token", "empty token fails");

    let allow_zero = ServerConfig::from_env(
        &*env_map(&[
            ("HOME", "/Users/a"),
            ("SCRIBER_TOKEN", ""),
            ("SCRIBER_ALLOW_NO_TOKEN", "0"),
        ]),
        cwd,
    )
    .unwrap_err();
    assert_eq!(
        allow_zero.reason, "missing_token",
        "SCRIBER_ALLOW_NO_TOKEN=0 does not enable tokenless mode"
    );

    let tokenless = ServerConfig::from_env(
        &*env_map(&[
            ("HOME", "/Users/a"),
            ("SCRIBER_TOKEN", ""),
            ("SCRIBER_ALLOW_NO_TOKEN", "1"),
        ]),
        cwd,
    )
    .expect("tokenless config allowed");
    assert_eq!(tokenless.auth, AuthMode::NoToken, "NoToken mode");
    assert_eq!(tokenless.auth.as_str(), "no_token", "no_token label");
    assert_eq!(tokenless.auth.expected_token(), "", "empty expected token");

    let bad_bind = ServerConfig::from_env(
        &*env_map(&[
            ("HOME", "/Users/a"),
            ("SCRIBER_TOKEN", "tok"),
            ("SCRIBER_BIND", "not-an-addr"),
        ]),
        cwd,
    )
    .unwrap_err();
    assert_eq!(bad_bind.reason, "invalid_bind", "invalid bind rejected");

    let no_home = ServerConfig::from_env(&*env_map(&[("SCRIBER_TOKEN", "tok")]), cwd).unwrap_err();
    assert_eq!(no_home.reason, "missing_home", "missing HOME rejected");

    let rel_model = ServerConfig::from_env(
        &*env_map(&[
            ("HOME", "/Users/a"),
            ("SCRIBER_TOKEN", "tok"),
            ("SCRIBER_MODEL_FOLDER", "relative/models"),
        ]),
        cwd,
    )
    .unwrap_err();
    assert_eq!(
        rel_model.reason, "relative_model_folder",
        "relative model folder rejected"
    );

    let cfg_io = StartupError::config_io(std::io::Error::other("runtime init failed"));
    assert_eq!(
        (cfg_io.stage, cfg_io.reason),
        ("config", "runtime_failed"),
        "config_io maps runtime/signal IO errors to config/runtime_failed"
    );
    assert!(
        cfg_io.message.contains("runtime init failed"),
        "config_io preserves error message"
    );
}

// ---------------------------------------------------------------------------
// AC 4: SCRIBER_MODEL_FOLDER persistence & fake downloader target
// ---------------------------------------------------------------------------

async fn query_model_folder(addr: SocketAddr) -> Settings {
    let mut client = TestWsClient::connect(addr).await;
    let (_hello, _boot) = client.hello(TEST_TOKEN).await;
    let (reply, _) = client.call(1, "settings_get", json!({})).await;
    serde_json::from_value(reply["ok"].clone()).unwrap()
}

#[tokio::test]
async fn model_folder_startup_persistence_preservation_and_download() {
    let dir = TempDir::new().unwrap();
    let data_dir = dir.path().join("data");
    let env_models = dir.path().join("env-models");

    let files = DataDir::new(data_dir.clone());
    let mut initial = scriber_core::settings::load(&files);
    initial.summaries = false;
    initial.cleanup = true;
    initial.library_width = 333;
    initial.silence_split_ms = 2_500;
    initial.copy_sound = false;
    scriber_core::settings::save(&files, &initial).unwrap();

    let model_bytes = b"fake-whisper-model-bytes".to_vec();
    let sha = format!("{:x}", Sha256::digest(&model_bytes));
    let expected_model_file = env_models.join("ggml-large-v3-turbo.bin");
    let bundle = FakeEngineBundle {
        transcriber: FakeTranscriber {
            model: Some(expected_model_file.clone()),
            ..FakeTranscriber::default()
        },
        downloader: FakeDownloader {
            body: model_bytes.clone(),
            opens: Arc::new(AtomicU32::new(0)),
        },
        model_source: Some(ModelSource::Download(Source {
            url: "https://fake.invalid/model.bin".to_string(),
            bytes: u64::try_from(model_bytes.len()).unwrap(),
            sha256: sha,
        })),
        ollama_url: None,
    };

    let mut cfg = base_config(
        &dir,
        dir.path().join("web"),
        AuthMode::Token(TEST_TOKEN.into()),
    );
    cfg.data_dir = data_dir;
    cfg.model_folder = Some(env_models.to_string_lossy().into_owned());
    let mut srv = TestServer::start(cfg, bundle.clone(), Arc::new(NoopHooks)).await;

    let got = query_model_folder(srv.addr).await;
    assert_eq!(
        got.model_folder,
        env_models.to_string_lossy(),
        "SCRIBER_MODEL_FOLDER persisted before Engine::open"
    );
    assert!(!got.summaries, "existing summaries preserved");
    assert!(got.cleanup, "existing cleanup preserved");
    assert_eq!(got.library_width, 333, "existing library_width preserved");
    assert_eq!(
        got.silence_split_ms, 2_500,
        "existing silence_split_ms preserved"
    );
    assert!(!got.copy_sound, "existing copy_sound preserved");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !expected_model_file.is_file() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "downloader wrote model into SCRIBER_MODEL_FOLDER"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        std::fs::read(&expected_model_file).unwrap(),
        model_bytes,
        "downloaded model bytes match"
    );
    assert!(
        bundle.downloader.opens.load(Ordering::SeqCst) >= 1,
        "fake downloader ran"
    );
    let _rep = srv.stop("signal").await;
}

#[tokio::test]
async fn model_folder_settings_set_and_restart_precedence() {
    let dir = TempDir::new().unwrap();
    let data_dir = dir.path().join("data");
    let env_models = dir.path().join("env-models");
    let custom_models = dir.path().join("custom-models");

    let mut cfg1 = base_config(
        &dir,
        dir.path().join("web"),
        AuthMode::Token(TEST_TOKEN.into()),
    );
    cfg1.data_dir = data_dir.clone();
    cfg1.model_folder = Some(env_models.to_string_lossy().into_owned());
    let mut srv1 = TestServer::start(cfg1, FakeEngineBundle::default(), Arc::new(NoopHooks)).await;

    let mut client = TestWsClient::connect(srv1.addr).await;
    let (_h1, _b1) = client.hello(TEST_TOKEN).await;
    let (get_rep, _) = client.call(1, "settings_get", json!({})).await;
    let mut updated: Settings = serde_json::from_value(get_rep["ok"].clone()).unwrap();
    updated.model_folder = custom_models.to_string_lossy().into_owned();
    let (set_rep, _) = client
        .call(2, "settings_set", json!({ "settings": updated }))
        .await;
    let saved: Settings = serde_json::from_value(set_rep["ok"].clone()).unwrap();
    assert_eq!(
        saved.model_folder,
        custom_models.to_string_lossy(),
        "settings_set changes model_folder in session"
    );
    drop(client);
    let _rep1 = srv1.stop("signal").await;

    // Restart without SCRIBER_MODEL_FOLDER -> retains last saved folder.
    let mut cfg2 = base_config(
        &dir,
        dir.path().join("web"),
        AuthMode::Token(TEST_TOKEN.into()),
    );
    cfg2.data_dir = data_dir.clone();
    let mut srv2 = TestServer::start(cfg2, FakeEngineBundle::default(), Arc::new(NoopHooks)).await;
    assert_eq!(
        query_model_folder(srv2.addr).await.model_folder,
        custom_models.to_string_lossy(),
        "restart without env retains last saved folder"
    );
    let _rep2 = srv2.stop("signal").await;

    // Next env-configured launch overrides it again.
    let mut cfg3 = base_config(
        &dir,
        dir.path().join("web"),
        AuthMode::Token(TEST_TOKEN.into()),
    );
    cfg3.data_dir = data_dir;
    cfg3.model_folder = Some(env_models.to_string_lossy().into_owned());
    let mut srv3 = TestServer::start(cfg3, FakeEngineBundle::default(), Arc::new(NoopHooks)).await;
    assert_eq!(
        query_model_folder(srv3.addr).await.model_folder,
        env_models.to_string_lossy(),
        "next env-configured launch overrides model_folder again"
    );
    let _rep3 = srv3.stop("signal").await;
}

#[tokio::test]
async fn model_folder_save_error_fails_before_engine_open() {
    let dir = TempDir::new().unwrap();
    let blocking_file = dir.path().join("not-a-dir");
    std::fs::write(&blocking_file, b"file").unwrap();

    let (diag, events) = Diagnostics::recording();
    let opened = Arc::new(AtomicBool::new(false));
    let opened_flag = Arc::clone(&opened);
    let mut cfg = base_config(&dir, dir.path().join("web"), AuthMode::NoToken);
    cfg.data_dir = blocking_file;
    cfg.model_folder = Some("/abs/models".to_string());

    let err = run_lifecycle(
        cfg,
        diag,
        Arc::new(NoopHooks),
        Box::new(move |data_dir, recorder, ev| {
            opened_flag.store(true, Ordering::SeqCst);
            FakeEngineBundle::default().open(data_dir, recorder, ev)
        }),
        Box::new(|_addr| {}),
        Box::pin(async { "signal" }),
        Box::new(|l, a, s| Box::pin(serve_axum(l, a, s))),
    )
    .await
    .unwrap_err();
    assert_eq!(
        (err.stage, err.reason),
        ("model_folder", "settings_save_failed"),
        "save error fails at model_folder stage"
    );
    assert!(
        !opened.load(Ordering::SeqCst),
        "engine was never opened when model_folder save failed"
    );
    assert!(
        events
            .lock()
            .unwrap()
            .contains(&DiagnosticEvent::StartupFailed {
                stage: "model_folder",
                reason: "settings_save_failed",
            }),
        "diagnostic recorded model_folder save failure"
    );
    assert!(
        persist_model_folder(dir.path(), None).is_ok(),
        "None model_folder is a no-op"
    );
}

// ---------------------------------------------------------------------------
// AC 5: Lifecycle orchestration (bind failure, serve failure, shutdown, OS signals)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn lifecycle_bind_failure_does_not_open_engine() {
    let dir = TempDir::new().unwrap();
    let occupied = StdTcpListener::bind("127.0.0.1:0").unwrap();
    let addr = occupied.local_addr().unwrap();

    let (diag, events) = Diagnostics::recording();
    let opened = Arc::new(AtomicBool::new(false));
    let opened_flag = Arc::clone(&opened);
    let mut cfg = base_config(&dir, dir.path().join("web"), AuthMode::NoToken);
    cfg.bind = addr;

    let err = run_lifecycle(
        cfg,
        diag,
        Arc::new(NoopHooks),
        Box::new(move |data_dir, recorder, ev| {
            opened_flag.store(true, Ordering::SeqCst);
            FakeEngineBundle::default().open(data_dir, recorder, ev)
        }),
        Box::new(|_addr| {}),
        Box::pin(async { "signal" }),
        Box::new(|l, a, s| Box::pin(serve_axum(l, a, s))),
    )
    .await
    .unwrap_err();
    assert_eq!(
        (err.stage, err.reason),
        ("bind", "bind_failed"),
        "bind failure reported at bind stage"
    );
    assert!(
        !opened.load(Ordering::SeqCst),
        "engine factory not invoked on bind failure"
    );
    assert!(
        events
            .lock()
            .unwrap()
            .contains(&DiagnosticEvent::StartupFailed {
                stage: "bind",
                reason: "bind_failed",
            }),
        "bind failure diagnostic emitted"
    );
    drop(occupied);
}

#[tokio::test]
async fn lifecycle_injected_serve_failure_shuts_down_and_unloads_engine() {
    let dir = TempDir::new().unwrap();
    let (diag, events) = Diagnostics::recording();
    let bundle = FakeEngineBundle::default();
    let transcriber = bundle.transcriber.clone();
    let cfg = base_config(&dir, dir.path().join("web"), AuthMode::NoToken);

    let err = run_lifecycle(
        cfg,
        diag,
        Arc::new(NoopHooks),
        Box::new(move |data_dir, recorder, ev| {
            let eng = bundle.open(data_dir, recorder, ev);
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while bundle.transcriber.warms.load(Ordering::SeqCst) == 0
                && std::time::Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            eng
        }),
        Box::new(|_addr| {}),
        Box::pin(std::future::pending::<&'static str>()),
        Box::new(|_listener, _app, _stop_rx| {
            Box::pin(async { Err(std::io::Error::other("injected serve error")) })
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(
        (err.stage, err.reason),
        ("serve", "serve_failed"),
        "serve failure stage and reason"
    );
    assert_eq!(
        transcriber.unloads.load(Ordering::SeqCst),
        1,
        "fake transcriber unloaded after injected serve failure"
    );
    let recorded = events.lock().unwrap().clone();
    assert!(
        recorded.contains(&DiagnosticEvent::StartupFailed {
            stage: "serve",
            reason: "serve_failed",
        }),
        "serve failure diagnostic emitted"
    );
    assert!(
        recorded.contains(&DiagnosticEvent::Shutdown {
            trigger: "serve_error",
            first: true,
            paused_recording: false,
            freed_model: true,
        }),
        "shutdown diagnostic records serve_error and freed_model"
    );

    let (diag2, events2) = Diagnostics::recording();
    let report = run_lifecycle(
        base_config(&dir, dir.path().join("web"), AuthMode::NoToken),
        diag2,
        Arc::new(NoopHooks),
        Box::new(move |data_dir, recorder, ev| {
            FakeEngineBundle::default().open(data_dir, recorder, ev)
        }),
        Box::new(|_addr| {}),
        Box::pin(std::future::pending::<&'static str>()),
        Box::new(|_listener, _app, _stop_rx| Box::pin(async { Ok(()) })),
    )
    .await
    .expect("clean serve exit returns report");
    assert!(report.first, "first shutdown");
    assert!(
        events2.lock().unwrap().iter().any(|e| matches!(
            e,
            DiagnosticEvent::Shutdown {
                trigger: "signal",
                first: true,
                ..
            }
        )),
        "clean serve exit emits shutdown diagnostic"
    );
}

#[tokio::test]
async fn lifecycle_shutdown_with_open_recording_owner_retains_audio_and_unloads() {
    let dir = TempDir::new().unwrap();
    let bundle = FakeEngineBundle::default();
    let transcriber = bundle.transcriber.clone();
    let cfg = base_config(
        &dir,
        dir.path().join("web"),
        AuthMode::Token(TEST_TOKEN.into()),
    );
    let data_dir = cfg.data_dir.clone();
    let mut srv = TestServer::start(cfg, bundle, Arc::new(NoopHooks)).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while transcriber.warms.load(Ordering::SeqCst) == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "fake model warmed up"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let mut owner = TestWsClient::connect(srv.addr).await;
    let (_h, _b) = owner.hello(TEST_TOKEN).await;
    let (r_new, _) = owner
        .call(1, "dictation_act", json!({ "action": "new", "text": "" }))
        .await;
    assert_eq!(r_new["ok"]["phase"], "ready", "session ready");
    let (r_rec, _) = owner
        .call(
            2,
            "dictation_act",
            json!({ "action": "record", "text": "" }),
        )
        .await;
    assert_eq!(r_rec["ok"]["phase"], "recording", "session recording");

    owner.send_binary(&pcm16_le_bytes(&loud_pcm16(8_000))).await;
    let (_r_state, _) = owner.call(3, "dictation_state", json!({})).await;
    let mut idle_unauth = TestWsClient::connect(srv.addr).await;

    let report = srv.stop("sigterm").await;
    assert!(report.first, "first shutdown report");
    assert!(
        report.paused_recording,
        "shutdown paused active recording to retain audio"
    );
    assert!(report.freed_model, "shutdown freed loaded speech model");
    assert_eq!(
        transcriber.unloads.load(Ordering::SeqCst),
        1,
        "fake transcriber unload called before exit"
    );

    let owner_end = owner.recv().await;
    assert!(
        matches!(owner_end, ClientFrame::Close(Some(1001)) | ClientFrame::Eof),
        "owner socket closed on shutdown, got {owner_end:?}"
    );
    let unauth_end = idle_unauth.recv().await;
    assert!(
        matches!(
            unauth_end,
            ClientFrame::Close(Some(1001)) | ClientFrame::Eof
        ),
        "pre-auth socket closed on shutdown, got {unauth_end:?}"
    );
    let seg_wav = data_dir.join("segments/segment-0000.wav");
    assert!(
        seg_wav.is_file(),
        "recorded audio retained on disk at {seg_wav:?}"
    );
}

#[test]
fn production_engine_config_and_cli_entry() {
    let dir = TempDir::new().unwrap();
    let cfg = production_engine_config(
        dir.path().join("data"),
        ModelSource::default(),
        StreamRecorder::new(),
    );
    assert!(cfg.recorder.is_some(), "StreamRecorder attached");
    assert!(cfg.clipboard.is_none(), "clipboard is None");
    assert!(cfg.library, "library is enabled");
    assert_eq!(
        cfg.model,
        ModelSource::default(),
        "default downloadable model source"
    );

    let mut local_cfg = production_engine_config(
        dir.path().join("local-data"),
        ModelSource::Local(dir.path().join("absent-model.bin")),
        StreamRecorder::new(),
    );
    assert_eq!(
        local_cfg.model,
        ModelSource::Local(dir.path().join("absent-model.bin")),
        "custom model source preserved in EngineConfig"
    );
    local_cfg.context_lexicon = None;
    let engine = open_production_engine(local_cfg, Arc::new(|_ev| {}));
    assert_eq!(
        engine.state().phase,
        Phase::Idle,
        "offline production engine opens idle"
    );
    let _rep = engine.shutdown();

    let (diag, events) = Diagnostics::recording();
    let fail_cfg = run_cli(
        &*env_map(&[]),
        dir.path(),
        diag.clone(),
        Box::new(|_c, _d| {
            panic!("runner must not run on bad config");
        }),
    );
    assert_eq!(fail_cfg, ExitCode::FAILURE, "bad config returns FAILURE");
    let fail_run = run_cli(
        &*env_map(&[("HOME", "/tmp"), ("SCRIBER_ALLOW_NO_TOKEN", "1")]),
        dir.path(),
        diag.clone(),
        Box::new(|_c, _d| Err(StartupError::new("bind", "bind_failed", "busy"))),
    );
    assert_eq!(fail_run, ExitCode::FAILURE, "runner error returns FAILURE");
    let ok_run = run_cli(
        &*env_map(&[("HOME", "/tmp"), ("SCRIBER_ALLOW_NO_TOKEN", "1")]),
        dir.path(),
        diag,
        Box::new(|_c, _d| {
            Ok(ShutdownReport {
                first: true,
                paused_recording: false,
                freed_model: false,
            })
        }),
    );
    assert_eq!(ok_run, ExitCode::SUCCESS, "runner ok returns SUCCESS");
    assert!(
        events
            .lock()
            .unwrap()
            .contains(&DiagnosticEvent::StartupFailed {
                stage: "config",
                reason: "missing_token",
            }),
        "run_cli emitted config StartupFailed"
    );
    if std::env::var("SCRIBER_TOKEN").is_err() && std::env::var("SCRIBER_ALLOW_NO_TOKEN").is_err() {
        assert_eq!(
            entry_main(),
            ExitCode::FAILURE,
            "main() fails cleanly without SCRIBER_TOKEN"
        );
    }
}

#[test]
fn runtime_wrapper_and_os_signals() {
    let dir = TempDir::new().unwrap();
    let occupied = StdTcpListener::bind("127.0.0.1:0").unwrap();
    let (rt_diag, _rt_events) = Diagnostics::recording();
    let rt_cfg = ServerConfig {
        bind: occupied.local_addr().unwrap(),
        auth: AuthMode::NoToken,
        data_dir: dir.path().join("rt-data"),
        model_folder: None,
        web_dir: dir.path().join("web"),
        model_source: ModelSource::Local(dir.path().join("absent.bin")),
    };
    let prod_err = run_production(rt_cfg.clone(), rt_diag).unwrap_err();
    assert_eq!(
        prod_err.reason, "bind_failed",
        "run_production propagates bind failure"
    );
    drop(occupied);

    // Run run_production end-to-end with real SIGTERM triggered on StartupReady.
    let mut offline_cfg = rt_cfg;
    offline_cfg.bind = "127.0.0.1:0".parse().unwrap();
    #[cfg(unix)]
    {
        let fired = Arc::new(AtomicBool::new(false));
        let fired_cb = Arc::clone(&fired);
        let fired_wd = Arc::clone(&fired);
        let prod_diag = Diagnostics::new(move |ev| {
            if matches!(ev, DiagnosticEvent::StartupReady { .. })
                && !fired_cb.swap(true, Ordering::SeqCst)
            {
                let pid = std::process::id().to_string();
                let _sent = std::process::Command::new("kill")
                    .args(["-TERM", &pid])
                    .status();
            }
        });
        let wd_done = Arc::new(AtomicBool::new(false));
        let wd_done_bg = Arc::clone(&wd_done);
        let watchdog = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while !wd_done_bg.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            if !wd_done_bg.load(Ordering::SeqCst) && !fired_wd.load(Ordering::SeqCst) {
                let pid = std::process::id().to_string();
                let _sent = std::process::Command::new("kill")
                    .args(["-TERM", &pid])
                    .status();
            }
        });
        let prod_rep = run_production(offline_cfg, prod_diag)
            .expect("run_production shuts down cleanly on SIGTERM");
        wd_done.store(true, Ordering::SeqCst);
        let _wd = watchdog.join();
        assert!(
            fired.load(Ordering::SeqCst),
            "StartupReady diagnostic must fire"
        );
        assert!(prod_rep.first, "run_production first shutdown");
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        assert_eq!(
            select_shutdown_trigger(
                Box::pin(std::future::ready(())),
                Box::pin(std::future::pending()),
            )
            .await,
            "ctrl_c",
            "ctrl_c branch"
        );
        assert_eq!(
            select_shutdown_trigger(
                Box::pin(std::future::pending()),
                Box::pin(std::future::ready(())),
            )
            .await,
            "sigterm",
            "sigterm branch"
        );
        #[cfg(unix)]
        {
            let pid = std::process::id().to_string();
            let sigs_term = OsSignals::install().unwrap();
            let _term_status = std::process::Command::new("kill")
                .args(["-TERM", &pid])
                .status()
                .unwrap();
            assert_eq!(sigs_term.wait().await, "sigterm", "real SIGTERM handled");

            let sigs_int = OsSignals::install().unwrap();
            let _int_status = std::process::Command::new("kill")
                .args(["-INT", &pid])
                .status()
                .unwrap();
            assert_eq!(sigs_int.wait().await, "ctrl_c", "real SIGINT handled");
        }
    });
}

// ---------------------------------------------------------------------------
// AC 6: /healthz and static serving containment (traversal, symlinks, missing build)
// ---------------------------------------------------------------------------

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "end-to-end test verifies healthz, static files, traversal and symlink containment together"
)]
async fn healthz_and_static_serving_and_symlink_containment() {
    let dir = TempDir::new().unwrap();
    let web_dir = dir.path().join("web");
    let outside_dir = dir.path().join("outside");
    std::fs::create_dir_all(web_dir.join("assets")).unwrap();
    std::fs::create_dir_all(&outside_dir).unwrap();

    std::fs::write(web_dir.join("index.html"), "<h1>Scriber Web</h1>").unwrap();
    std::fs::write(web_dir.join("assets/app.js"), "console.log('ok');").unwrap();
    std::fs::write(web_dir.join("assets/style.css"), "body{}").unwrap();
    std::fs::write(web_dir.join("manifest.webmanifest"), "{}").unwrap();
    std::fs::write(web_dir.join("icon.svg"), "<svg/>").unwrap();
    std::fs::write(web_dir.join("icon.png"), [0x89, 0x50, 0x4E, 0x47]).unwrap();
    std::fs::write(web_dir.join("favicon.ico"), [0, 0, 1, 0]).unwrap();
    std::fs::write(web_dir.join("info.json"), "{\"v\":1}").unwrap();
    std::fs::write(web_dir.join("readme.txt"), "hello").unwrap();
    std::fs::write(web_dir.join("raw.bin"), [1, 2, 3]).unwrap();
    std::fs::write(outside_dir.join("secret.txt"), "TOP SECRET").unwrap();

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            outside_dir.join("secret.txt"),
            web_dir.join("escape-link.txt"),
        )
        .unwrap();
        std::os::unix::fs::symlink(web_dir.join("readme.txt"), web_dir.join("inside-link.txt"))
            .unwrap();
    }

    let cfg = base_config(&dir, web_dir.clone(), AuthMode::Token(TEST_TOKEN.into()));
    let mut srv = TestServer::start(cfg, FakeEngineBundle::default(), Arc::new(NoopHooks)).await;

    let (status, headers, body) = http_get(srv.addr, "/healthz").await;
    assert_eq!(status, 200, "/healthz is 200");
    assert_eq!(body, b"ok", "/healthz body is ok");
    assert!(!headers.contains(TEST_TOKEN), "no token in headers");

    for (path, want_mime, want_body) in [
        (
            "/",
            "text/html; charset=utf-8",
            b"<h1>Scriber Web</h1>".as_slice(),
        ),
        (
            "/assets/app.js",
            "text/javascript; charset=utf-8",
            b"console.log('ok');".as_slice(),
        ),
        (
            "/assets/style.css",
            "text/css; charset=utf-8",
            b"body{}".as_slice(),
        ),
        (
            "/manifest.webmanifest",
            "application/manifest+json",
            b"{}".as_slice(),
        ),
        ("/icon.svg", "image/svg+xml", b"<svg/>".as_slice()),
        ("/icon.png", "image/png", &[0x89, 0x50, 0x4E, 0x47]),
        ("/favicon.ico", "image/x-icon", &[0, 0, 1, 0]),
        ("/info.json", "application/json", b"{\"v\":1}".as_slice()),
        (
            "/readme.txt",
            "text/plain; charset=utf-8",
            b"hello".as_slice(),
        ),
        ("/raw.bin", "application/octet-stream", &[1, 2, 3]),
    ] {
        let (st, hdrs, bytes) = http_get(srv.addr, path).await;
        assert_eq!(st, 200, "static asset {path} returns 200");
        assert!(
            hdrs.to_lowercase()
                .contains(&format!("content-type: {want_mime}")),
            "static asset {path} content-type {want_mime}, got {hdrs}"
        );
        assert_eq!(bytes, want_body, "static asset {path} body matches");
        assert!(
            !String::from_utf8_lossy(&bytes).contains(TEST_TOKEN),
            "token never exposed in static asset"
        );
    }

    let (st_enc, _, body_enc) = http_get(srv.addr, "/readme%2e%74%78%74").await;
    assert_eq!(st_enc, 200, "valid percent-encoded path served");
    assert_eq!(body_enc, b"hello", "valid percent-encoded body");
    let (st_enc_hi, _, _) = http_get(srv.addr, "/readme%2Etxt").await;
    assert_eq!(st_enc_hi, 200, "uppercase hex percent-encoding served");

    for bad in [
        "/../outside/secret.txt",
        "/%2e%2e/outside/secret.txt",
        "/assets/../../outside/secret.txt",
        "/assets",
        "/escape-link.txt",
        "/%00index.html",
        "/bad%ZZ.txt",
        "/a\\b.txt",
        "/./index.html",
    ] {
        let (st, _, _) = http_get(srv.addr, bad).await;
        assert_eq!(st, 404, "disallowed path {bad} must return 404");
    }

    #[cfg(unix)]
    {
        let (st_in, _, body_in) = http_get(srv.addr, "/inside-link.txt").await;
        assert_eq!(st_in, 200, "symlink staying inside web_dir is allowed");
        assert_eq!(body_in, b"hello", "inside symlink content");
    }

    assert!(
        crate::server::sanitize_static_path("/%00index.html").is_none(),
        "NUL byte rejected by sanitize_static_path"
    );
    assert!(
        crate::server::sanitize_static_path("/a%5cb.txt").is_none(),
        "backslash rejected by sanitize_static_path"
    );
    assert!(
        read_static_asset(&web_dir, "/missing.txt").is_none(),
        "missing file returns None"
    );
    assert!(
        srv.events().iter().any(|e| matches!(
            e,
            DiagnosticEvent::StartupReady {
                auth_mode: "token",
                static_available: true,
                ..
            }
        )),
        "startup diagnostic records static_available=true"
    );
    let _rep = srv.stop("signal").await;
}

#[tokio::test]
async fn missing_web_dir_keeps_healthz_and_ws_working() {
    let dir = TempDir::new().unwrap();
    let cfg = base_config(&dir, dir.path().join("does-not-exist"), AuthMode::NoToken);
    let mut srv = TestServer::start(cfg, FakeEngineBundle::default(), Arc::new(NoopHooks)).await;

    let (st_health, _, body_health) = http_get(srv.addr, "/healthz").await;
    assert_eq!(st_health, 200, "/healthz works with missing build");
    assert_eq!(body_health, b"ok", "/healthz body ok");

    let (st_root, _, _) = http_get(srv.addr, "/").await;
    assert_eq!(st_root, 404, "missing build returns 404 for static");

    let (st_ws_bad, _, _) = http_get(srv.addr, "/ws").await;
    assert_eq!(st_ws_bad, 400, "plain HTTP GET /ws returns 400");

    let mut bad_nonempty = TestWsClient::connect(srv.addr).await;
    bad_nonempty
        .send_json(&json!({ "hello": { "token": "unexpected" } }))
        .await;
    assert_eq!(
        bad_nonempty.recv().await,
        ClientFrame::Close(Some(CLOSE_UNAUTHORIZED)),
        "tokenless mode rejects non-empty token with 4401"
    );

    let mut good = TestWsClient::connect(srv.addr).await;
    let (hello, boot) = good.hello("").await;
    assert_eq!(
        hello,
        json!({ "hello": { "version": SERVER_VERSION, "platform": "web" } }),
        "tokenless hello succeeds with empty token"
    );
    assert_eq!(
        boot["event"], DICTATION_EVENT,
        "bootstrap snapshot follows hello"
    );
    good.send_close(CLOSE_NORMAL).await;
    let _close_ack = good.recv().await;

    assert!(
        srv.events().iter().any(|e| matches!(
            e,
            DiagnosticEvent::StartupReady {
                auth_mode: "no_token",
                static_available: false,
                ..
            }
        )),
        "startup diagnostic records static_available=false and no_token"
    );
    let _rep = srv.stop("signal").await;
}

// ---------------------------------------------------------------------------
// AC 7: Hello handshake, control frames, 4401 rejections before engine/events
// ---------------------------------------------------------------------------

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "end-to-end test verifies hello handshake, control frames, 4401 rejections and diagnostics together"
)]
async fn hello_handshake_control_frames_and_4401_rejections() {
    assert_eq!(
        websocket_accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
        "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=",
        "RFC 6455 test vector"
    );
    assert!(constant_time_eq(b"abc", b"abc"), "equal slices");
    assert!(!constant_time_eq(b"abc", b"abx"), "different bytes");
    assert!(!constant_time_eq(b"abc", b"abcd"), "different lengths");

    let dir = TempDir::new().unwrap();
    let cfg = base_config(
        &dir,
        dir.path().join("web"),
        AuthMode::Token(TEST_TOKEN.into()),
    );
    let mut srv = TestServer::start(cfg, FakeEngineBundle::default(), Arc::new(NoopHooks)).await;

    let mut c_ctrl = TestWsClient::connect(srv.addr).await;
    c_ctrl.send_ping(b"hi").await;
    assert_eq!(
        c_ctrl.recv().await,
        ClientFrame::Pong(b"hi".to_vec()),
        "pre-hello Ping answered with Pong"
    );
    c_ctrl.send_pong(b"ignored").await;
    let (hello, boot) = c_ctrl.hello(TEST_TOKEN).await;
    assert_eq!(hello["hello"]["version"], SERVER_VERSION, "hello version");
    assert_eq!(hello["hello"]["platform"], "web", "hello platform");
    assert_eq!(boot["event"], DICTATION_EVENT, "bootstrap snapshot event");
    assert_eq!(boot["payload"]["phase"], "idle", "initial phase idle");

    c_ctrl.send_ping(b"live").await;
    assert_eq!(
        c_ctrl.recv_pong().await,
        ClientFrame::Pong(b"live".to_vec()),
        "post-hello Ping answered with Pong"
    );
    let ping_125 = vec![b'p'; 125];
    c_ctrl.send_ping(&ping_125).await;
    assert_eq!(
        c_ctrl.recv_pong().await,
        ClientFrame::Pong(ping_125),
        "125-byte Ping is valid per RFC 6455"
    );
    c_ctrl.send_pong(b"ok").await;

    for (label, text) in [
        (
            "wrong token",
            json!({ "hello": { "token": "wrong" } }).to_string(),
        ),
        ("missing token", json!({ "hello": {} }).to_string()),
        (
            "extra hello field",
            json!({ "hello": { "token": TEST_TOKEN, "x": 1 } }).to_string(),
        ),
        ("invalid json", "{broken".to_string()),
        (
            "ordinary pre-hello RPC",
            json!({ "id": 1, "call": "dictation_act", "args": { "action": "new", "text": "" } })
                .to_string(),
        ),
    ] {
        let mut c = TestWsClient::connect(srv.addr).await;
        c.send_text(&text).await;
        assert_eq!(
            c.recv().await,
            ClientFrame::Close(Some(CLOSE_UNAUTHORIZED)),
            "{label} closes with 4401"
        );
    }

    let mut c_bin = TestWsClient::connect(srv.addr).await;
    c_bin.send_binary(&[0, 1, 2, 3]).await;
    assert_eq!(
        c_bin.recv().await,
        ClientFrame::Close(Some(CLOSE_UNAUTHORIZED)),
        "pre-hello binary closes with 4401"
    );

    let mut c_close = TestWsClient::connect(srv.addr).await;
    c_close.send_close(CLOSE_NORMAL).await;
    assert_eq!(
        c_close.recv().await,
        ClientFrame::Close(Some(CLOSE_NORMAL)),
        "pre-hello close echoed"
    );
    let c_eof = TestWsClient::connect(srv.addr).await;
    drop(c_eof);

    let (state_reply, _) = c_ctrl.call(1, "dictation_state", json!({})).await;
    assert_eq!(
        state_reply["ok"]["phase"], "idle",
        "pre-hello RPC caused zero engine work"
    );

    drop(c_ctrl);
    let _rep = srv.stop("signal").await;
    let diag_events = srv.events();
    assert!(
        diag_events.iter().any(|e| matches!(
            e,
            DiagnosticEvent::Auth {
                outcome: "rejected",
                reason: "bad_token",
                ..
            }
        )),
        "rejected auth diagnostic emitted"
    );
    assert!(
        diag_events.iter().any(|e| matches!(
            e,
            DiagnosticEvent::ConnectionClosed {
                reason: "auth_rejected",
                ..
            }
        )),
        "rejected auth connection close diagnostic emitted"
    );
}

// ---------------------------------------------------------------------------
// AC 8, AC 9, AC 10, AC 11: Fixtures, RPCs, fixed values, library, Ollama, errors
// ---------------------------------------------------------------------------

#[test]
fn wire_and_bridge_fixtures_and_enum_values_pinned() {
    let wire: Value = serde_json::from_str(WIRE_FIXTURE).unwrap();
    let bridge: Value = serde_json::from_str(BRIDGE_FIXTURE).unwrap();

    assert!(
        wire["helloResponse"]["hello"]["version"]
            .as_str()
            .is_some_and(|v| !v.is_empty()),
        "wire fixture hello version is a non-empty string"
    );
    assert_eq!(
        wire["helloResponse"]["hello"]["version"], wire["rpcReplyOkVersion"]["ok"],
        "wire fixture hello version matches app_version reply"
    );
    assert_eq!(
        wire["helloResponse"]["hello"]["platform"], "web",
        "wire fixture platform web"
    );
    assert_eq!(
        wire["rpcReplyOkInputDevices"]["ok"],
        json!({ "default": "Browser microphone", "others": [] }),
        "wire fixture input_devices shape"
    );
    assert_eq!(
        wire["rpcReplyErrUnavailable"]["err"], NOT_AVAILABLE_ERR,
        "wire fixture unavailable error"
    );
    assert!(
        wire["rpcReplyErrUncorrelated"]["id"].is_null(),
        "wire fixture uncorrelated error id is null"
    );

    let snap: Snapshot = serde_json::from_value(bridge["snapshot"].clone()).unwrap();
    assert_eq!(
        serde_json::to_value(&snap).unwrap(),
        bridge["snapshot"],
        "nested Snapshot fixture round-trips"
    );
    let entry: LibraryEntry = serde_json::from_value(bridge["libraryEntry"].clone()).unwrap();
    assert_eq!(
        serde_json::to_value(&entry).unwrap(),
        bridge["libraryEntry"],
        "nested LibraryEntry fixture round-trips"
    );
    let settings: Settings = serde_json::from_value(bridge["settings"].clone()).unwrap();
    assert_eq!(
        serde_json::to_value(&settings).unwrap(),
        bridge["settings"],
        "nested Settings fixture round-trips"
    );

    for (action, name) in [
        (Action::New, "new"),
        (Action::Record, "record"),
        (Action::Pause, "pause"),
        (Action::Resume, "resume"),
        (Action::Done, "done"),
        (Action::Discard, "discard"),
        (Action::Retry, "retry"),
        (Action::Copy, "copy"),
        (Action::Accept, "accept"),
        (Action::DownloadModel, "downloadModel"),
        (Action::UndoLearn, "undoLearn"),
    ] {
        assert_eq!(
            serde_json::to_value(action).unwrap(),
            json!(name),
            "Action {name}"
        );
        assert_eq!(
            serde_json::from_value::<Action>(json!(name)).unwrap(),
            action,
            "Action {name} parses"
        );
    }

    for (phase, name) in [
        (Phase::Idle, "idle"),
        (Phase::Ready, "ready"),
        (Phase::Recording, "recording"),
        (Phase::Paused, "paused"),
        (Phase::Transcribing, "transcribing"),
        (Phase::Finished, "finished"),
        (Phase::Failed, "failed"),
    ] {
        assert_eq!(
            serde_json::to_value(phase).unwrap(),
            json!(name),
            "Phase {name}"
        );
        assert_eq!(
            serde_json::from_value::<Phase>(json!(name)).unwrap(),
            phase,
            "Phase {name} parses"
        );
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "end-to-end test verifies fixed RPC values, unavailable calls and real library flows together"
)]
async fn rpc_fixed_values_unavailable_and_real_library() {
    let dir = TempDir::new().unwrap();
    let secret_transcript = "Confidential transcript about Project Orion.";
    let bundle = FakeEngineBundle {
        transcriber: FakeTranscriber::answering(&[Ok(secret_transcript)]),
        ..FakeEngineBundle::default()
    };
    let mut srv = TestServer::start(
        base_config(
            &dir,
            dir.path().join("web"),
            AuthMode::Token(TEST_TOKEN.into()),
        ),
        bundle,
        Arc::new(NoopHooks),
    )
    .await;
    let mut client = TestWsClient::connect(srv.addr).await;
    let (hello, _boot) = client.hello(TEST_TOKEN).await;

    let (ver, _) = client.call(1, "app_version", json!({})).await;
    assert_eq!(
        ver["ok"], hello["hello"]["version"],
        "app_version matches hello"
    );
    let (devs, _) = client.call(2, "input_devices", json!({})).await;
    assert_eq!(
        devs["ok"],
        json!({ "default": "Browser microphone", "others": [] }),
        "input_devices placeholder"
    );
    let (fs, _) = client.call(3, "window_fullscreen", json!({})).await;
    assert_eq!(fs["ok"], json!(true), "window_fullscreen returns true");

    for (idx, name) in [
        (4, "vocabulary_open"),
        (5, "learned_vocabulary_open"),
        (6, "pick_vocabulary_file"),
        (7, "pick_model_folder"),
    ] {
        let (rep, _) = client.call(idx, name, json!({})).await;
        assert_eq!(
            rep,
            json!({ "id": idx, "err": NOT_AVAILABLE_ERR }),
            "{name} returns unavailable message"
        );
    }

    let (_n, _) = client
        .call(10, "dictation_act", json!({ "action": "new", "text": "" }))
        .await;
    let (_r, _) = client
        .call(
            11,
            "dictation_act",
            json!({ "action": "record", "text": "" }),
        )
        .await;
    client
        .send_binary(&pcm16_le_bytes(&loud_pcm16(8_000)))
        .await;
    let finished_snap = finish_done(&mut client, 12).await;
    assert_eq!(
        finished_snap["text"], secret_transcript,
        "transcript delivered"
    );
    assert_eq!(
        finished_snap["notice"], "Copied to the clipboard.",
        "raw Snapshot notice preserved on event"
    );

    let (r_copy_act, _) = client
        .call(
            13,
            "dictation_act",
            json!({ "action": "copy", "text": secret_transcript }),
        )
        .await;
    assert_eq!(
        r_copy_act["ok"]["notice"], "Copied to the clipboard.",
        "raw Snapshot notice preserved on RPC"
    );

    let (list_rep, _) = client.call(14, "library_list", json!({})).await;
    let entries = list_rep["ok"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "one real engine library entry");
    let entry_id = entries[0]["id"].as_str().unwrap().to_string();

    let (get_rep, _) = client
        .call(15, "library_get", json!({ "id": entry_id }))
        .await;
    assert_eq!(get_rep["ok"], secret_transcript, "library_get transcript");
    let (copy_rep, _) = client
        .call(16, "library_copy", json!({ "id": entry_id }))
        .await;
    assert_eq!(
        copy_rep,
        json!({ "id": 16, "ok": null }),
        "library_copy returns null ok without physical clipboard"
    );
    let (miss_get, _) = client
        .call(17, "library_get", json!({ "id": "99990101-000000" }))
        .await;
    assert_eq!(
        miss_get["err"], "That recording is no longer in the library.",
        "missing entry error on library_get"
    );
    let (miss_copy, _) = client
        .call(18, "library_copy", json!({ "id": "99990101-000000" }))
        .await;
    assert_eq!(
        miss_copy["err"], "That recording is no longer in the library.",
        "missing entry error on library_copy"
    );

    drop(client);
    let _rep = srv.stop("signal").await;
    let serialized = serde_json::to_string(&srv.events()).unwrap();
    assert!(
        !serialized.contains(TEST_TOKEN),
        "token excluded from diagnostics"
    );
    assert!(
        !serialized.contains(secret_transcript),
        "transcript excluded from diagnostics"
    );
}

#[tokio::test]
async fn ollama_models_loopback_success_and_upstream_rejection() {
    let dir = TempDir::new().unwrap();
    let tags_body =
        r#"{"models":[{"name":"zeta:latest"},{"name":"nomic-embed-text"},{"name":"alpha:7b"}]}"#;
    let ok_resp: &'static str = Box::leak(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{tags_body}",
            tags_body.len()
        )
        .into_boxed_str(),
    );
    let mut srv_ok = TestServer::start(
        base_config(&dir, dir.path().join("web"), AuthMode::NoToken),
        FakeEngineBundle {
            ollama_url: Some(spawn_http_stub(ok_resp, None)),
            ..FakeEngineBundle::default()
        },
        Arc::new(NoopHooks),
    )
    .await;
    let mut c_ok = TestWsClient::connect(srv_ok.addr).await;
    let (_h_ok, _b_ok) = c_ok.hello("").await;
    let (models, _) = c_ok.call(1, "ollama_models", json!({})).await;
    assert_eq!(
        models["ok"],
        json!(["alpha:7b", "zeta:latest"]),
        "ollama_models sorted without embed model"
    );
    drop(c_ok);
    let _rep_ok = srv_ok.stop("signal").await;

    let bad_resp =
        "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 4\r\nConnection: close\r\n\r\nboom";
    let mut srv_bad = TestServer::start(
        base_config(&dir, dir.path().join("web"), AuthMode::NoToken),
        FakeEngineBundle {
            ollama_url: Some(spawn_http_stub(bad_resp, None)),
            ..FakeEngineBundle::default()
        },
        Arc::new(NoopHooks),
    )
    .await;
    let mut c_bad = TestWsClient::connect(srv_bad.addr).await;
    let (_h_bad, _b_bad) = c_bad.hello("").await;
    let (err_rep, _) = c_bad.call(1, "ollama_models", json!({})).await;
    assert!(
        err_rep["err"]
            .as_str()
            .unwrap()
            .starts_with("Local AI needs Ollama: "),
        "upstream rejection surfaces engine error: {err_rep}"
    );
    drop(c_bad);
    let _rep_bad = srv_bad.stop("signal").await;

    let mut srv_closed = TestServer::start(
        base_config(&dir, dir.path().join("web"), AuthMode::NoToken),
        FakeEngineBundle {
            ollama_url: Some(closed_loopback_url()),
            ..FakeEngineBundle::default()
        },
        Arc::new(NoopHooks),
    )
    .await;
    let mut c_closed = TestWsClient::connect(srv_closed.addr).await;
    let (_h_cl, _b_cl) = c_closed.hello("").await;
    let (closed_rep, _) = c_closed.call(1, "ollama_models", json!({})).await;
    assert!(
        closed_rep["err"]
            .as_str()
            .unwrap()
            .starts_with("Local AI needs Ollama: "),
        "unreachable Ollama returns error"
    );
    drop(c_closed);
    let _rep_closed = srv_closed.stop("signal").await;
}

#[tokio::test]
async fn recoverable_authenticated_errors_and_continuation() {
    let dir = TempDir::new().unwrap();
    let mut srv = TestServer::start(
        base_config(
            &dir,
            dir.path().join("web"),
            AuthMode::Token(TEST_TOKEN.into()),
        ),
        FakeEngineBundle::default(),
        Arc::new(NoopHooks),
    )
    .await;
    let mut client = TestWsClient::connect(srv.addr).await;
    let (_hello, _boot) = client.hello(TEST_TOKEN).await;

    let (unk, _) = client.call(20, "no_such_call", json!({})).await;
    assert_eq!(unk["id"], 20, "unknown call preserves id");
    assert!(unk["err"].is_string(), "unknown call returns err");

    client.send_text("{not valid json").await;
    let mal = client.recv_json().await;
    assert!(
        mal["id"].is_null() && mal["err"].is_string(),
        "malformed JSON"
    );

    for bad_uncorr in [
        "[1,2,3]".to_string(),
        json!({ "call": "app_version", "args": {} }).to_string(),
        json!({ "id": -1, "call": "app_version", "args": {} }).to_string(),
        json!({ "id": "21", "call": "app_version", "args": {} }).to_string(),
        json!({ "id": 5_000_000_000_u64, "call": "app_version", "args": {} }).to_string(),
    ] {
        client.send_text(&bad_uncorr).await;
        let rep = client.recv_json().await;
        assert!(
            rep["id"].is_null() && rep["err"].is_string(),
            "uncorrelated bad envelope returns id: null for {bad_uncorr}"
        );
    }

    for (eid, raw) in [
        (30, json!({ "id": 30, "call": "app_version", "args": null })),
        (
            31,
            json!({ "id": 31, "call": "app_version", "args": {}, "extra": 1 }),
        ),
        (32, json!({ "id": 32, "call": 123, "args": {} })),
    ] {
        client.send_json(&raw).await;
        let rep = client.recv_json().await;
        assert_eq!(rep["id"], eid, "recoverable id {eid} preserved");
        assert!(rep["err"].is_string(), "bad envelope returns err");
    }

    for (idx, call, bad_args) in [
        (40, "dictation_state", json!({ "extra": 1 })),
        (
            41,
            "dictation_act",
            json!({ "action": "bogus", "text": "" }),
        ),
        (42, "library_list", json!({ "extra": 1 })),
        (43, "library_get", json!({})),
        (44, "library_copy", json!({})),
        (45, "settings_get", json!({ "extra": 1 })),
        (46, "settings_set", json!({ "settings": {} })),
        (47, "ollama_models", json!({ "extra": 1 })),
        (48, "app_version", json!({ "extra": 1 })),
        (49, "input_devices", json!({ "extra": 1 })),
        (50, "window_fullscreen", json!({ "extra": 1 })),
        (51, "vocabulary_open", json!({ "extra": 1 })),
    ] {
        let (rep, _) = client.call(idx, call, bad_args).await;
        assert_eq!(rep["id"], idx, "invalid args for {call} preserves id");
        assert!(
            rep["err"].is_string(),
            "invalid args for {call} returns err"
        );
    }

    client.send_binary(&[1, 2, 3]).await;
    let odd = client.recv_json().await;
    assert!(
        odd["id"].is_null() && odd["err"].is_string(),
        "odd-byte PCM"
    );

    let (after, _) = client.call(99, "app_version", json!({})).await;
    assert_eq!(
        after["ok"], SERVER_VERSION,
        "subsequent RPC succeeds after recoverable errors"
    );

    drop(client);
    let _rep = srv.stop("signal").await;
    let serialized = serde_json::to_string(&srv.events()).unwrap();
    assert!(!serialized.contains("{not valid json"), "raw JSON excluded");
    assert!(
        !serialized.contains("no_such_call"),
        "raw unknown call excluded"
    );
}

// ---------------------------------------------------------------------------
// AC 12: Authenticated-only events, level to two clients, controlled bootstrap
// ---------------------------------------------------------------------------

struct BootstrapHook {
    target_conn: u64,
    fired: AtomicBool,
    hub: Mutex<Option<EventHub>>,
    bootstrap_rev: u64,
}

impl ServerHooks for BootstrapHook {
    fn before_sample(&self, conn_id: u64) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        if conn_id == self.target_conn
            && !self.fired.swap(true, Ordering::SeqCst)
            && let Some(hub) = self.hub.lock().unwrap().clone()
        {
            let make_snap = |rev: u64| Snapshot {
                revision: rev,
                phase: Phase::Ready,
                recorded_ms: 0,
                segments: 0,
                pending: 0,
                text: String::new(),
                error: None,
                notice: None,
                model_error: None,
                download: None,
                library_revision: 0,
                microphone: None,
                learned: Vec::new(),
                partial: String::new(),
                raw_text: None,
                post_processing: false,
            };
            hub.publish(Event::Level(0.42));
            hub.publish(Event::Level(0.43));
            hub.publish(Event::State(make_snap(
                self.bootstrap_rev.saturating_sub(1),
            )));
            hub.publish(Event::State(make_snap(self.bootstrap_rev)));
            hub.publish(Event::State(make_snap(
                self.bootstrap_rev.saturating_add(1),
            )));
        }
        Box::pin(std::future::ready(()))
    }

    fn before_event_write(&self, _conn_id: u64) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::ready(()))
    }
}

#[tokio::test]
async fn events_authenticated_only_and_level_fanout_to_two_clients() {
    let dir = TempDir::new().unwrap();
    let cfg = base_config(
        &dir,
        dir.path().join("web"),
        AuthMode::Token(TEST_TOKEN.into()),
    );
    let mut srv = TestServer::start(cfg, FakeEngineBundle::default(), Arc::new(NoopHooks)).await;

    let mut unauth = TestWsClient::connect(srv.addr).await;
    let mut c1 = TestWsClient::connect(srv.addr).await;
    let (_h1, _b1) = c1.hello(TEST_TOKEN).await;
    let mut c2 = TestWsClient::connect(srv.addr).await;
    let (_h2, _b2) = c2.hello(TEST_TOKEN).await;

    let (_r_new, _) = c1
        .call(1, "dictation_act", json!({ "action": "new", "text": "" }))
        .await;
    let (_r_rec, _) = c1
        .call(
            2,
            "dictation_act",
            json!({ "action": "record", "text": "" }),
        )
        .await;
    c1.send_binary(&pcm16_le_bytes(&loud_pcm16(1_600))).await;

    for (label, client) in [("c1", &mut c1), ("c2", &mut c2)] {
        let mut saw_level = false;
        for _ in 0..20 {
            let msg = client.recv_json().await;
            if msg.get("event").and_then(Value::as_str) == Some(LEVEL_EVENT)
                && msg["payload"].as_f64().unwrap_or(0.0) > 0.0
            {
                saw_level = true;
                break;
            }
        }
        assert!(saw_level, "{label} received dictation-level from owner PCM");
    }

    let (h_unauth, b_unauth) = unauth.hello(TEST_TOKEN).await;
    assert_eq!(
        h_unauth["hello"]["platform"], "web",
        "unauthenticated socket received no events before hello"
    );
    assert_eq!(
        b_unauth["event"], DICTATION_EVENT,
        "bootstrap snapshot followed hello"
    );

    drop((unauth, c1, c2));
    let _rep = srv.stop("signal").await;
}

#[tokio::test]
async fn controlled_concurrent_bootstrap_publish_filtering() {
    let dir = TempDir::new().unwrap();
    let (diag, _events) = Diagnostics::recording();
    let hub = EventHub::new(diag.clone());
    let hook: Arc<dyn ServerHooks> = Arc::new(BootstrapHook {
        target_conn: 1,
        fired: AtomicBool::new(false),
        hub: Mutex::new(Some(hub.clone())),
        bootstrap_rev: 2,
    });
    let bundle = FakeEngineBundle::default();
    let recorder = StreamRecorder::new();
    let engine = bundle.open(
        dir.path().join("data"),
        recorder.clone(),
        Arc::new(|_ev| {}),
    );
    let s1 = engine.act(Action::New, "").unwrap();
    let s2 = engine.act(Action::Discard, "").unwrap();
    assert_eq!(s2.revision, s1.revision.saturating_add(1), "revision grew");
    assert_eq!(s2.revision, 2, "bootstrap revision is 2");

    let state = ServerState::new(
        engine.clone(),
        recorder,
        AuthMode::NoToken,
        dir.path().join("web"),
        hub.clone(),
        diag,
        hook,
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let server_task = tokio::spawn(serve_axum(listener, router(state.clone()), stop_rx));

    let mut boot_client = TestWsClient::connect(addr).await;
    boot_client
        .send_json(&json!({ "hello": { "token": "" } }))
        .await;

    let f1 = boot_client.recv_json().await;
    let f2 = boot_client.recv_json().await;
    let f3 = boot_client.recv_json().await;
    let f4 = boot_client.recv_json().await;
    assert_eq!(f1["hello"]["platform"], "web", "frame 1 is hello");
    assert_eq!(
        f2["event"], DICTATION_EVENT,
        "frame 2 is bootstrap snapshot"
    );
    assert_eq!(f2["payload"]["revision"], 2, "bootstrap rev 2");
    assert_eq!(
        f3["event"], DICTATION_EVENT,
        "frame 3 is equal-rev snapshot"
    );
    assert_eq!(f3["payload"]["revision"], 2, "preserved equal rev 2");
    assert_eq!(f4["event"], DICTATION_EVENT, "frame 4 is newer snapshot");
    assert_eq!(f4["payload"]["revision"], 3, "preserved newer rev 3");

    hub.publish(Event::Level(0.75));
    let f5 = boot_client.recv_json().await;
    assert_eq!(
        f5["event"], LEVEL_EVENT,
        "live level after boundary delivered"
    );
    assert_eq!(f5["payload"], 0.75, "live level payload matches");

    assert!(
        format_filtered_event(
            &crate::server::HubEvent {
                seq: 5,
                event: Event::Level(0.1),
            },
            5,
            2,
        )
        .is_none(),
        "level at boundary seq discarded"
    );

    drop(boot_client);
    let _sent = stop_tx.send(true);
    state.drain_and_close_connections().await;
    let _joined = server_task.await;
    let _rep = engine.shutdown();
}

// ---------------------------------------------------------------------------
// AC 13: 64-event subscriber overflow (stalled vs healthy) + blocking RPC isolation
// ---------------------------------------------------------------------------

struct StallFirstWriterHook {
    stalled_conn: u64,
    entered: Notify,
    release: Notify,
    paused_once: AtomicBool,
}

impl ServerHooks for StallFirstWriterHook {
    fn before_sample(&self, _conn_id: u64) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::ready(()))
    }

    fn before_event_write(&self, conn_id: u64) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        if conn_id == self.stalled_conn && !self.paused_once.swap(true, Ordering::SeqCst) {
            Box::pin(async move {
                self.entered.notify_one();
                self.release.notified().await;
            })
        } else {
            Box::pin(std::future::ready(()))
        }
    }
}

#[tokio::test]
async fn stalled_subscriber_disconnects_on_64_queue_overflow_while_healthy_peer_receives_all() {
    let dir = TempDir::new().unwrap();
    let (diag, events) = Diagnostics::recording();
    let hub = EventHub::new(diag.clone());
    let hook = Arc::new(StallFirstWriterHook {
        stalled_conn: 1,
        entered: Notify::new(),
        release: Notify::new(),
        paused_once: AtomicBool::new(false),
    });
    let hooks_dyn: Arc<dyn ServerHooks> = hook.clone();
    let recorder = StreamRecorder::new();
    let pub_hub = hub.clone();
    let engine = FakeEngineBundle::default().open(
        dir.path().join("data"),
        recorder.clone(),
        Arc::new(move |ev| pub_hub.publish(ev)),
    );
    let state = ServerState::new(
        engine.clone(),
        recorder,
        AuthMode::NoToken,
        dir.path().join("web"),
        hub.clone(),
        diag,
        hooks_dyn,
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let server_task = tokio::spawn(serve_axum(listener, router(state.clone()), stop_rx));

    let mut stalled = TestWsClient::connect(addr).await;
    let (_h1, _b1) = stalled.hello("").await;
    let mut healthy = TestWsClient::connect(addr).await;
    let (_h2, _b2) = healthy.hello("").await;

    hub.publish(Event::Level(0.01));
    tokio::time::timeout(Duration::from_secs(3), hook.entered.notified())
        .await
        .expect("stalled writer entered before_event_write");
    let first = healthy.recv_json().await;
    assert_eq!(first["event"], LEVEL_EVENT, "healthy peer receives event 0");

    for i in 1..=65_u32 {
        hub.publish(Event::Level(f64::from(i) / 100.0));
        let msg = healthy.recv_json().await;
        assert_eq!(
            msg["event"], LEVEL_EVENT,
            "healthy peer receives every level"
        );
    }

    let stalled_end = stalled.recv().await;
    assert!(
        matches!(stalled_end, ClientFrame::Close(_) | ClientFrame::Eof),
        "stalled subscriber disconnected on overflow, got {stalled_end:?}"
    );

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let diag_snapshot = events.lock().unwrap().clone();
        if diag_snapshot.contains(&DiagnosticEvent::SubscriberLag {
            conn_id: 1,
            reason: "subscriber_lag",
            queue_capacity: SUBSCRIBER_QUEUE_CAPACITY,
        }) && diag_snapshot.iter().any(|e| {
            matches!(
                e,
                DiagnosticEvent::ConnectionClosed {
                    conn_id: 1,
                    reason: "subscriber_lag",
                    ..
                }
            )
        }) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "lag diagnostics recorded: {diag_snapshot:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    hook.release.notify_waiters();
    drop((stalled, healthy));
    let _sent = stop_tx.send(true);
    state.drain_and_close_connections().await;
    let _joined = server_task.await;
    let _rep = engine.shutdown();
}

#[tokio::test]
async fn pending_blocking_rpc_does_not_block_independent_peer() {
    let dir = TempDir::new().unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let body = r#"{"models":[{"name":"qwen3.8:27b-mlx"}]}"#;
    let resp: &'static str = Box::leak(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_boxed_str(),
    );
    let ollama_url = spawn_http_stub(resp, Some((entered_tx, release_rx)));

    let mut srv = TestServer::start(
        base_config(&dir, dir.path().join("web"), AuthMode::NoToken),
        FakeEngineBundle {
            ollama_url: Some(ollama_url),
            ..FakeEngineBundle::default()
        },
        Arc::new(NoopHooks),
    )
    .await;

    let mut blocking_client = TestWsClient::connect(srv.addr).await;
    let (_h1, _b1) = blocking_client.hello("").await;
    let mut peer = TestWsClient::connect(srv.addr).await;
    let (_h2, _b2) = peer.hello("").await;

    blocking_client
        .send_json(&json!({ "id": 1, "call": "ollama_models", "args": {} }))
        .await;
    tokio::task::spawn_blocking(move || {
        entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("ollama stub reached");
    })
    .await
    .unwrap();

    let (ver, _) = peer.call(10, "app_version", json!({})).await;
    assert_eq!(ver["ok"], SERVER_VERSION, "peer app_version unblocked");
    let (act, _) = peer
        .call(11, "dictation_act", json!({ "action": "new", "text": "" }))
        .await;
    assert_eq!(act["ok"]["phase"], "ready", "peer dictation_act unblocked");

    release_tx.send(()).unwrap();
    let models = loop {
        let msg = blocking_client.recv_json().await;
        if msg.get("id").and_then(Value::as_u64) == Some(1) {
            break msg;
        }
    };
    assert_eq!(
        models["ok"],
        json!(["qwen3.8:27b-mlx"]),
        "blocking RPC completed after release"
    );

    drop((blocking_client, peer));
    let _rep = srv.stop("signal").await;
}

// ---------------------------------------------------------------------------
// AC 14 & AC 15: Ownership coordination, disconnect recovery, exact PCM & split
// ---------------------------------------------------------------------------

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "end-to-end test verifies ownership refusals, late cleanup and disconnect recovery sequentially"
)]
async fn ownership_refusals_late_old_owner_cleanup_and_disconnect_recovery() {
    let dir = TempDir::new().unwrap();
    let bundle = FakeEngineBundle {
        transcriber: FakeTranscriber::answering(&[Ok("captured before drop")]),
        ..FakeEngineBundle::default()
    };
    let mut srv = TestServer::start(
        base_config(&dir, dir.path().join("web"), AuthMode::NoToken),
        bundle.clone(),
        Arc::new(NoopHooks),
    )
    .await;

    let mut c1 = TestWsClient::connect(srv.addr).await;
    let (_h1, _b1) = c1.hello("").await;
    let mut c2 = TestWsClient::connect(srv.addr).await;
    let (_h2, _b2) = c2.hello("").await;

    for (id, action) in [(1, "record"), (2, "resume"), (3, "discard")] {
        let (r, _) = c1
            .call(id, "dictation_act", json!({ "action": action, "text": "" }))
            .await;
        assert!(r["err"].is_string(), "idle {action} refused");
    }

    let (_r_new, _) = c1
        .call(4, "dictation_act", json!({ "action": "new", "text": "" }))
        .await;
    let (r_rec, _) = c1
        .call(
            5,
            "dictation_act",
            json!({ "action": "record", "text": "" }),
        )
        .await;
    assert_eq!(r_rec["ok"]["phase"], "recording", "c1 recording");

    let (refused_record, _) = c2
        .call(
            6,
            "dictation_act",
            json!({ "action": "record", "text": "" }),
        )
        .await;
    assert!(refused_record["err"].is_string(), "c2 record refused");
    let (refused_resume, _) = c2
        .call(
            7,
            "dictation_act",
            json!({ "action": "resume", "text": "" }),
        )
        .await;
    assert!(refused_resume["err"].is_string(), "c2 resume refused");

    let (p_rep, _) = c2
        .call(8, "dictation_act", json!({ "action": "pause", "text": "" }))
        .await;
    assert_eq!(p_rep["ok"]["phase"], "paused", "paused by c2");
    let (res_rep, _) = c2
        .call(
            9,
            "dictation_act",
            json!({ "action": "resume", "text": "" }),
        )
        .await;
    assert_eq!(res_rep["ok"]["phase"], "recording", "resumed by c2");

    // Late old-owner cleanup: c1 disconnects AFTER c2 became owner -> c2 keeps ownership.
    c1.send_close(CLOSE_NORMAL).await;
    let _c1_ack = c1.recv().await;
    drop(c1);

    let c2_samples = loud_pcm16(8_000);
    c2.send_binary(&pcm16_le_bytes(&c2_samples)).await;
    let (_barrier, _) = c2.call(10, "dictation_state", json!({})).await;

    // Owner c2 disconnects while recording -> stays recording; c3 gets no implicit grant.
    c2.send_close(CLOSE_NORMAL).await;
    let _c2_ack = c2.recv().await;
    drop(c2);

    let mut c3 = TestWsClient::connect(srv.addr).await;
    let (_h3, boot3) = c3.hello("").await;
    assert_eq!(
        boot3["payload"]["phase"], "recording",
        "owner disconnect leaves session in recording"
    );
    c3.send_binary(&pcm16_le_bytes(&vec![25_000_i16; 1_600]))
        .await;
    let (_b3, _) = c3.call(11, "dictation_state", json!({})).await;

    let fin = finish_done(&mut c3, 12).await;
    assert_eq!(
        fin["text"], "captured before drop",
        "recovery by Done transcribes pre-disconnect audio"
    );
    let heard = bundle.transcriber.heard_samples();
    assert_eq!(heard.len(), 1, "one non-silent segment transcribed");
    assert_eq!(
        heard[0].len(),
        c2_samples.len(),
        "c3 post-disconnect PCM was excluded"
    );

    // Recovery by Discard after owner disconnect.
    let (_n2, _) = c3
        .call(13, "dictation_act", json!({ "action": "new", "text": "" }))
        .await;
    let (_r2, _) = c3
        .call(
            14,
            "dictation_act",
            json!({ "action": "record", "text": "" }),
        )
        .await;
    c3.send_close(CLOSE_NORMAL).await;
    let _c3_ack = c3.recv().await;
    drop(c3);

    let mut c4 = TestWsClient::connect(srv.addr).await;
    let (_h4, boot4) = c4.hello("").await;
    assert_eq!(
        boot4["payload"]["phase"], "recording",
        "stays recording after second owner disconnect"
    );
    let (disc, _) = c4
        .call(
            15,
            "dictation_act",
            json!({ "action": "discard", "text": "" }),
        )
        .await;
    assert_eq!(disc["ok"]["phase"], "ready", "Discard recovers to ready");

    drop(c4);
    let _rep = srv.stop("signal").await;
    let ownership_events = srv.events();
    assert!(
        ownership_events.iter().any(|e| matches!(
            e,
            DiagnosticEvent::Ownership {
                outcome: "cleared",
                action: "pause",
                phase: "paused",
                ..
            }
        )),
        "pause clears ownership with diagnostic"
    );
    assert!(
        !ownership_events.iter().any(|e| matches!(
            e,
            DiagnosticEvent::Ownership {
                outcome: "cleared",
                action: "new",
                ..
            }
        )),
        "new without an active owner does not emit cleared"
    );
    assert!(
        ownership_events.iter().any(|e| matches!(
            e,
            DiagnosticEvent::Ownership {
                conn_id: 2,
                outcome: "released",
                action: "disconnect",
                phase: "recording",
            }
        )),
        "owner c2 disconnect emits released diagnostic"
    );
    assert!(
        !ownership_events.iter().any(|e| matches!(
            e,
            DiagnosticEvent::Ownership {
                conn_id: 1,
                outcome: "released",
                ..
            }
        )),
        "non-owner c1 disconnect does not emit released"
    );
}

#[tokio::test]
async fn exact_ordered_pcm_through_done_with_split_and_rejection_barriers() {
    let dir = TempDir::new().unwrap();
    let bundle = FakeEngineBundle {
        transcriber: FakeTranscriber::answering(&[Ok("part one"), Ok("part two")]),
        ..FakeEngineBundle::default()
    };
    let mut srv = TestServer::start(
        base_config(&dir, dir.path().join("web"), AuthMode::NoToken),
        bundle.clone(),
        Arc::new(NoopHooks),
    )
    .await;

    let mut owner = TestWsClient::connect(srv.addr).await;
    let (_h1, _b1) = owner.hello("").await;
    let mut nonowner = TestWsClient::connect(srv.addr).await;
    let (_h2, _b2) = nonowner.hello("").await;

    let (s_get, _) = owner.call(1, "settings_get", json!({})).await;
    let mut settings: Settings = serde_json::from_value(s_get["ok"].clone()).unwrap();
    settings.silence_split_ms = 1_000;
    let (_s_set, _) = owner
        .call(2, "settings_set", json!({ "settings": settings }))
        .await;

    let (_r_new, _) = owner
        .call(3, "dictation_act", json!({ "action": "new", "text": "" }))
        .await;
    let (_r_rec, _) = owner
        .call(
            4,
            "dictation_act",
            json!({ "action": "record", "text": "" }),
        )
        .await;

    // Nonowner poison PCM barriered by same-socket RPC.
    nonowner
        .send_binary(&pcm16_le_bytes(&vec![30_000_i16; 1_600]))
        .await;
    let (_nonowner_barrier, _) = nonowner.call(100, "dictation_state", json!({})).await;

    // Pause -> out-of-phase (paused) poison PCM barriered by same-socket RPC -> Resume.
    let (p_rep, _) = owner
        .call(5, "dictation_act", json!({ "action": "pause", "text": "" }))
        .await;
    assert_eq!(p_rep["ok"]["phase"], "paused", "paused");
    owner
        .send_binary(&pcm16_le_bytes(&vec![-28_000_i16; 1_600]))
        .await;
    let (_paused_barrier, _) = owner.call(6, "dictation_state", json!({})).await;

    let (res_rep, _) = owner
        .call(
            7,
            "dictation_act",
            json!({ "action": "resume", "text": "" }),
        )
        .await;
    assert_eq!(res_rep["ok"]["phase"], "recording", "resumed");

    // 2 s speech + 1 s silence (triggers real StreamRecorder split) + final pre-Done block + Done.
    let split_part = loud_pcm16(32_000);
    let split_silence = vec![0_i16; 16_000];
    let final_pre_done: Vec<i16> = (0..4_000_i16)
        .map(|i| if i % 2 == 0 { 12_345 } else { -12_345 })
        .collect();
    for chunk in split_part.chunks(4_000) {
        owner.send_binary(&pcm16_le_bytes(chunk)).await;
    }
    for chunk in split_silence.chunks(4_000) {
        owner.send_binary(&pcm16_le_bytes(chunk)).await;
    }
    owner.send_binary(&pcm16_le_bytes(&final_pre_done)).await;
    let _fin = finish_done(&mut owner, 8).await;

    let heard = bundle.transcriber.heard_samples();
    let expected_final: Vec<f32> = final_pre_done
        .iter()
        .map(|&s| f32::from(s) / 32_768.0)
        .collect();
    let last_segment = heard.last().expect("at least one transcribed segment");
    assert!(
        last_segment.ends_with(&expected_final),
        "exact ordered final pre-Done PCM16 samples reached transcriber (last len {})",
        last_segment.len()
    );
    let nonowner_val = f32::from(30_000_i16) / 32_768.0;
    let paused_val = f32::from(-28_000_i16) / 32_768.0;
    for seg in &heard {
        assert!(
            !seg.iter().any(|&v| (v - nonowner_val).abs() < 1e-6),
            "nonowner PCM excluded from capture"
        );
        assert!(
            !seg.iter().any(|&v| (v - paused_val).abs() < 1e-6),
            "paused PCM excluded from capture"
        );
    }

    owner.send_close(CLOSE_NORMAL).await;
    let _owner_ack = owner.recv().await;
    nonowner.send_close(CLOSE_NORMAL).await;
    let _nonowner_ack = nonowner.recv().await;
    drop((owner, nonowner));

    let _rep = srv.stop("signal").await;
    assert_pcm_rejection_diagnostics(&srv.events());
}

fn assert_pcm_rejection_diagnostics(diag_events: &[DiagnosticEvent]) {
    assert!(
        diag_events.iter().any(|e| matches!(
            e,
            DiagnosticEvent::ConnectionClosed {
                audio: AudioCounts {
                    dropped_out_of_phase_frames: 1,
                    accepted_frames,
                    ..
                },
                ..
            } if *accepted_frames > 0
        )),
        "owner diagnostics recorded accepted and out-of-phase frames: {diag_events:?}"
    );
    assert!(
        diag_events.iter().any(|e| matches!(
            e,
            DiagnosticEvent::ConnectionClosed {
                audio: AudioCounts {
                    dropped_nonowner_frames: 1,
                    accepted_frames: 0,
                    ..
                },
                ..
            }
        )),
        "nonowner diagnostics recorded nonowner frame drop: {diag_events:?}"
    );
}

// ---------------------------------------------------------------------------
// AC 16: Exact 1 MiB limit, 1009 overflow (single, fragmented, text, pre-hello)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn exact_1mib_pcm_and_valid_fragmented_rpc_succeed() {
    let dir = TempDir::new().unwrap();
    let bundle = FakeEngineBundle::default();
    let mut srv = TestServer::start(
        base_config(
            &dir,
            dir.path().join("web"),
            AuthMode::Token(TEST_TOKEN.into()),
        ),
        bundle.clone(),
        Arc::new(NoopHooks),
    )
    .await;

    let mut c_exact = TestWsClient::connect(srv.addr).await;
    let (_h, _b) = c_exact.hello(TEST_TOKEN).await;
    let (_n, _) = c_exact
        .call(1, "dictation_act", json!({ "action": "new", "text": "" }))
        .await;
    let (_r, _) = c_exact
        .call(
            2,
            "dictation_act",
            json!({ "action": "record", "text": "" }),
        )
        .await;
    let exact_samples = loud_pcm16(MAX_WS_PAYLOAD / 2);
    let exact_bytes = pcm16_le_bytes(&exact_samples);
    assert_eq!(exact_bytes.len(), MAX_WS_PAYLOAD, "exact 1 MiB payload");
    c_exact.send_binary(&exact_bytes).await;
    let _fin = finish_done(&mut c_exact, 3).await;
    assert_eq!(
        bundle.transcriber.heard_samples()[0].len(),
        exact_samples.len(),
        "exact 1 MiB PCM reached StreamRecorder and transcriber"
    );

    let rpc = json!({ "id": 4, "call": "app_version", "args": {} }).to_string();
    let (left, right) = rpc.split_at(rpc.len() / 2);
    c_exact
        .send_raw_frame(false, 0, 1, true, left.as_bytes())
        .await;
    c_exact.send_ping(b"mid").await;
    loop {
        match c_exact.recv().await {
            ClientFrame::Pong(p) => {
                assert_eq!(p, b"mid", "Ping interleaved during fragmentation answered");
                break;
            }
            ClientFrame::Text(_) => {}
            ClientFrame::Close(code) => panic!("unexpected close {code:?}"),
            ClientFrame::Eof => panic!("unexpected EOF"),
        }
    }
    c_exact
        .send_raw_frame(true, 0, 0, true, right.as_bytes())
        .await;
    let frag_reply = loop {
        let msg = c_exact.recv_json().await;
        if msg.get("id").and_then(Value::as_u64) == Some(4) {
            break msg;
        }
    };
    assert_eq!(
        frag_reply["ok"], SERVER_VERSION,
        "reassembled fragmented RPC succeeded"
    );

    drop(c_exact);
    let _rep = srv.stop("signal").await;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "end-to-end test verifies single/fragmented binary and text 1009 caps plus transport errors"
)]
async fn oversized_frames_close_1009_and_protocol_errors_are_distinct() {
    let dir = TempDir::new().unwrap();
    let mut srv = TestServer::start(
        base_config(
            &dir,
            dir.path().join("web"),
            AuthMode::Token(TEST_TOKEN.into()),
        ),
        FakeEngineBundle::default(),
        Arc::new(NoopHooks),
    )
    .await;

    let mut c_single = TestWsClient::connect(srv.addr).await;
    let (_h1, _b1) = c_single.hello(TEST_TOKEN).await;
    c_single
        .send_binary(&vec![0u8; MAX_WS_PAYLOAD.saturating_add(1)])
        .await;
    assert_eq!(
        c_single.recv_close().await,
        ClientFrame::Close(Some(CLOSE_MESSAGE_TOO_BIG)),
        "single 1-byte-over binary closes with 1009"
    );

    let mut c_frag = TestWsClient::connect(srv.addr).await;
    let (_h2, _b2) = c_frag.hello(TEST_TOKEN).await;
    c_frag
        .send_raw_frame(false, 0, 2, true, &vec![0u8; 600_000])
        .await;
    c_frag
        .send_raw_frame(true, 0, 0, true, &vec![0u8; 448_577])
        .await;
    assert_eq!(
        c_frag.recv_close().await,
        ClientFrame::Close(Some(CLOSE_MESSAGE_TOO_BIG)),
        "authenticated fragmented binary overflow closes with 1009"
    );

    let mut c_text = TestWsClient::connect(srv.addr).await;
    let (_h3, _b3) = c_text.hello(TEST_TOKEN).await;
    c_text
        .send_text(&"a".repeat(MAX_WS_PAYLOAD.saturating_add(1)))
        .await;
    assert_eq!(
        c_text.recv_close().await,
        ClientFrame::Close(Some(CLOSE_MESSAGE_TOO_BIG)),
        "authenticated oversized text closes with 1009"
    );

    let mut c_pre_frag = TestWsClient::connect(srv.addr).await;
    c_pre_frag
        .send_raw_frame(false, 0, 2, true, &vec![0u8; 600_000])
        .await;
    c_pre_frag
        .send_raw_frame(true, 0, 0, true, &vec![0u8; 448_577])
        .await;
    assert_eq!(
        c_pre_frag.recv().await,
        ClientFrame::Close(Some(CLOSE_MESSAGE_TOO_BIG)),
        "pre-hello fragmented binary overflow closes with 1009 before auth"
    );

    let mut c_pre_text = TestWsClient::connect(srv.addr).await;
    c_pre_text
        .send_text(&"b".repeat(MAX_WS_PAYLOAD.saturating_add(1)))
        .await;
    assert_eq!(
        c_pre_text.recv().await,
        ClientFrame::Close(Some(CLOSE_MESSAGE_TOO_BIG)),
        "pre-hello oversized text closes with 1009 before auth"
    );

    for (label, fin, rsv, opcode, masked, payload, want_code) in [
        (
            "unmasked",
            true,
            0,
            1,
            false,
            b"hi".as_slice(),
            CLOSE_PROTOCOL_ERROR,
        ),
        (
            "nonzero rsv",
            true,
            0x40,
            1,
            true,
            b"hi".as_slice(),
            CLOSE_PROTOCOL_ERROR,
        ),
        (
            "invalid opcode",
            true,
            0,
            3,
            true,
            b"".as_slice(),
            CLOSE_PROTOCOL_ERROR,
        ),
        (
            "invalid control opcode",
            true,
            0,
            11,
            true,
            b"".as_slice(),
            CLOSE_PROTOCOL_ERROR,
        ),
        (
            "fragmented ping",
            false,
            0,
            9,
            true,
            b"".as_slice(),
            CLOSE_PROTOCOL_ERROR,
        ),
        (
            "close len 1",
            true,
            0,
            8,
            true,
            &[0x03],
            CLOSE_PROTOCOL_ERROR,
        ),
        (
            "unexpected continuation",
            true,
            0,
            0,
            true,
            b"x".as_slice(),
            CLOSE_PROTOCOL_ERROR,
        ),
        (
            "oversized ping 126 bytes",
            true,
            0,
            9,
            true,
            &[b'p'; 126],
            CLOSE_PROTOCOL_ERROR,
        ),
        (
            "invalid utf8 text",
            true,
            0,
            1,
            true,
            &[0xFF, 0xFE],
            CLOSE_INVALID_PAYLOAD,
        ),
    ] {
        let mut c = TestWsClient::connect(srv.addr).await;
        c.send_raw_frame(fin, rsv, opcode, masked, payload).await;
        assert_eq!(
            c.recv().await,
            ClientFrame::Close(Some(want_code)),
            "transport failure {label} closes with {want_code}"
        );
    }

    let mut c_nested_frag = TestWsClient::connect(srv.addr).await;
    c_nested_frag.send_raw_frame(false, 0, 2, true, b"a").await;
    c_nested_frag.send_raw_frame(true, 0, 2, true, b"b").await;
    assert_eq!(
        c_nested_frag.recv().await,
        ClientFrame::Close(Some(CLOSE_PROTOCOL_ERROR)),
        "new data frame while fragmented message is active closes with 1002"
    );

    let mut c_mid_eof = TestWsClient::connect(srv.addr).await;
    c_mid_eof.send_raw_frame(false, 0, 2, true, b"abc").await;
    drop(c_mid_eof);

    let _rep = srv.stop("signal").await;
    let diag_json = serde_json::to_string(&srv.events()).unwrap();
    assert!(!diag_json.contains(TEST_TOKEN), "secret token excluded");
}

// ---------------------------------------------------------------------------
// AC 17: Diagnostics writer & sanitization unit test
// ---------------------------------------------------------------------------

#[test]
fn diagnostics_json_lines_exclude_secrets_and_write_cleanly() {
    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let diag = Diagnostics::to_writer(Arc::clone(&buf));
    diag.emit(&DiagnosticEvent::StartupReady {
        bind: "127.0.0.1:8742".to_string(),
        auth_mode: "token",
        static_available: true,
    });
    diag.emit(&DiagnosticEvent::Ownership {
        conn_id: 7,
        outcome: "granted",
        action: "record",
        phase: "recording",
    });
    let text = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "two compact JSON lines");
    let first: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(first["event"], "startup_ready", "startup_ready tag");
    assert_eq!(first["auth_mode"], "token", "auth_mode present");
    let second: Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(second["event"], "ownership", "ownership tag");
    assert_eq!(second["conn_id"], 7, "generated conn_id present");

    Diagnostics::stderr().emit(&DiagnosticEvent::Shutdown {
        trigger: "ctrl_c",
        first: true,
        paused_recording: false,
        freed_model: false,
    });
}

// ---------------------------------------------------------------------------
// Transport, writer, hub, shutdown-race and name-helper edge cases
// ---------------------------------------------------------------------------

struct FailIo;

impl tokio::io::AsyncRead for FailIo {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        _buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Err(std::io::Error::other("broken read")))
    }
}

impl tokio::io::AsyncWrite for FailIo {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        _buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::task::Poll::Ready(Err(std::io::Error::other("broken write")))
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Err(std::io::Error::other("broken flush")))
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Err(std::io::Error::other("broken shutdown")))
    }
}

struct ScriptedWriteFailIo {
    read: std::io::Cursor<Vec<u8>>,
    fail_after_writes: usize,
    writes: usize,
}

impl tokio::io::AsyncRead for ScriptedWriteFailIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let mut tmp = vec![0u8; buf.remaining()];
        let n = std::io::Read::read(&mut self.read, &mut tmp).unwrap_or(0);
        buf.put_slice(&tmp[..n]);
        std::task::Poll::Ready(Ok(()))
    }
}

impl tokio::io::AsyncWrite for ScriptedWriteFailIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        if self.writes >= self.fail_after_writes {
            return std::task::Poll::Ready(Err(std::io::Error::other("write failed")));
        }
        self.writes = self.writes.saturating_add(1);
        std::task::Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

fn masked_client_frame(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.push((u8::from(fin) << 7) | (opcode & 0x0F));
    if let Ok(short) = u8::try_from(payload.len())
        && short <= 125
    {
        out.push(0x80 | short);
    } else {
        out.push(0x80 | 0x7E);
        out.extend_from_slice(&u16::try_from(payload.len()).unwrap().to_be_bytes());
    }
    let mask = [0x11, 0x22, 0x33, 0x44];
    out.extend_from_slice(&mask);
    for (i, &b) in payload.iter().enumerate() {
        out.push(b ^ mask[i & 3]);
    }
    out
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "unit test verifies framing edge cases, length boundaries, drain and ownership transitions"
)]
async fn framing_and_name_and_blocking_edge_cases() {
    for (phase, want) in [
        (Phase::Idle, "idle"),
        (Phase::Ready, "ready"),
        (Phase::Recording, "recording"),
        (Phase::Paused, "paused"),
        (Phase::Transcribing, "transcribing"),
        (Phase::Finished, "finished"),
        (Phase::Failed, "failed"),
    ] {
        assert_eq!(phase_name(phase), want, "phase_name {want}");
    }
    for (action, want) in [
        (Action::New, "new"),
        (Action::Record, "record"),
        (Action::Pause, "pause"),
        (Action::Resume, "resume"),
        (Action::Done, "done"),
        (Action::Discard, "discard"),
        (Action::Retry, "retry"),
        (Action::Copy, "copy"),
        (Action::Accept, "accept"),
        (Action::DownloadModel, "downloadModel"),
        (Action::UndoLearn, "undoLearn"),
    ] {
        assert_eq!(action_name(action), want, "action_name {want}");
    }

    // run_blocking converts panic JoinError into shutdown error.
    let join_err = run_blocking(Box::new(|| -> () { panic!("expected test panic") })).await;
    assert_eq!(
        join_err,
        Err("The engine has shut down."),
        "run_blocking handles join error"
    );

    // WsReader: IO error, mid-fragment EOF, and 64-bit length with MSB=1.
    let mut r_io = WsReader::new(Box::new(FailIo));
    assert_eq!(
        r_io.read_message().await,
        Err(WsReadError::Io),
        "IO error mapped to WsReadError::Io"
    );
    let mid_frag = masked_client_frame(false, 2, b"part");
    let mut r_eof = WsReader::new(Box::new(std::io::Cursor::new(mid_frag)));
    assert_eq!(
        r_eof.read_message().await,
        Err(WsReadError::ProtocolError),
        "EOF mid-fragment is protocol error"
    );
    let mut msb_set = vec![0x82, 0xFF]; // fin=1, binary, masked=1, len7=127
    msb_set.extend_from_slice(&(1u64 << 63).to_be_bytes());
    let mut r_msb = WsReader::new(Box::new(std::io::Cursor::new(msb_set)));
    assert_eq!(
        r_msb.read_message().await,
        Err(WsReadError::ProtocolError),
        "64-bit length with MSB=1 rejected"
    );

    // ws_write_frame length encodings: <= 125 (7-bit), 126..=65535 (16-bit), > 65535 (64-bit).
    for (len, want_len7, want_total) in [
        (10_usize, 10_u8, 12_usize),
        (125_usize, 125_u8, 127_usize),
        (126_usize, 126_u8, 130_usize),
        (70_000_usize, 127_u8, 70_010_usize),
    ] {
        let shared_buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let mut writer: BoxWrite = Box::new(VecWriter(Arc::clone(&shared_buf)));
        let payload = vec![b'x'; len];
        ws_write_frame(&mut writer, 1, &payload).await.unwrap();
        let written = shared_buf.lock().unwrap();
        assert_eq!(written[0], 0x81, "FIN + OP_TEXT header byte for len {len}");
        assert_eq!(written[1], want_len7, "length marker for len {len}");
        assert_eq!(
            written.len(),
            want_total,
            "exact framed byte count for len {len}"
        );
    }

    // apply_ownership_transition requires BOTH Record/Resume AND Phase::Recording to grant.
    let mut owner = None;
    assert_eq!(
        crate::server::apply_ownership_transition(&mut owner, 1, Action::Record, Phase::Ready),
        None,
        "Record not reaching Recording grants nothing"
    );
    assert_eq!(owner, None);
    assert_eq!(
        crate::server::apply_ownership_transition(&mut owner, 1, Action::Record, Phase::Recording),
        Some("granted"),
        "Record reaching Recording grants ownership"
    );
    assert_eq!(owner, Some(1));
    assert_eq!(
        crate::server::apply_ownership_transition(&mut owner, 2, Action::Copy, Phase::Recording),
        None,
        "non-Record/Resume action while Recording neither grants nor clears"
    );
    assert_eq!(owner, Some(1));
    assert_eq!(
        crate::server::apply_ownership_transition(&mut owner, 2, Action::Pause, Phase::Paused),
        Some("cleared"),
        "leaving Recording clears ownership"
    );
    assert_eq!(owner, None);

    let mut cursor: crate::server::BoxRead = Box::new(std::io::Cursor::new(vec![0x5Au8; 8192]));
    crate::server::drain_peer_after_close(&mut cursor).await;
    let mut rem = [0u8; 1];
    assert_eq!(
        cursor.read(&mut rem).await.unwrap(),
        0,
        "drain_peer_after_close consumes unread peer bytes"
    );
}

struct VecWriter(Arc<Mutex<Vec<u8>>>);

impl tokio::io::AsyncWrite for VecWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.0.lock().unwrap().extend_from_slice(buf);
        std::task::Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

async fn run_boxed_conn<
    IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
>(
    state: ServerState,
    conn_id: u64,
    io: IO,
) {
    let (r, w) = tokio::io::split(io);
    handle_connection(state, conn_id, Box::new(r), Box::new(w)).await;
}

#[tokio::test]
async fn connection_writer_errors_closed_hub_and_post_drain_refusals() {
    let dir = TempDir::new().unwrap();
    let (diag, events) = Diagnostics::recording();
    let hub = EventHub::new(diag.clone());

    // EventHub unsubscribes immediately on Drop (without needing a subsequent publish).
    assert_eq!(hub.subscriber_count(), 0, "starts with zero subscribers");
    let sub_drop = hub.subscribe(98);
    assert_eq!(hub.subscriber_count(), 1, "one subscriber registered");
    drop(sub_drop);
    assert_eq!(
        hub.subscriber_count(),
        0,
        "Drop unsubscribes subscriber immediately"
    );

    // EventHub drops closed subscriber cleanly without emitting lag.
    let mut sub_closed = hub.subscribe(99);
    sub_closed.close_rx();
    hub.publish(Event::Level(0.1));
    drop(sub_closed);

    let recorder = StreamRecorder::new();
    let pub_hub = hub.clone();
    let engine = FakeEngineBundle::default().open(
        dir.path().join("data"),
        recorder.clone(),
        Arc::new(move |ev| pub_hub.publish(ev)),
    );
    let state = ServerState::new(
        engine.clone(),
        recorder,
        AuthMode::NoToken,
        dir.path().join("web"),
        hub.clone(),
        diag,
        Arc::new(NoopHooks),
    );

    // 1. Pre-auth IO read error.
    run_boxed_conn(state.clone(), 101, FailIo).await;
    // 2. Pre-auth Ping with writer error.
    run_boxed_conn(
        state.clone(),
        102,
        ScriptedWriteFailIo {
            read: std::io::Cursor::new(masked_client_frame(true, 9, b"p")),
            fail_after_writes: 0,
            writes: 0,
        },
    )
    .await;
    // 3. Bootstrap write error after valid hello.
    let hello_bytes = masked_client_frame(true, 1, br#"{"hello":{"token":""}}"#);
    run_boxed_conn(
        state.clone(),
        103,
        ScriptedWriteFailIo {
            read: std::io::Cursor::new(hello_bytes.clone()),
            fail_after_writes: 0,
            writes: 0,
        },
    )
    .await;
    // 4. Live RPC writer error (bootstrap takes 4 writes: head+body x2, then RPC reply fails).
    let mut hello_then_rpc = hello_bytes.clone();
    hello_then_rpc.extend_from_slice(&masked_client_frame(
        true,
        1,
        br#"{"id":1,"call":"app_version","args":{}}"#,
    ));
    run_boxed_conn(
        state.clone(),
        104,
        ScriptedWriteFailIo {
            read: std::io::Cursor::new(hello_then_rpc),
            fail_after_writes: 4,
            writes: 0,
        },
    )
    .await;
    // 5. Authenticated EOF (normal abrupt disconnect after hello).
    run_boxed_conn(
        state.clone(),
        105,
        ScriptedWriteFailIo {
            read: std::io::Cursor::new(hello_bytes.clone()),
            fail_after_writes: 100,
            writes: 0,
        },
    )
    .await;

    // 6. Bootstrap join error propagates cleanly as Err(()).
    let mut sink_writer: BoxWrite = Box::new(tokio::io::sink());
    assert!(
        crate::server::send_bootstrap_result(&mut sink_writer, Err("blocking panicked"))
            .await
            .is_err(),
        "bootstrap join error returns Err(())"
    );

    // 7. Drain connections -> stops admissions, refuses /ws with 503, refuses dictation_act and drops PCM.
    assert_drained_server_refusals(&state, &engine).await;

    assert!(
        events.lock().unwrap().iter().any(|e| matches!(
            e,
            DiagnosticEvent::WriterOutcome {
                reason: "writer_error",
                ..
            }
        )),
        "writer_error diagnostic emitted"
    );
}

struct HelloThenFailIo {
    read: std::io::Cursor<Vec<u8>>,
}

impl tokio::io::AsyncRead for HelloThenFailIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let mut tmp = vec![0u8; buf.remaining()];
        let n = std::io::Read::read(&mut self.read, &mut tmp).unwrap_or(0);
        if n == 0 {
            return std::task::Poll::Ready(Err(std::io::Error::other("post-auth read error")));
        }
        buf.put_slice(&tmp[..n]);
        std::task::Poll::Ready(Ok(()))
    }
}

impl tokio::io::AsyncWrite for HelloThenFailIo {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::task::Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

struct PreOverflowHook {
    hub: EventHub,
}

impl ServerHooks for PreOverflowHook {
    fn before_sample(&self, _conn_id: u64) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        for i in 0..=SUBSCRIBER_QUEUE_CAPACITY {
            self.hub
                .publish(Event::Level(f64::from(u32::try_from(i).unwrap()) / 100.0));
        }
        Box::pin(std::future::ready(()))
    }

    fn before_event_write(&self, _conn_id: u64) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::ready(()))
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "end-to-end test verifies post-auth IO errors, writer overflow and aborted upgrade together"
)]
async fn post_auth_io_error_outer_overflow_and_aborted_upgrade() {
    let dir = TempDir::new().unwrap();
    let (diag, events) = Diagnostics::recording();
    let hub = EventHub::new(diag.clone());
    let recorder = StreamRecorder::new();
    let pub_hub = hub.clone();
    let engine = FakeEngineBundle::default().open(
        dir.path().join("data"),
        recorder.clone(),
        Arc::new(move |ev| pub_hub.publish(ev)),
    );
    let state = ServerState::new(
        engine.clone(),
        recorder.clone(),
        AuthMode::NoToken,
        dir.path().join("web"),
        hub.clone(),
        diag.clone(),
        Arc::new(NoopHooks),
    );

    // Authenticated connection hitting WsReadError::Io (close_code == None).
    let hello_bytes = masked_client_frame(true, 1, br#"{"hello":{"token":""}}"#);
    run_boxed_conn(
        state.clone(),
        201,
        HelloThenFailIo {
            read: std::io::Cursor::new(hello_bytes.clone()),
        },
    )
    .await;

    // Outer writer overflow: queue overflows during before_sample before writer enters Event step.
    let overflow_state = ServerState::new(
        engine.clone(),
        recorder.clone(),
        AuthMode::NoToken,
        dir.path().join("web"),
        hub.clone(),
        diag.clone(),
        Arc::new(PreOverflowHook { hub: hub.clone() }),
    );
    run_boxed_conn(
        overflow_state,
        202,
        ScriptedWriteFailIo {
            read: std::io::Cursor::new(hello_bytes),
            fail_after_writes: 100,
            writes: 0,
        },
    )
    .await;

    // Shutdown while writer is paused inside before_event_write (covers inner shutdown race).
    let stall_hook = Arc::new(StallFirstWriterHook {
        stalled_conn: 1,
        entered: Notify::new(),
        release: Notify::new(),
        paused_once: AtomicBool::new(false),
    });
    let stall_dyn: Arc<dyn ServerHooks> = stall_hook.clone();
    let stall_state = ServerState::new(
        engine.clone(),
        recorder,
        AuthMode::NoToken,
        dir.path().join("web"),
        hub.clone(),
        diag,
        stall_dyn,
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let srv_task = tokio::spawn(serve_axum(listener, router(stall_state.clone()), stop_rx));
    let mut stalled_client = TestWsClient::connect(addr).await;
    let (_hs, _bs) = stalled_client.hello("").await;
    hub.publish(Event::Level(0.25));
    tokio::time::timeout(Duration::from_secs(3), stall_hook.entered.notified())
        .await
        .expect("stalled shutdown writer entered before_event_write");
    let _sent = stop_tx.send(true);
    stall_state.drain_and_close_connections().await;
    stall_hook.release.notify_waiters();
    drop(stalled_client);
    let _joined = srv_task.await;

    // Invalid WebSocket upgrade headers with a valid key still return 400.
    for (label, upg, conn, ver) in [
        ("missing upgrade", "http/2", "Upgrade", "13"),
        ("missing connection", "websocket", "keep-alive", "13"),
        ("wrong version", "websocket", "Upgrade", "12"),
    ] {
        let bad_req = axum::extract::Request::builder()
            .uri("/ws")
            .header("upgrade", upg)
            .header("connection", conn)
            .header("sec-websocket-key", "AAAAAAAAAAAAAAAAAAAAAA==")
            .header("sec-websocket-version", ver)
            .body(axum::body::Body::empty())
            .unwrap();
        let bad_response = ws_upgrade(axum::extract::State(state.clone()), bad_req).await;
        assert_eq!(
            bad_response.status(),
            400,
            "invalid ws_upgrade ({label}) returns 400"
        );
    }

    // Synthetic ws_upgrade request without hyper upgrade extension covers on_upgrade Err path.
    let synth_req = axum::extract::Request::builder()
        .uri("/ws")
        .header("upgrade", "websocket")
        .header("connection", "Upgrade")
        .header("sec-websocket-key", "AAAAAAAAAAAAAAAAAAAAAA==")
        .header("sec-websocket-version", "13")
        .body(axum::body::Body::empty())
        .unwrap();
    let upgrade_response = ws_upgrade(axum::extract::State(state.clone()), synth_req).await;
    assert_eq!(
        upgrade_response.status(),
        101,
        "synthetic upgrade returns 101"
    );
    state.drain_and_close_connections().await;
    let _rep = engine.shutdown();

    assert!(
        events.lock().unwrap().iter().any(|e| matches!(
            e,
            DiagnosticEvent::WriterOutcome {
                conn_id: 202,
                reason: "subscriber_lag",
            }
        )),
        "outer writer overflow emitted subscriber_lag"
    );
}

async fn assert_drained_server_refusals(state: &ServerState, engine: &scriber_engine::Engine) {
    state.drain_and_close_connections().await;
    let act_after_drain = dispatch_text_rpc(
        state,
        106,
        r#"{"id":1,"call":"dictation_act","args":{"action":"new","text":""}}"#,
    )
    .await;
    assert!(
        act_after_drain.contains("shut down"),
        "dictation_act refused after drain: {act_after_drain}"
    );
    let mut counts = AudioCounts::default();
    assert!(
        handle_pcm(state, 106, &[0, 0], &mut counts).await.is_none(),
        "PCM dropped after drain"
    );
    assert_eq!(counts.accepted_frames, 0, "no accepted frames after drain");

    let current_settings = engine.settings();
    let _rep = engine.shutdown();
    let set_after_shutdown = dispatch_text_rpc(
        state,
        107,
        &json!({ "id": 2, "call": "settings_set", "args": { "settings": current_settings } })
            .to_string(),
    )
    .await;
    assert!(
        set_after_shutdown.contains("shut down"),
        "settings_set error propagated: {set_after_shutdown}"
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let srv_task = tokio::spawn(serve_axum(listener, router(state.clone()), stop_rx));
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let req = format!(
        "GET /ws HTTP/1.1\r\nHost: {addr}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: AAAAAAAAAAAAAAAAAAAAAA==\r\nSec-WebSocket-Version: 13\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut resp = vec![0u8; 256];
    let n = stream.read(&mut resp).await.unwrap();
    assert!(
        String::from_utf8_lossy(&resp[..n]).starts_with("HTTP/1.1 503 "),
        "drained /ws returns 503"
    );
    let _sent = stop_tx.send(true);
    let _joined = srv_task.await;
}
