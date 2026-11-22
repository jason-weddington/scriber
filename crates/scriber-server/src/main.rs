//! `scriber-server`: axum HTTP and WebSocket host around `scriber-engine`.

mod diagnostics;
mod server;
#[cfg(test)]
mod tests;

use std::fmt;
use std::future::Future;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::ExitCode;
use std::sync::Arc;

use axum::Router;
use scriber_engine::{
    DataDir, Engine, EngineConfig, Event, ModelSource, ShutdownReport, StreamRecorder,
};
use tokio::net::TcpListener;
use tokio::sync::watch;

use crate::diagnostics::{DiagnosticEvent, Diagnostics};
use crate::server::{Either, EventHub, NoopHooks, ServerHooks, ServerState, race2, router};

/// Default socket address when `SCRIBER_BIND` is unset.
pub(crate) const DEFAULT_BIND: &str = "127.0.0.1:8742";

/// Default data folder under `$HOME` (distinct from the desktop app's folder).
pub(crate) const DEFAULT_DATA_SUBPATH: &str =
    "Library/Application Support/com.jasonweddington.scriber-server";

/// Default static web build folder relative to the working directory.
pub(crate) const DEFAULT_WEB_DIR: &str = "apps/desktop/dist-web";

/// Shared-secret authentication mode for `/ws`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AuthMode {
    /// Clients must present this exact shared secret in `hello.token`.
    Token(String),
    /// Tokenless local development (`SCRIBER_ALLOW_NO_TOKEN=1`): clients must
    /// still send a valid `hello` with an empty `token` string.
    NoToken,
}

impl AuthMode {
    /// Fixed diagnostic label for this auth mode.
    #[must_use]
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Token(_) => "token",
            Self::NoToken => "no_token",
        }
    }

    /// Expected `hello.token` value.
    #[must_use]
    pub(crate) fn expected_token(&self) -> &str {
        match self {
            Self::Token(token) => token,
            Self::NoToken => "",
        }
    }
}

/// Environment-derived server configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServerConfig {
    /// Address to bind the HTTP/WebSocket server on.
    pub(crate) bind: SocketAddr,
    /// Authentication mode for `/ws`.
    pub(crate) auth: AuthMode,
    /// Data directory passed to `EngineConfig`.
    pub(crate) data_dir: PathBuf,
    /// Optional absolute `SCRIBER_MODEL_FOLDER` override persisted at startup.
    pub(crate) model_folder: Option<String>,
    /// Static assets directory for the built web app.
    pub(crate) web_dir: PathBuf,
    /// Speech model source (defaults to `ModelSource::default()`).
    pub(crate) model_source: ModelSource,
}

/// Startup or serving failure annotated with a fixed stage and reason code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StartupError {
    /// Fixed stage name (`"config"`, `"bind"`, `"model_folder"`, `"serve"`).
    pub(crate) stage: &'static str,
    /// Fixed machine-readable reason code.
    pub(crate) reason: &'static str,
    /// Human-readable message without secrets.
    pub(crate) message: String,
}

impl StartupError {
    pub(crate) fn new(
        stage: &'static str,
        reason: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            stage,
            reason,
            message: message.into(),
        }
    }

    #[expect(
        clippy::needless_pass_by_value,
        reason = "passed as function item to Result::map_err"
    )]
    pub(crate) fn config_io(err: std::io::Error) -> Self {
        Self::new("config", "runtime_failed", err.to_string())
    }
}

impl fmt::Display for StartupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

fn non_empty_env(get: &dyn Fn(&str) -> Option<String>, key: &str) -> Option<String> {
    get(key).filter(|val| !val.is_empty())
}

impl ServerConfig {
    /// Parses configuration from environment lookups and working directory.
    ///
    /// # Errors
    ///
    /// Returns [`StartupError`] at stage `"config"` when `SCRIBER_TOKEN` is
    /// missing/empty without `SCRIBER_ALLOW_NO_TOKEN=1`, `SCRIBER_BIND` is not
    /// a valid socket address, `$HOME` is missing for the default data folder,
    /// or `SCRIBER_MODEL_FOLDER` is not an absolute path.
    pub(crate) fn from_env(
        get: &dyn Fn(&str) -> Option<String>,
        cwd: &Path,
    ) -> Result<Self, StartupError> {
        let auth = match non_empty_env(get, "SCRIBER_TOKEN") {
            Some(token) => AuthMode::Token(token),
            None if get("SCRIBER_ALLOW_NO_TOKEN").as_deref() == Some("1") => AuthMode::NoToken,
            None => {
                return Err(StartupError::new(
                    "config",
                    "missing_token",
                    "SCRIBER_TOKEN is required unless SCRIBER_ALLOW_NO_TOKEN=1.",
                ));
            }
        };

        let bind_raw = non_empty_env(get, "SCRIBER_BIND").unwrap_or_else(|| DEFAULT_BIND.into());
        let bind = bind_raw.parse::<SocketAddr>().map_err(|_| {
            StartupError::new(
                "config",
                "invalid_bind",
                "SCRIBER_BIND must be a valid host:port socket address.",
            )
        })?;

        let data_dir = if let Some(dir) = non_empty_env(get, "SCRIBER_DATA") {
            cwd.join(dir)
        } else {
            let home = non_empty_env(get, "HOME").ok_or_else(|| {
                StartupError::new(
                    "config",
                    "missing_home",
                    "HOME must be set when SCRIBER_DATA is unset.",
                )
            })?;
            PathBuf::from(home).join(DEFAULT_DATA_SUBPATH)
        };

        let model_folder = match non_empty_env(get, "SCRIBER_MODEL_FOLDER") {
            Some(folder) if folder.starts_with('/') => Some(folder),
            Some(_) => {
                return Err(StartupError::new(
                    "config",
                    "relative_model_folder",
                    "SCRIBER_MODEL_FOLDER must be an absolute path.",
                ));
            }
            None => None,
        };

        let web_dir = if let Some(dir) = non_empty_env(get, "SCRIBER_WEB_DIR") {
            cwd.join(dir)
        } else {
            cwd.join(DEFAULT_WEB_DIR)
        };

        Ok(Self {
            bind,
            auth,
            data_dir,
            model_folder,
            web_dir,
            model_source: ModelSource::default(),
        })
    }
}

/// Persists `SCRIBER_MODEL_FOLDER` into `settings.json` before `Engine::open`,
/// preserving every other saved setting.
pub(crate) fn persist_model_folder(
    data_dir: &Path,
    model_folder: Option<&str>,
) -> Result<(), StartupError> {
    let Some(folder) = model_folder else {
        return Ok(());
    };
    let files = DataDir::new(data_dir.to_path_buf());
    let mut settings = scriber_core::settings::load(&files);
    settings.model_folder = folder.to_string();
    scriber_core::settings::save(&files, &settings).map_err(|e| {
        StartupError::new(
            "model_folder",
            "settings_save_failed",
            format!("Could not save SCRIBER_MODEL_FOLDER: {e}"),
        )
    })
}

/// Builds the production [`EngineConfig`] for `scriber-server`: shared
/// [`StreamRecorder`], no clipboard, recording library enabled.
#[must_use]
pub(crate) fn production_engine_config(
    data_dir: PathBuf,
    model_source: ModelSource,
    recorder: StreamRecorder,
) -> EngineConfig {
    EngineConfig {
        model: model_source,
        recorder: Some(recorder),
        // clipboard stays None from EngineConfig::new
        library: true,
        ..EngineConfig::new(data_dir)
    }
}

/// Opens the production [`Engine`] for `config`.
pub(crate) fn open_production_engine(
    config: EngineConfig,
    events: Arc<dyn Fn(Event) + Send + Sync>,
) -> Engine {
    Engine::open(config, events)
}

/// Runs axum's HTTP/1.1 server on `listener` until `stop_rx` signals `true`.
pub(crate) async fn serve_axum(
    listener: TcpListener,
    app: Router,
    mut stop_rx: watch::Receiver<bool>,
) -> std::io::Result<()> {
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _done = stop_rx.wait_for(|&v| v).await;
        })
        .await
}

/// Boxed factory creating one [`Engine`] from a data dir, shared [`StreamRecorder`], and event callback.
pub(crate) type EngineFactory =
    Box<dyn FnOnce(PathBuf, StreamRecorder, Arc<dyn Fn(Event) + Send + Sync>) -> Engine + Send>;

/// Boxed serving future used by [`run_lifecycle`].
pub(crate) type ServeFn = Box<
    dyn FnOnce(
            TcpListener,
            Router,
            watch::Receiver<bool>,
        ) -> Pin<Box<dyn Future<Output = std::io::Result<()>> + Send>>
        + Send,
>;

/// Boxed shutdown trigger future returning the trigger label.
pub(crate) type ShutdownSignal = Pin<Box<dyn Future<Output = &'static str> + Send>>;

/// Full server lifecycle shared between production and offline tests:
/// bind listener -> persist startup model folder -> open one engine -> serve ->
/// stop admissions, close/join tracked sockets -> explicit `Engine::shutdown`
/// off runtime workers.
pub(crate) async fn run_lifecycle(
    config: ServerConfig,
    diagnostics: Diagnostics,
    hooks: Arc<dyn ServerHooks>,
    open_engine: EngineFactory,
    on_bound: Box<dyn FnOnce(SocketAddr) + Send>,
    shutdown_signal: ShutdownSignal,
    serve_fn: ServeFn,
) -> Result<ShutdownReport, StartupError> {
    let listener = TcpListener::bind(config.bind).await.map_err(|e| {
        diagnostics.emit(&DiagnosticEvent::StartupFailed {
            stage: "bind",
            reason: "bind_failed",
        });
        StartupError::new(
            "bind",
            "bind_failed",
            format!("Could not bind listener: {e}"),
        )
    })?;
    let bound_addr = listener.local_addr().unwrap_or(config.bind);

    if let Err(err) = persist_model_folder(&config.data_dir, config.model_folder.as_deref()) {
        diagnostics.emit(&DiagnosticEvent::StartupFailed {
            stage: err.stage,
            reason: err.reason,
        });
        return Err(err);
    }

    let hub = EventHub::new(diagnostics.clone());
    let publish_hub = hub.clone();
    let events: Arc<dyn Fn(Event) + Send + Sync> = Arc::new(move |ev| publish_hub.publish(ev));
    let recorder = StreamRecorder::new();
    let engine = open_engine(config.data_dir.clone(), recorder.clone(), events);

    let state = ServerState::new(
        engine.clone(),
        recorder,
        config.auth.clone(),
        config.web_dir.clone(),
        hub,
        diagnostics.clone(),
        hooks,
    );
    let app = router(state.clone());

    diagnostics.emit(&DiagnosticEvent::StartupReady {
        bind: bound_addr.to_string(),
        auth_mode: config.auth.as_str(),
        static_available: config.web_dir.is_dir(),
    });
    on_bound(bound_addr);

    let (stop_tx, stop_rx) = watch::channel(false);
    let serve_fut = serve_fn(listener, app, stop_rx);
    let mut serve_fut = std::pin::pin!(serve_fut);
    let (trigger, serve_err) = match race2(shutdown_signal, serve_fut.as_mut()).await {
        Either::Left(trigger) => {
            let _sent = stop_tx.send(true);
            let res = serve_fut.await;
            (trigger, res.err())
        }
        Either::Right(res) => {
            let _sent = stop_tx.send(true);
            match res {
                Ok(()) => ("signal", None),
                Err(e) => ("serve_error", Some(e)),
            }
        }
    };

    if serve_err.is_some() {
        diagnostics.emit(&DiagnosticEvent::StartupFailed {
            stage: "serve",
            reason: "serve_failed",
        });
    }

    state.drain_and_close_connections().await;
    let shutdown_engine = engine;
    let report = tokio::task::spawn_blocking(move || shutdown_engine.shutdown())
        .await
        .unwrap_or(ShutdownReport {
            first: false,
            paused_recording: false,
            freed_model: false,
        });
    diagnostics.emit(&DiagnosticEvent::Shutdown {
        trigger,
        first: report.first,
        paused_recording: report.paused_recording,
        freed_model: report.freed_model,
    });

    if let Some(err) = serve_err {
        return Err(StartupError::new("serve", "serve_failed", err.to_string()));
    }
    Ok(report)
}

/// Races Ctrl-C and Unix SIGTERM signal futures and returns the winning trigger label.
pub(crate) async fn select_shutdown_trigger(
    ctrl_c: Pin<Box<dyn Future<Output = ()> + Send>>,
    sigterm: Pin<Box<dyn Future<Output = ()> + Send>>,
) -> &'static str {
    match race2(ctrl_c, sigterm).await {
        Either::Left(()) => "ctrl_c",
        Either::Right(()) => "sigterm",
    }
}

/// Parses config and runs `runner`, converting startup errors into `ExitCode::FAILURE`.
pub(crate) fn run_cli(
    get_env: &dyn Fn(&str) -> Option<String>,
    cwd: &Path,
    diagnostics: Diagnostics,
    runner: Box<dyn FnOnce(ServerConfig, Diagnostics) -> Result<ShutdownReport, StartupError>>,
) -> ExitCode {
    let config = match ServerConfig::from_env(get_env, cwd) {
        Ok(cfg) => cfg,
        Err(err) => {
            diagnostics.emit(&DiagnosticEvent::StartupFailed {
                stage: err.stage,
                reason: err.reason,
            });
            return ExitCode::FAILURE;
        }
    };
    match runner(config, diagnostics) {
        Ok(_) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

/// Installed OS shutdown signal listeners (`SIGINT` and Unix `SIGTERM`).
/// Installing them up front guarantees handlers are active before awaiting.
pub(crate) struct OsSignals {
    #[cfg(unix)]
    sigint: tokio::signal::unix::Signal,
    #[cfg(unix)]
    sigterm: tokio::signal::unix::Signal,
}

impl OsSignals {
    /// Installs OS shutdown signal listeners.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if registering Unix signal handlers fails.
    pub(crate) fn install() -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            Ok(Self {
                sigint: signal(SignalKind::interrupt())?,
                sigterm: signal(SignalKind::terminate())?,
            })
        }
        #[cfg(not(unix))]
        {
            Ok(Self {})
        }
    }

    /// Waits for either Ctrl-C (`SIGINT`) or Unix `SIGTERM` and returns the
    /// winning trigger label (`"ctrl_c"` or `"sigterm"`).
    pub(crate) async fn wait(mut self) -> &'static str {
        #[cfg(unix)]
        {
            let ctrl_c: Pin<Box<dyn Future<Output = ()> + Send>> = Box::pin(async move {
                let _recv = self.sigint.recv().await;
            });
            let sigterm: Pin<Box<dyn Future<Output = ()> + Send>> = Box::pin(async move {
                let _recv = self.sigterm.recv().await;
            });
            select_shutdown_trigger(ctrl_c, sigterm).await
        }
        #[cfg(not(unix))]
        {
            let _recv = tokio::signal::ctrl_c().await;
            "ctrl_c"
        }
    }
}

/// Builds the tokio multi-thread runtime and runs the production lifecycle.
pub(crate) fn run_production(
    config: ServerConfig,
    diagnostics: Diagnostics,
) -> Result<ShutdownReport, StartupError> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(StartupError::config_io)?;
    let model_source = config.model_source.clone();
    rt.block_on(async move {
        let signals = OsSignals::install().map_err(StartupError::config_io)?;
        run_lifecycle(
            config,
            diagnostics,
            Arc::new(NoopHooks),
            Box::new(move |data_dir, recorder, events| {
                open_production_engine(
                    production_engine_config(data_dir, model_source, recorder),
                    events,
                )
            }),
            Box::new(|_addr| {}),
            Box::pin(signals.wait()),
            Box::new(|l, a, s| Box::pin(serve_axum(l, a, s))),
        )
        .await
    })
}

fn main() -> ExitCode {
    let cwd = std::env::current_dir().unwrap_or_default();
    run_cli(
        &|key| std::env::var(key).ok(),
        &cwd,
        Diagnostics::stderr(),
        Box::new(run_production),
    )
}
