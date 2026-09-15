pub mod orphan;
pub mod profiles;
pub mod prompts;
pub mod pty;
pub mod status;

use crate::error::AetherError;
use crate::events::{now_millis, LogEvent, LOG_EVENT, STATUS_EVENT};
use crate::state::ConnectionState;
use aether_exit_proxy::Server as FinalProxyServer;
use profiles::ConnectionProfile;
use pty::PtySession;
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

pub struct AetherManager {
    session: Option<PtySession>,
    final_proxy: Option<FinalProxyServer>,
    state: ConnectionState,
    user_requested_stop: bool,
    retry_count: u32,
    // Invalidates old monitors and retry timers when a connection is stopped.
    generation: u64,
}

impl AetherManager {
    pub fn new() -> Self {
        Self {
            session: None,
            final_proxy: None,
            state: ConnectionState::Idle,
            user_requested_stop: false,
            retry_count: 0,
            generation: 0,
        }
    }

    pub fn status(&self) -> ConnectionState {
        self.state.clone()
    }

    fn is_current(&self, generation: u64) -> bool {
        self.generation == generation && !self.user_requested_stop
    }
}

#[derive(Clone)]
struct Attempt {
    binary: PathBuf,
    data_dir: PathBuf,
    profile: ConnectionProfile,
    generation: u64,
}

fn app_data_dir(app: &AppHandle) -> PathBuf {
    app.path()
        .app_data_dir()
        .unwrap_or_else(|_| std::env::temp_dir())
}

fn resolve_binary(app: &AppHandle) -> Result<PathBuf, AetherError> {
    let dir = app
        .path()
        .resource_dir()
        .map_err(|e| AetherError::Internal(e.to_string()))?;
    let name = if cfg!(windows) {
        "aether.exe"
    } else {
        "aether"
    };
    let path = dir.join("binaries").join(name);
    if !path.exists() {
        return Err(AetherError::BinaryMissing(path.display().to_string()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    }
    Ok(path)
}

pub fn start_connect(
    app: AppHandle,
    manager: Arc<Mutex<AetherManager>>,
    profile_override: Option<ConnectionProfile>,
) -> Result<(), AetherError> {
    let profile = profile_override.unwrap_or_else(|| profiles::load(&app));
    profile.final_proxy.config()?;
    let bind: SocketAddr = profile
        .bind_address
        .parse()
        .map_err(|_| AetherError::Internal("Enter a valid local SOCKS5 address".into()))?;
    if bind.port() == 0 {
        return Err(AetherError::Internal(
            "The local SOCKS5 port must be between 1 and 65535".into(),
        ));
    }
    let binary = resolve_binary(&app)?;
    let data_dir = app_data_dir(&app);
    std::fs::create_dir_all(&data_dir).map_err(|e| AetherError::Internal(e.to_string()))?;
    let generation = {
        let mut mgr = manager.lock().unwrap();
        if !matches!(
            mgr.state,
            ConnectionState::Idle | ConnectionState::Error { .. }
        ) {
            return Err(AetherError::AlreadyRunning);
        }
        if status::port_is_live(&bind) {
            return Err(AetherError::PortInUse(bind.port()));
        }
        mgr.generation = mgr.generation.wrapping_add(1);
        mgr.user_requested_stop = false;
        mgr.retry_count = 0;
        mgr.state = ConnectionState::Launching;
        mgr.generation
    };
    let _ = app.emit(STATUS_EVENT, &ConnectionState::Launching);
    spawn_and_monitor(
        app,
        manager,
        Attempt {
            binary,
            data_dir,
            profile,
            generation,
        },
    )
}

fn spawn_and_monitor(
    app: AppHandle,
    manager: Arc<Mutex<AetherManager>>,
    attempt: Attempt,
) -> Result<(), AetherError> {
    // Reserve the public listener before starting the core. The core uses a
    // fresh loopback port; monitor_connect probes THAT port, never our relay.
    let prepared = (|| {
        let mut core_addr = status::parse_bind_address(&attempt.profile.bind_address);
        let mut proxy = None;
        if let Some(config) = attempt.profile.final_proxy.config()? {
            let listener = TcpListener::bind(&attempt.profile.bind_address).map_err(|e| {
                AetherError::FinalProxy(format!("Cannot open the local listener: {e}"))
            })?;
            let reservation = TcpListener::bind("127.0.0.1:0")
                .map_err(|e| AetherError::FinalProxy(e.to_string()))?;
            core_addr = reservation
                .local_addr()
                .map_err(|e| AetherError::FinalProxy(e.to_string()))?;
            let logger = app.clone();
            proxy = Some(
                FinalProxyServer::start(
                    listener,
                    core_addr,
                    config,
                    Arc::new(move |line| {
                        let _ = logger.emit(
                            LOG_EVENT,
                            LogEvent {
                                line,
                                timestamp: now_millis(),
                            },
                        );
                    }),
                )
                .map_err(|e| AetherError::FinalProxy(e.to_string()))?,
            );
            // Aether accepts a bind address, not an inherited listening FD.
            // It will report a bind error if another process wins this port.
            drop(reservation);
        }
        let core_profile = attempt.profile.for_core(core_addr.to_string());
        let (log_tx, log_rx) = mpsc::channel::<LogEvent>();
        let session = pty::spawn(&attempt.binary, &attempt.data_dir, core_profile, log_tx)?;
        Ok::<_, AetherError>((session, proxy, core_addr, log_rx))
    })();
    let (mut session, proxy, core_addr, log_rx) = match prepared {
        Ok(value) => value,
        Err(error) => {
            finish_error(&app, &manager, &attempt, error.to_string(), "launching");
            return Err(error);
        }
    };
    {
        let mut mgr = manager.lock().unwrap();
        if !mgr.is_current(attempt.generation) {
            session.kill();
            return Ok(());
        }
        orphan::write_pid(&attempt.data_dir, session.pid());
        mgr.session = Some(session);
        mgr.final_proxy = proxy;
    }
    let logger = app.clone();
    std::thread::spawn(move || {
        for log in log_rx {
            let _ = logger.emit(LOG_EVENT, &log);
        }
    });
    std::thread::spawn(move || monitor_connect(app, manager, attempt, core_addr));
    Ok(())
}

fn finish_error(
    app: &AppHandle,
    manager: &Arc<Mutex<AetherManager>>,
    attempt: &Attempt,
    message: String,
    phase: &str,
) {
    let mut mgr = manager.lock().unwrap();
    if !mgr.is_current(attempt.generation) {
        return;
    }
    if let Some(session) = mgr.session.as_mut() {
        session.kill();
    }
    mgr.session = None;
    mgr.final_proxy = None;
    mgr.state = ConnectionState::Error {
        message,
        phase: phase.into(),
    };
    let state = mgr.state.clone();
    orphan::clear_pid(&attempt.data_dir);
    let _ = app.emit(STATUS_EVENT, &state);
}

fn handle_unexpected_failure(
    app: AppHandle,
    manager: Arc<Mutex<AetherManager>>,
    attempt: Attempt,
    failure_message: String,
    phase: &'static str,
) {
    let retries = {
        let mut mgr = manager.lock().unwrap();
        if !mgr.is_current(attempt.generation) {
            return;
        }
        if let Some(session) = mgr.session.as_mut() {
            session.kill();
        }
        mgr.session = None;
        mgr.final_proxy = None;
        orphan::clear_pid(&attempt.data_dir);
        mgr.retry_count += 1;
        if mgr.retry_count > status::MAX_AUTO_RETRIES {
            mgr.state = ConnectionState::Error {
                message: format!(
                    "{failure_message} (gave up after {} retries)",
                    status::MAX_AUTO_RETRIES
                ),
                phase: phase.into(),
            };
            let _ = app.emit(STATUS_EVENT, &mgr.state);
            return;
        }
        mgr.state = ConnectionState::Reconnecting {
            attempt: mgr.retry_count,
            max_attempts: status::MAX_AUTO_RETRIES,
        };
        let _ = app.emit(STATUS_EVENT, &mgr.state);
        mgr.retry_count
    };
    std::thread::spawn(move || {
        std::thread::sleep(status::RETRY_BACKOFF[(retries - 1) as usize]);
        {
            let mut mgr = manager.lock().unwrap();
            if !mgr.is_current(attempt.generation) {
                return;
            }
            mgr.state = ConnectionState::Launching;
            let _ = app.emit(STATUS_EVENT, &mgr.state);
        }
        let _ = spawn_and_monitor(app, manager, attempt);
    });
}

fn monitor_connect(
    app: AppHandle,
    manager: Arc<Mutex<AetherManager>>,
    attempt: Attempt,
    core_addr: SocketAddr,
) {
    let deadline = Instant::now() + status::connect_timeout(&attempt.profile.scan_mode);
    let mut announced_connecting = false;
    loop {
        std::thread::sleep(Duration::from_millis(400));
        let mut mgr = manager.lock().unwrap();
        if !mgr.is_current(attempt.generation) {
            return;
        }
        if let Some(exit) = mgr.session.as_mut().and_then(|s| s.try_wait()) {
            mgr.session = None;
            drop(mgr);
            handle_unexpected_failure(
                app,
                manager,
                attempt,
                format!("Aether exited before connecting ({exit})"),
                "connecting",
            );
            return;
        }
        if !announced_connecting && mgr.session.as_ref().is_some_and(|s| s.prompts_done()) {
            mgr.state = ConnectionState::Connecting;
            let _ = app.emit(STATUS_EVENT, &mgr.state);
            announced_connecting = true;
        }
        if status::port_is_live(&core_addr) {
            let probe = mgr.final_proxy.as_ref().map(FinalProxyServer::probe);
            if probe.is_some() {
                mgr.state = ConnectionState::CheckingProxy;
                let _ = app.emit(STATUS_EVENT, &mgr.state);
            }
            drop(mgr);
            if let Some(probe) = probe {
                let _ = app.emit(
                    LOG_EVENT,
                    LogEvent {
                        line: "Checking the final proxy through Aether…".into(),
                        timestamp: now_millis(),
                    },
                );
                if let Err(error) = probe.verify() {
                    finish_error(&app, &manager, &attempt,
                        format!("Final proxy check failed: {error}. Check its address, credentials and permission to connect to example.com:443."),
                        "final_proxy");
                    return;
                }
            }
            let mut mgr = manager.lock().unwrap();
            if !mgr.is_current(attempt.generation) {
                return;
            }
            // The child may have exited while the final proxy was checked.
            if let Some(exit) = mgr.session.as_mut().and_then(|s| s.try_wait()) {
                mgr.session = None;
                drop(mgr);
                handle_unexpected_failure(
                    app,
                    manager,
                    attempt,
                    format!("Aether exited during the connection check ({exit})"),
                    "connecting",
                );
                return;
            }
            mgr.state = ConnectionState::Connected {
                socks_addr: attempt.profile.bind_address.clone(),
                connected_at_ms: now_millis(),
            };
            mgr.retry_count = 0;
            let _ = app.emit(STATUS_EVENT, &mgr.state);
            profiles::save(&app, &attempt.profile);
            drop(mgr);
            monitor_connected(app, manager, attempt);
            return;
        }
        if Instant::now() >= deadline {
            drop(mgr);
            handle_unexpected_failure(
                app,
                manager,
                attempt,
                "Timed out waiting for Aether to connect".into(),
                "connecting",
            );
            return;
        }
    }
}

fn monitor_connected(app: AppHandle, manager: Arc<Mutex<AetherManager>>, attempt: Attempt) {
    loop {
        std::thread::sleep(Duration::from_millis(500));
        let mut mgr = manager.lock().unwrap();
        if !mgr.is_current(attempt.generation) {
            return;
        }
        if let Some(exit) = mgr.session.as_mut().and_then(|s| s.try_wait()) {
            mgr.session = None;
            drop(mgr);
            handle_unexpected_failure(
                app,
                manager,
                attempt,
                format!("Lost connection unexpectedly ({exit})"),
                "connected",
            );
            return;
        }
    }
}

pub fn request_disconnect(
    app: &AppHandle,
    manager: &Arc<Mutex<AetherManager>>,
) -> Result<(), AetherError> {
    let generation = {
        let mut mgr = manager.lock().unwrap();
        if matches!(
            mgr.state,
            ConnectionState::Idle | ConnectionState::Error { .. }
        ) {
            return Err(AetherError::NotConnected);
        }
        mgr.user_requested_stop = true;
        mgr.generation = mgr.generation.wrapping_add(1);
        mgr.retry_count = 0;
        // Close the public listener AND every active/probing relay socket now.
        mgr.final_proxy = None;
        if let Some(session) = mgr.session.as_ref() {
            session.send_ctrl_c();
        }
        if mgr.session.is_none() {
            mgr.state = ConnectionState::Idle;
            let _ = app.emit(STATUS_EVENT, &mgr.state);
            return Ok(());
        }
        mgr.state = ConnectionState::Disconnecting;
        let _ = app.emit(STATUS_EVENT, &mgr.state);
        mgr.generation
    };
    let app = app.clone();
    let manager = Arc::clone(manager);
    std::thread::spawn(move || {
        let deadline = Instant::now() + status::GRACEFUL_SHUTDOWN_GRACE;
        loop {
            std::thread::sleep(Duration::from_millis(200));
            let mut mgr = manager.lock().unwrap();
            if mgr.generation != generation {
                return;
            }
            let exited = mgr.session.as_mut().and_then(|s| s.try_wait()).is_some();
            if exited || Instant::now() >= deadline {
                if !exited {
                    if let Some(session) = mgr.session.as_mut() {
                        session.kill();
                    }
                }
                mgr.session = None;
                orphan::clear_pid(&app_data_dir(&app));
                mgr.state = ConnectionState::Idle;
                let _ = app.emit(STATUS_EVENT, &mgr.state);
                return;
            }
        }
    });
    Ok(())
}

pub fn submit_access_code(
    manager: &Arc<Mutex<AetherManager>>,
    code: String,
) -> Result<(), AetherError> {
    let manager = manager
        .lock()
        .map_err(|_| AetherError::Internal("Aether state is unavailable".into()))?;
    let session = manager.session.as_ref().ok_or(AetherError::NotConnected)?;
    session.send_access_code(&code)
}

pub fn shutdown_blocking(manager: &Arc<Mutex<AetherManager>>, data_dir: &Path) {
    let mut mgr = manager.lock().unwrap();
    mgr.user_requested_stop = true;
    mgr.generation = mgr.generation.wrapping_add(1);
    mgr.final_proxy = None;
    if let Some(session) = mgr.session.as_mut() {
        session.send_ctrl_c();
        std::thread::sleep(Duration::from_millis(500));
        session.kill();
    }
    mgr.session = None;
    orphan::clear_pid(data_dir);
}
