//! Privileged, single-owner Airdrop-compatible protocol service for the Omarchy widget, GPUI, and CLI.
use crate::airdrop;
pub use crate::ipc::{
    ActiveTransfer, DaemonSnapshot, PeerSummary, PendingTransfer, RadioState, ReceiveMode, Reply,
    Request, TransferDirection, WindowState,
};
use anyhow::{Context, Result};
use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::{mpsc, watch, Mutex},
};

/// How long `start_radio` waits for awdlctl to report the radio ready.
const RADIO_READY_TIMEOUT: Duration = Duration::from_secs(10);
/// How long `discover` waits for the peer browse before giving up on it.
const BROWSE_TIMEOUT: Duration = Duration::from_secs(20);
/// Grace period after SIGTERM before SIGKILL when stopping `awdlctl`.
const RADIO_STOP_GRACE: Duration = Duration::from_secs(1);
/// Maximum window `awdlctl discoverable --seconds` accepts per lease.
const MAX_RADIO_WINDOW_SECONDS: u64 = 3600;

const SOCKET: &str = "/run/ac-dc/control.sock";
const ROOT_IDENTITY: &str = "/var/lib/ac-dc/airdrop";

fn default_directory() -> PathBuf {
    if let Some(dl) = std::env::var_os("XDG_DOWNLOAD_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(dl).join("Adhoc");
    }
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("Downloads/Adhoc")
}

fn default_identity() -> PathBuf {
    #[cfg(unix)]
    if unsafe { libc::geteuid() } == 0 {
        return PathBuf::from(ROOT_IDENTITY);
    }
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join(".local/share")
        });
    data_home.join("ac-dc/airdrop")
}

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait RadioHandle: Send + Sync {
    fn renew(&mut self) -> BoxFuture<'_, Result<()>>;
    fn stop(&mut self) -> BoxFuture<'_, ()>;
}

pub trait ServiceOps: Send + Sync {
    fn start_radio(&self, seconds: u64) -> BoxFuture<'_, Result<Box<dyn RadioHandle>>>;
    fn run_airdrop(&self, cfg: airdrop::Config) -> BoxFuture<'_, Result<()>>;
    fn browse_peers<'a>(
        &'a self,
        iface: &'a str,
        identity: &'a Path,
    ) -> BoxFuture<'a, Result<Vec<airdrop::peers::Peer>>>;
    #[allow(clippy::too_many_arguments)]
    fn send_transfer<'a>(
        &'a self,
        host: &'a str,
        port: u16,
        identity: &'a Path,
        name: &'a str,
        files: Vec<PathBuf>,
        links: Vec<String>,
        progress: watch::Sender<(u64, u64)>,
    ) -> BoxFuture<'a, Result<String>>;
    fn always_window_seconds(&self) -> u64 {
        MAX_RADIO_WINDOW_SECONDS
    }
}

struct ChildRadioHandle {
    child: tokio::process::Child,
}

impl RadioHandle for ChildRadioHandle {
    fn renew(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            #[cfg(unix)]
            if let Some(pid) = self.child.id() {
                let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGUSR1) };
                if rc != 0 {
                    anyhow::bail!("SIGUSR1 awdlctl: {}", std::io::Error::last_os_error());
                }
            }
            Ok(())
        })
    }

    fn stop(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            #[cfg(unix)]
            if let Some(pid) = self.child.id() {
                unsafe {
                    libc::kill(pid as libc::pid_t, libc::SIGTERM);
                }
                if tokio::time::timeout(RADIO_STOP_GRACE, self.child.wait())
                    .await
                    .is_ok()
                {
                    return;
                }
            }
            let _ = self.child.kill().await;
            let _ = self.child.wait().await;
        })
    }
}

struct RemoteRadioHandle {
    client: crate::radio_daemon::RemoteRadioClient,
}

impl RadioHandle for RemoteRadioHandle {
    fn renew(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(self.client.renew())
    }
    fn stop(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(self.client.stop())
    }
}

struct ProdServiceOps;

impl ServiceOps for ProdServiceOps {
    fn start_radio(&self, seconds: u64) -> BoxFuture<'_, Result<Box<dyn RadioHandle>>> {
        Box::pin(async move {
            #[cfg(unix)]
            if unsafe { libc::geteuid() } != 0 {
                let client = crate::radio_daemon::RemoteRadioClient::connect_and_start(
                    Path::new(crate::radio_daemon::DEFAULT_SOCKET),
                    seconds,
                )
                .await?;
                return Ok(Box::new(RemoteRadioHandle { client }) as Box<dyn RadioHandle>);
            }
            let mut child = tokio::process::Command::new("/usr/bin/awdlctl")
                .args(["discoverable", "--seconds", &seconds.to_string()])
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .context("start AWDL radio")?;
            let deadline = tokio::time::Instant::now() + RADIO_READY_TIMEOUT;
            loop {
                if let Some(status) = child.try_wait().context("check awdlctl startup")? {
                    let mut stderr = String::new();
                    if let Some(mut pipe) = child.stderr.take() {
                        use tokio::io::AsyncReadExt;
                        let _ = pipe.read_to_string(&mut stderr).await;
                    }
                    let detail = stderr.trim();
                    anyhow::bail!(
                        "awdlctl exited while starting AWDL radio ({status}){}",
                        if detail.is_empty() {
                            String::new()
                        } else {
                            format!(": {detail}")
                        }
                    );
                }
                if crate::health::radio()
                    .await
                    .ok()
                    .is_some_and(|r| r["ready"] == true && r["interface"] == "awdl0")
                {
                    return Ok(Box::new(ChildRadioHandle { child }) as Box<dyn RadioHandle>);
                }
                if tokio::time::Instant::now() >= deadline {
                    let mut handle = ChildRadioHandle { child };
                    handle.stop().await;
                    anyhow::bail!("AWDL radio did not become ready within 10 seconds");
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
    }

    fn run_airdrop(&self, cfg: airdrop::Config) -> BoxFuture<'_, Result<()>> {
        Box::pin(airdrop::run(cfg))
    }

    fn browse_peers<'a>(
        &'a self,
        iface: &'a str,
        identity: &'a Path,
    ) -> BoxFuture<'a, Result<Vec<airdrop::peers::Peer>>> {
        Box::pin(airdrop::peers::browse(iface, identity, 8))
    }

    fn send_transfer<'a>(
        &'a self,
        host: &'a str,
        port: u16,
        identity: &'a Path,
        name: &'a str,
        files: Vec<PathBuf>,
        links: Vec<String>,
        progress: watch::Sender<(u64, u64)>,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let address = crate::discover::socket_target(host, port).await?;
            let client = airdrop::send::Client::new(address, identity)?;
            let receiver = client.discover().await?;
            client
                .transfer_with_progress(name, files, links, Some(progress))
                .await?;
            Ok(receiver)
        })
    }
}

struct Pending {
    sender: String,
    items: Vec<String>,
    decision: tokio::sync::oneshot::Sender<bool>,
}

#[derive(Clone)]
struct DaemonConfig {
    default_directory: PathBuf,
    identity: PathBuf,
    auto_clipboard: bool,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            default_directory: default_directory(),
            identity: default_identity(),
            auto_clipboard: false,
        }
    }
}

struct State {
    receive_task: Option<tokio::task::JoinHandle<()>>,
    send_task: Option<tokio::task::JoinHandle<()>>,
    clipboard_task: Option<tokio::task::JoinHandle<()>>,
    radio_starting: bool,
    pending: Option<Pending>,
    snapshot: DaemonSnapshot,
    tx: watch::Sender<DaemonSnapshot>,
    _tx_rx: watch::Receiver<DaemonSnapshot>,
    radio_ready_tx: watch::Sender<bool>,
    _radio_ready_rx: watch::Receiver<bool>,
    ops: Arc<dyn ServiceOps>,
    cfg: DaemonConfig,
}

impl State {
    fn new(ops: Arc<dyn ServiceOps>, cfg: DaemonConfig) -> Arc<Mutex<Self>> {
        let snapshot = DaemonSnapshot::default();
        let (tx, _tx_rx) = watch::channel(snapshot.clone());
        let (radio_ready_tx, _radio_ready_rx) = watch::channel(false);
        Arc::new(Mutex::new(Self {
            receive_task: None,
            send_task: None,
            clipboard_task: None,
            radio_starting: false,
            pending: None,
            snapshot,
            tx,
            _tx_rx,
            radio_ready_tx,
            _radio_ready_rx,
            ops,
            cfg,
        }))
    }

    fn recompute_phase(&mut self) {
        let receiving = self.receive_task.as_ref().is_some_and(|t| !t.is_finished());
        let sending = self.send_task.as_ref().is_some_and(|t| !t.is_finished());
        self.snapshot.phase = if sending {
            "sending"
        } else if receiving && self.radio_starting {
            "starting"
        } else if receiving {
            "receiving"
        } else if self.snapshot.last_error.is_some() {
            "error"
        } else {
            "idle"
        }
        .into();
    }

    fn publish(&mut self) {
        self.recompute_phase();
        self.snapshot.pending = self.pending.as_ref().map(|p| PendingTransfer {
            sender: p.sender.clone(),
            items: truncate_pending_items(&p.items),
            total_items: p.items.len(),
        });
        let _ = self.tx.send(self.snapshot.clone());
    }
}

/// Panel.qml drops an entire snapshot line over 65536 chars, so a sender that
/// lists hundreds of long item names must not blow that budget; the panel
/// already only displays the first 16 entries anyway.
const MAX_PENDING_ITEMS: usize = 16;
const MAX_PENDING_ITEM_CHARS: usize = 128;

fn truncate_pending_items(items: &[String]) -> Vec<String> {
    items
        .iter()
        .take(MAX_PENDING_ITEMS)
        .map(|s| s.chars().take(MAX_PENDING_ITEM_CHARS).collect())
        .collect()
}

fn now_ms_plus(seconds: u64) -> Option<u64> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    now.checked_add(Duration::from_secs(seconds))
        .map(|d| d.as_millis() as u64)
}

fn ok_reply(state: &State, message: impl Into<String>, data: Option<serde_json::Value>) -> Reply {
    Reply {
        ok: true,
        state: state.snapshot.phase.clone(),
        message: message.into(),
        data,
        snapshot: Some(state.snapshot.clone()),
    }
}

fn err_reply(state: &State, message: impl Into<String>) -> Reply {
    Reply {
        ok: false,
        state: state.snapshot.phase.clone(),
        message: message.into(),
        data: None,
        snapshot: Some(state.snapshot.clone()),
    }
}

pub async fn run(socket: PathBuf) -> Result<()> {
    let cfg = DaemonConfig {
        auto_clipboard: true,
        ..DaemonConfig::default()
    };
    run_with_ops(socket, Arc::new(ProdServiceOps), cfg).await
}

async fn run_with_ops(socket: PathBuf, ops: Arc<dyn ServiceOps>, cfg: DaemonConfig) -> Result<()> {
    if let Some(parent) = socket.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let _ = tokio::fs::remove_file(&socket).await;
    let listener =
        UnixListener::bind(&socket).with_context(|| format!("bind {}", socket.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let is_root = unsafe { libc::geteuid() } == 0;
        if let Some(parent) = socket.parent() {
            let dir_mode = if is_root { 0o750 } else { 0o700 };
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(dir_mode));
        }
        let sock_mode = if is_root { 0o660 } else { 0o600 };
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(sock_mode))?;
        if is_root {
            let grp = unsafe { libc::getgrnam(c"wheel".as_ptr()) };
            if !grp.is_null() {
                let gid = unsafe { (*grp).gr_gid };
                let meta = std::fs::metadata(&socket)?;
                if meta.gid() != gid {
                    let raw = std::ffi::CString::new(socket.as_os_str().as_encoded_bytes())?;
                    let rc = unsafe { libc::chown(raw.as_ptr(), 0, gid) };
                    if rc != 0 {
                        anyhow::bail!(
                            "set daemon socket group: {}",
                            std::io::Error::last_os_error()
                        );
                    }
                }
            }
        }
    }
    let auto_clip = cfg.auto_clipboard;
    let state = State::new(ops, cfg);
    if auto_clip {
        let _ = start_clipboard_sync(state.clone()).await;
    }
    tracing::info!(path=%socket.display(), "ac-dc daemon listening");
    loop {
        let (stream, _) = listener.accept().await?;
        tracing::debug!("accepted daemon control connection");
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = handle(stream, state).await {
                tracing::debug!(error=%e, "daemon client disconnected");
            }
        });
    }
}

async fn handle(stream: UnixStream, state: Arc<Mutex<State>>) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let request: Request = serde_json::from_str(line.trim()).context("invalid daemon request")?;
    tracing::debug!(
        op = %request.op,
        host = ?request.host,
        port = ?request.port,
        name = ?request.name,
        directory = ?request.directory,
        files = request.files.len(),
        links = request.links.len(),
        "daemon request"
    );

    if request.op == "subscribe" {
        return handle_subscribe(reader, write, state).await;
    }

    let reply = match request.op.as_str() {
        "status" => {
            let s = state.lock().await;
            let pending = s
                .pending
                .as_ref()
                .map(|p| serde_json::json!({"sender": p.sender, "items": p.items}));
            ok_reply(&s, s.snapshot.phase.clone(), pending)
        }
        "receive" => start_receive(state.clone(), request).await,
        "stop" => stop(state.clone()).await,
        "cancel" => cancel_send(state.clone()).await,
        "approve" => decide(state.clone(), true).await,
        "reject" => decide(state.clone(), false).await,
        "send" => start_send(state.clone(), request).await,
        "peers" => discover(state.clone()).await,
        "clipboard_report" => report_clipboard(state.clone(), request).await,
        "clipboard_start" => start_clipboard_sync(state.clone()).await,
        "clipboard_stop" => stop_clipboard_sync(state.clone()).await,
        _ => {
            let s = state.lock().await;
            err_reply(&s, "unknown operation")
        }
    };
    write
        .write_all(serde_json::to_string(&reply)?.as_bytes())
        .await?;
    write.write_all(b"\n").await?;
    Ok(())
}

async fn handle_subscribe(
    mut reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    mut write: tokio::net::unix::OwnedWriteHalf,
    state: Arc<Mutex<State>>,
) -> Result<()> {
    let mut rx = {
        let s = state.lock().await;
        s.tx.subscribe()
    };
    let initial = rx.borrow_and_update().clone();
    write
        .write_all(serde_json::to_string(&initial)?.as_bytes())
        .await?;
    write.write_all(b"\n").await?;

    let mut eof_buf = String::new();
    loop {
        tokio::select! {
            changed = rx.changed() => {
                if changed.is_err() {
                    break;
                }
                let snap = rx.borrow_and_update().clone();
                if write.write_all(serde_json::to_string(&snap)?.as_bytes()).await.is_err()
                    || write.write_all(b"\n").await.is_err()
                {
                    break;
                }
            }
            read_res = reader.read_line(&mut eof_buf) => {
                match read_res {
                    Ok(0) | Err(_) => break,
                    Ok(_) => eof_buf.clear(),
                }
            }
        }
    }
    Ok(())
}

fn resolve_receive_mode(request: &Request) -> ReceiveMode {
    if let Some(mode) = request.mode {
        return mode;
    }
    if request.once == Some(true) {
        ReceiveMode::Once
    } else {
        ReceiveMode::Timed
    }
}

async fn start_receive(state: Arc<Mutex<State>>, request: Request) -> Reply {
    let mode = resolve_receive_mode(&request);
    let seconds = request.seconds.unwrap_or(600);
    let ble_wake = request.ble_wake.unwrap_or(true);
    let contacts_only = request.contacts_only.unwrap_or(false);
    let max_disk_percent = request
        .max_disk_percent
        .unwrap_or(airdrop::DEFAULT_MAX_DISK_PERCENT);
    let copy_to_clipboard = request.copy_to_clipboard.unwrap_or(true);
    let mut s = state.lock().await;
    let directory = request
        .directory
        .unwrap_or_else(|| s.cfg.default_directory.clone());
    let identity = s.cfg.identity.clone();
    let name = request.name.unwrap_or_else(|| "Omarchy".into());

    if let Err(e) = airdrop::validate_max_disk_percent(max_disk_percent) {
        return err_reply(&s, format!("{e:#}"));
    }
    if contacts_only {
        match airdrop::has_apple_record(&identity) {
            Ok(true) => {}
            Ok(false) => {
                return err_reply(
                    &s,
                    "Contacts Only mode requires validation_record.cms in identity directory",
                );
            }
            Err(e) => return err_reply(&s, format!("check Apple identity: {e:#}")),
        }
        match crate::contacts::TrustedSenders::load(&crate::contacts::default_path()) {
            Ok(senders) if !senders.is_empty() => {}
            Ok(_) => {
                return err_reply(
                    &s,
                    "Contacts Only mode requires at least one trusted sender (ac-dc contacts add <email-or-phone>)",
                );
            }
            Err(e) => return err_reply(&s, format!("load trusted contacts: {e:#}")),
        }
    }
    if mode != ReceiveMode::Always && !airdrop::RECEIVE_SECONDS.contains(&seconds) {
        return err_reply(&s, airdrop::receive_seconds_error());
    }
    if s.receive_task.as_ref().is_some_and(|t| !t.is_finished()) {
        return ok_reply(&s, "already receiving", None);
    }

    s.radio_starting = true;
    let _ = s.radio_ready_tx.send(false);
    s.snapshot.contacts_only = contacts_only;
    s.snapshot.last_error = None;
    s.snapshot.radio = RadioState {
        ready: false,
        visible: None,
        reason: None,
    };
    s.snapshot.window = Some(match mode {
        ReceiveMode::Timed => WindowState {
            mode,
            remaining_seconds: Some(seconds),
            expires_at_ms: now_ms_plus(seconds),
        },
        ReceiveMode::Once | ReceiveMode::Always => WindowState {
            mode,
            remaining_seconds: None,
            expires_at_ms: None,
        },
    });

    let ops = s.ops.clone();
    let task_state = state.clone();
    s.receive_task = Some(tokio::spawn(async move {
        let lease_seconds = if mode == ReceiveMode::Always {
            ops.always_window_seconds()
        } else {
            seconds
        };
        let result = async {
            let mut radio = ops.start_radio(lease_seconds).await?;
            {
                let mut st = task_state.lock().await;
                st.radio_starting = false;
                st.snapshot.radio = RadioState {
                    ready: true,
                    visible: Some(true),
                    reason: None,
                };
                let _ = st.radio_ready_tx.send(true);
                st.publish();
            }

            let run_result = run_receive_loop(
                &*ops,
                &mut *radio,
                task_state.clone(),
                mode,
                lease_seconds,
                ble_wake,
                contacts_only,
                max_disk_percent,
                copy_to_clipboard,
                directory,
                identity,
                name,
            )
            .await;

            radio.stop().await;
            run_result
        }
        .await;

        let mut st = task_state.lock().await;
        st.radio_starting = false;
        let _ = st.radio_ready_tx.send(false);
        st.pending = None;
        st.receive_task = None;
        st.snapshot.contacts_only = false;
        st.snapshot.window = None;
        st.snapshot.radio = RadioState {
            ready: false,
            visible: None,
            reason: result.as_ref().err().map(|e| format!("{e:#}")),
        };
        if let Err(e) = result {
            let msg = format!("{e:#}");
            tracing::error!(error=%msg, "Airdrop-compatible receiver stopped");
            st.snapshot.last_error = Some(msg);
        }
        st.publish();
    }));

    s.publish();
    ok_reply(&s, "receive window starting", None)
}

#[allow(clippy::too_many_arguments)]
async fn run_receive_loop(
    ops: &dyn ServiceOps,
    radio: &mut dyn RadioHandle,
    task_state: Arc<Mutex<State>>,
    mode: ReceiveMode,
    lease_seconds: u64,
    ble_wake: bool,
    contacts_only: bool,
    max_disk_percent: u8,
    copy_to_clipboard: bool,
    directory: PathBuf,
    identity: PathBuf,
    name: String,
) -> Result<()> {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    tick.tick().await;

    loop {
        let (approval_tx, mut approval_rx) = mpsc::channel(8);
        let receive = ops.run_airdrop(airdrop::Config {
            radio_managed: true,
            ble_wake,
            approval: Some(approval_tx),
            iface: "awdl0".into(),
            directory: directory.clone(),
            identity: identity.clone(),
            name: name.clone(),
            port: 8771,
            seconds: lease_seconds,
            once: mode == ReceiveMode::Once,
            notify: true,
            open_destination: false,
            visibility: if contacts_only {
                airdrop::VisibilityMode::ContactsOnly
            } else {
                airdrop::VisibilityMode::Everyone
            },
            max_disk_percent,
            copy_to_clipboard,
        });
        tokio::pin!(receive);

        let iter_res = loop {
            tokio::select! {
                result = &mut receive => break result,
                Some(incoming) = approval_rx.recv() => {
                    let mut st = task_state.lock().await;
                    st.pending = Some(Pending {
                        sender: incoming.sender,
                        items: incoming.items,
                        decision: incoming.decision,
                    });
                    st.publish();
                }
                _ = tick.tick(), if mode == ReceiveMode::Timed => {
                    let mut st = task_state.lock().await;
                    if let Some(w) = st.snapshot.window.as_mut() {
                        if let Some(rem) = w.remaining_seconds.as_mut() {
                            *rem = rem.saturating_sub(1);
                        }
                    }
                    st.publish();
                }
            }
        };

        iter_res?;
        if mode != ReceiveMode::Always {
            return Ok(());
        }
        radio.renew().await?;
    }
}

async fn stop(state: Arc<Mutex<State>>) -> Reply {
    let mut s = state.lock().await;
    if let Some(task) = s.receive_task.take() {
        task.abort();
    }
    s.radio_starting = false;
    let _ = s.radio_ready_tx.send(false);
    s.pending = None;
    s.snapshot.contacts_only = false;
    s.snapshot.window = None;
    s.snapshot.last_error = None;
    s.snapshot.radio = RadioState {
        ready: false,
        visible: None,
        reason: None,
    };
    s.publish();
    ok_reply(&s, "stopped", None)
}

async fn cancel_send(state: Arc<Mutex<State>>) -> Reply {
    let mut s = state.lock().await;
    if let Some(task) = s.send_task.take() {
        task.abort();
    }
    s.snapshot.active_transfer = None;
    s.publish();
    ok_reply(&s, "send cancelled", None)
}

async fn decide(state: Arc<Mutex<State>>, accepted: bool) -> Reply {
    let mut s = state.lock().await;
    let Some(pending) = s.pending.take() else {
        return err_reply(&s, "no transfer is waiting for approval");
    };
    let _ = pending.decision.send(accepted);
    s.publish();
    ok_reply(
        &s,
        if accepted {
            "transfer approved"
        } else {
            "transfer rejected"
        },
        None,
    )
}

async fn wait_for_active_radio(state: &Arc<Mutex<State>>) -> Result<bool> {
    let (has_receive, mut ready_rx) = {
        let s = state.lock().await;
        let has = s.receive_task.as_ref().is_some_and(|t| !t.is_finished());
        (has, s.radio_ready_tx.subscribe())
    };
    if !has_receive {
        return Ok(false);
    }
    if *ready_rx.borrow() {
        return Ok(true);
    }
    let _ = tokio::time::timeout(RADIO_READY_TIMEOUT, ready_rx.wait_for(|v| *v)).await;
    let ready = *ready_rx.borrow();
    Ok(ready)
}

async fn discover(state: Arc<Mutex<State>>) -> Reply {
    let (ops, identity) = {
        let s = state.lock().await;
        (s.ops.clone(), s.cfg.identity.clone())
    };
    let reused_radio = wait_for_active_radio(&state).await.unwrap_or(false);
    let mut owned_radio = if reused_radio {
        None
    } else {
        match ops.start_radio(30).await {
            Ok(handle) => Some(handle),
            Err(e) => {
                let s = state.lock().await;
                return err_reply(&s, format!("start radio: {e:#}"));
            }
        }
    };

    let result = tokio::time::timeout(BROWSE_TIMEOUT, ops.browse_peers("awdl0", &identity)).await;
    if let Some(mut handle) = owned_radio.take() {
        handle.stop().await;
    }

    let mut s = state.lock().await;
    match result {
        Ok(Ok(peers)) => {
            s.snapshot.peers = peers
                .iter()
                .map(|p| PeerSummary {
                    name: p.name.clone(),
                    instance: p.instance.clone(),
                    address: p.address.to_string(),
                })
                .collect();
            s.publish();
            ok_reply(
                &s,
                format!("{} recipient(s)", peers.len()),
                Some(serde_json::to_value(peers).unwrap_or_default()),
            )
        }
        Ok(Err(e)) => err_reply(&s, format!("{e:#}")),
        Err(e) => err_reply(&s, format!("{e:#}")),
    }
}

async fn start_send(state: Arc<Mutex<State>>, request: Request) -> Reply {
    let host = match request.host {
        Some(v) => v,
        None => {
            let s = state.lock().await;
            return err_reply(&s, "send requires host");
        }
    };
    let port = request.port.unwrap_or(8770);
    let name = request.name.unwrap_or_else(|| "Linux".into());
    let files = request.files;
    let links = request.links;

    let mut s = state.lock().await;
    if s.send_task.as_ref().is_some_and(|t| !t.is_finished()) {
        return err_reply(&s, "another send is already active");
    }

    let ops = s.ops.clone();
    let identity = s.cfg.identity.clone();
    s.snapshot.last_error = None;
    s.snapshot.active_transfer = Some(ActiveTransfer {
        direction: TransferDirection::Send,
        target: host.clone(),
        sent_bytes: 0,
        total_bytes: None,
    });

    let task_state = state.clone();
    s.send_task = Some(tokio::spawn(async move {
        let reused_radio = wait_for_active_radio(&task_state).await.unwrap_or(false);
        let result = async {
            let mut owned_radio = if reused_radio {
                None
            } else {
                Some(ops.start_radio(600).await?)
            };
            let (prog_tx, mut prog_rx) = watch::channel((0u64, 0u64));
            let prog_state = task_state.clone();
            let prog_task = tokio::spawn(async move {
                while prog_rx.changed().await.is_ok() {
                    let (sent, total) = *prog_rx.borrow_and_update();
                    let mut st = prog_state.lock().await;
                    if let Some(active) = st.snapshot.active_transfer.as_mut() {
                        active.sent_bytes = sent;
                        active.total_bytes = Some(total);
                    }
                    st.publish();
                }
            });
            let send_res = ops
                .send_transfer(&host, port, &identity, &name, files, links, prog_tx)
                .await;
            let _ = prog_task.await;
            if let Some(mut handle) = owned_radio.take() {
                handle.stop().await;
            }
            send_res
        }
        .await;

        let mut st = task_state.lock().await;
        st.snapshot.active_transfer = None;
        st.send_task = None;
        if let Err(e) = result {
            let msg = format!("{e:#}");
            tracing::error!(error=%msg, "Airdrop-compatible send failed");
            st.snapshot.last_error = Some(msg);
        }
        st.publish();
    }));

    s.publish();
    ok_reply(&s, "send started", None)
}

async fn report_clipboard(state: Arc<Mutex<State>>, request: Request) -> Reply {
    let mut s = state.lock().await;
    s.snapshot.clipboard = request.clipboard;
    s.publish();
    ok_reply(&s, "clipboard status updated", None)
}

async fn start_clipboard_sync(state: Arc<Mutex<State>>) -> Reply {
    let keys =
        crate::keystore::resolve_path(None, crate::keystore::default_keys_path(), "keys.json");
    let identity = crate::keystore::resolve_path(
        None,
        crate::keystore::default_rpidentity_path(),
        "rpidentity.json",
    );
    if !keys.exists() || !identity.exists() {
        let mut s = state.lock().await;
        s.snapshot.clipboard = Some(crate::ipc::ClipboardStatus {
            available: false,
            state: "unpaired".into(),
            last_item_kind: None,
            last_error: Some("Run ac-dc receive-key & ac-dc pair".into()),
        });
        s.publish();
        return err_reply(&s, "clipboard sync requires keys.json and rpidentity.json");
    }

    let mut s = state.lock().await;
    if s.clipboard_task.as_ref().is_some_and(|t| !t.is_finished()) {
        return ok_reply(&s, "clipboard sync already running", None);
    }

    let (status_tx, mut status_rx) = tokio::sync::mpsc::unbounded_channel();
    let reporter_state = state.clone();
    tokio::spawn(async move {
        while let Some(status) = status_rx.recv().await {
            let mut st = reporter_state.lock().await;
            st.snapshot.clipboard = Some(status);
            st.publish();
        }
    });

    let task_state = state.clone();
    s.clipboard_task = Some(tokio::spawn(async move {
        let res = crate::orchestrate::run_with_reporter(
            crate::orchestrate::AutoConfig {
                keys,
                identity_path: identity,
                instance: None,
                host: None,
                port: None,
                loopback: false,
                notify: true,
            },
            Some(status_tx),
        )
        .await;
        let mut st = task_state.lock().await;
        st.clipboard_task = None;
        if let Err(e) = res {
            let msg = format!("{e:#}");
            tracing::warn!(error=%msg, "clipboard auto-sync stopped");
            st.snapshot.clipboard = Some(crate::ipc::ClipboardStatus {
                available: false,
                state: "error".into(),
                last_item_kind: None,
                last_error: Some(msg),
            });
            st.publish();
        }
    }));
    ok_reply(&s, "clipboard sync started", None)
}

async fn stop_clipboard_sync(state: Arc<Mutex<State>>) -> Reply {
    let mut s = state.lock().await;
    if let Some(task) = s.clipboard_task.take() {
        task.abort();
    }
    if let Some(c) = s.snapshot.clipboard.as_mut() {
        c.state = "paused".into();
    }
    s.publish();
    ok_reply(&s, "clipboard sync paused", None)
}

/// Most handlers answer from state they already hold, so a request still
/// outstanding after this long is one the daemon will never answer. The UI awaits
/// `ctl` in its single engine loop, where an indefinite wait also blocks stop,
/// approve and quit.
const CTL_TIMEOUT: Duration = Duration::from_secs(5);

/// The work `discover` does before it replies, and therefore what a `peers`
/// request has to be allowed to wait for. The readiness loop checks its deadline
/// only after awaiting a probe, so a probe starting just inside
/// `RADIO_READY_TIMEOUT` still runs for up to `health::PROBE_TIMEOUT` more.
const DISCOVER_WORST_CASE: Duration = RADIO_READY_TIMEOUT
    .saturating_add(crate::health::PROBE_TIMEOUT)
    .saturating_add(BROWSE_TIMEOUT);

/// `peers` is the exception: `discover` works before it replies, so its deadline
/// covers that work plus the slack every other operation gets.
fn ctl_timeout(op: &str) -> Duration {
    match op {
        "peers" => DISCOVER_WORST_CASE + CTL_TIMEOUT,
        _ => CTL_TIMEOUT,
    }
}

pub async fn ctl(socket: PathBuf, request: Request) -> Result<Reply> {
    tokio::time::timeout(ctl_timeout(&request.op), async move {
        tracing::debug!(socket=%socket.display(), op=%request.op, "connecting to daemon");
        let mut stream = UnixStream::connect(socket)
            .await
            .context("connect ac-dc daemon")?;
        stream
            .write_all(serde_json::to_string(&request)?.as_bytes())
            .await?;
        stream.write_all(b"\n").await?;
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).await?;
        tracing::debug!(response=%line.trim(), "daemon response received");
        serde_json::from_str(&line).context("parse daemon response")
    })
    .await
    .context("ac-dc daemon did not respond")?
}

/// Stream `DaemonSnapshot` JSON lines to stdout until EOF, BrokenPipe, or signal.
pub async fn ctl_subscribe(socket: PathBuf) -> Result<()> {
    let mut stream = UnixStream::connect(&socket)
        .await
        .with_context(|| format!("connect ac-dc daemon at {}", socket.display()))?;
    stream
        .write_all(serde_json::to_string(&request("subscribe"))?.as_bytes())
        .await?;
    stream.write_all(b"\n").await?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let shutdown = crate::shutdown();
    tokio::pin!(shutdown);
    loop {
        line.clear();
        tokio::select! {
            _ = &mut shutdown => break,
            res = reader.read_line(&mut line) => {
                match res {
                    Ok(0) => break,
                    Ok(_) => {
                        use std::io::Write;
                        let mut out = std::io::stdout().lock();
                        if let Err(e) = out.write_all(line.as_bytes()).and_then(|_| out.flush()) {
                            if e.kind() == std::io::ErrorKind::BrokenPipe {
                                break;
                            }
                            return Err(e.into());
                        }
                    }
                    Err(e) => return Err(e.into()),
                }
            }
        }
    }
    Ok(())
}

pub fn default_socket() -> PathBuf {
    if let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(runtime_dir).join("ac-dc/control.sock");
    }
    PathBuf::from(SOCKET)
}

pub fn request(op: impl Into<String>) -> Request {
    Request {
        op: op.into(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct MockRadio {
        stopped: Arc<AtomicBool>,
        renewed: Arc<AtomicUsize>,
    }

    impl RadioHandle for MockRadio {
        fn renew(&mut self) -> BoxFuture<'_, Result<()>> {
            Box::pin(async move {
                self.renewed.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        }
        fn stop(&mut self) -> BoxFuture<'_, ()> {
            Box::pin(async move {
                self.stopped.store(true, Ordering::SeqCst);
            })
        }
    }

    struct MockOps {
        radio_starts: AtomicUsize,
        radio_stopped: Arc<AtomicBool>,
        radio_renewed: Arc<AtomicUsize>,
        airdrop_runs: AtomicUsize,
        always_secs: u64,
        trigger_approval: bool,
    }

    impl MockOps {
        fn new() -> Self {
            Self {
                radio_starts: AtomicUsize::new(0),
                radio_stopped: Arc::new(AtomicBool::new(false)),
                radio_renewed: Arc::new(AtomicUsize::new(0)),
                airdrop_runs: AtomicUsize::new(0),
                always_secs: 2,
                trigger_approval: false,
            }
        }
    }

    impl ServiceOps for MockOps {
        fn start_radio(&self, _seconds: u64) -> BoxFuture<'_, Result<Box<dyn RadioHandle>>> {
            self.radio_starts.fetch_add(1, Ordering::SeqCst);
            let stopped = self.radio_stopped.clone();
            let renewed = self.radio_renewed.clone();
            Box::pin(
                async move { Ok(Box::new(MockRadio { stopped, renewed }) as Box<dyn RadioHandle>) },
            )
        }
        fn run_airdrop(&self, cfg: airdrop::Config) -> BoxFuture<'_, Result<()>> {
            let run_idx = self.airdrop_runs.fetch_add(1, Ordering::SeqCst);
            let trigger_approval = self.trigger_approval;
            Box::pin(async move {
                if trigger_approval && run_idx == 0 {
                    if let Some(tx) = cfg.approval {
                        let (dtx, drx) = tokio::sync::oneshot::channel();
                        let _ = tx
                            .send(airdrop::Incoming {
                                sender: "Test Mac".into(),
                                items: vec!["photo.jpg".into()],
                                decision: dtx,
                            })
                            .await;
                        let accepted = drx.await.unwrap_or(false);
                        if accepted && cfg.once {
                            return Ok(());
                        }
                    }
                }
                tokio::time::sleep(Duration::from_millis(80)).await;
                Ok(())
            })
        }
        fn browse_peers<'a>(
            &'a self,
            _iface: &'a str,
            _identity: &'a Path,
        ) -> BoxFuture<'a, Result<Vec<airdrop::peers::Peer>>> {
            Box::pin(async move {
                Ok(vec![airdrop::peers::Peer {
                    name: "iPhone".into(),
                    instance: "abc._airdrop._tcp.local.".into(),
                    address: "[fe80::1]:8770".parse().unwrap(),
                }])
            })
        }
        fn send_transfer<'a>(
            &'a self,
            _host: &'a str,
            _port: u16,
            _identity: &'a Path,
            _name: &'a str,
            _files: Vec<PathBuf>,
            _links: Vec<String>,
            progress: watch::Sender<(u64, u64)>,
        ) -> BoxFuture<'a, Result<String>> {
            Box::pin(async move {
                let _ = progress.send((50, 100));
                tokio::time::sleep(Duration::from_millis(20)).await;
                let _ = progress.send((100, 100));
                Ok("iPhone".into())
            })
        }
        fn always_window_seconds(&self) -> u64 {
            self.always_secs
        }
    }

    fn test_state(ops: Arc<dyn ServiceOps>) -> Arc<Mutex<State>> {
        State::new(
            ops,
            DaemonConfig {
                default_directory: PathBuf::from("/tmp"),
                identity: PathBuf::from("/tmp"),
                auto_clipboard: false,
            },
        )
    }

    #[tokio::test]
    async fn start_receive_rejects_a_window_outside_the_allowed_range() {
        for seconds in [0, 3601] {
            let state = test_state(Arc::new(MockOps::new()));
            let mut req = request("receive");
            req.seconds = Some(seconds);

            let reply = start_receive(state.clone(), req).await;

            assert!(!reply.ok, "{seconds}s was accepted");
            assert!(
                reply.message.contains("1..3600"),
                "unexpected message: {}",
                reply.message
            );
            let s = state.lock().await;
            assert!(
                s.receive_task.is_none(),
                "{seconds}s started a receive task"
            );
            assert_eq!(s.snapshot.phase, "idle");
        }
    }

    #[tokio::test]
    async fn subscribe_streams_receive_approval_and_once_completion() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("control.sock");
        let mut ops = MockOps::new();
        ops.trigger_approval = true;
        let ops = Arc::new(ops);

        let srv_sock = socket.clone();
        let srv_ops = ops.clone();
        let server = tokio::spawn(async move {
            let _ = run_with_ops(srv_sock, srv_ops, DaemonConfig::default()).await;
        });
        for _ in 0..50 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let mut sub = UnixStream::connect(&socket).await.unwrap();
        sub.write_all(b"{\"op\":\"subscribe\"}\n").await.unwrap();
        let mut reader = BufReader::new(sub);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let initial: DaemonSnapshot = serde_json::from_str(&line).unwrap();
        assert_eq!(initial.phase, "idle");

        let mut req = request("receive");
        req.mode = Some(ReceiveMode::Once);
        let reply = ctl(socket.clone(), req).await.unwrap();
        assert!(reply.ok);

        let mut saw_pending = false;
        let mut saw_idle_after = false;
        let mut approved = false;
        for _ in 0..30 {
            line.clear();
            tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut line))
                .await
                .unwrap()
                .unwrap();
            let snap: DaemonSnapshot = serde_json::from_str(&line).unwrap();
            if snap.pending.is_some() && !approved {
                saw_pending = true;
                approved = true;
                let r = ctl(socket.clone(), request("approve")).await.unwrap();
                assert!(r.ok);
            }
            if approved && snap.phase == "idle" && snap.pending.is_none() {
                saw_idle_after = true;
                break;
            }
        }
        assert!(
            saw_pending,
            "never saw pending approval in subscribe stream"
        );
        assert!(
            saw_idle_after,
            "never returned to idle with cleared pending"
        );
        assert!(ops.radio_stopped.load(Ordering::SeqCst));
        server.abort();
    }

    #[tokio::test]
    async fn peers_and_send_reuse_active_receive_radio() {
        let ops = Arc::new(MockOps::new());
        let state = test_state(ops.clone());

        let mut recv_req = request("receive");
        recv_req.mode = Some(ReceiveMode::Always);
        assert!(start_receive(state.clone(), recv_req).await.ok);
        tokio::time::sleep(Duration::from_millis(20)).await;

        let peers_reply = discover(state.clone()).await;
        assert!(peers_reply.ok);
        assert_eq!(ops.radio_starts.load(Ordering::SeqCst), 1);

        let mut send_req = request("send");
        send_req.host = Some("fe80::1%awdl0".into());
        assert!(start_send(state.clone(), send_req).await.ok);
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(ops.radio_starts.load(Ordering::SeqCst), 1);

        let stop_reply = stop(state.clone()).await;
        assert!(stop_reply.ok);
        assert_eq!(stop_reply.state, "idle");
    }

    #[tokio::test]
    async fn always_mode_renews_radio_across_windows() {
        let ops = Arc::new(MockOps::new());
        let state = test_state(ops.clone());
        let mut req = request("receive");
        req.mode = Some(ReceiveMode::Always);
        assert!(start_receive(state.clone(), req).await.ok);

        for _ in 0..30 {
            if ops.radio_renewed.load(Ordering::SeqCst) >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(ops.radio_renewed.load(Ordering::SeqCst) >= 1);
        let _ = stop(state.clone()).await;
    }

    #[tokio::test]
    async fn start_receive_rejects_invalid_disk_percent_and_missing_contacts_identity() {
        let state = test_state(Arc::new(MockOps::new()));
        for pct in [0, 91] {
            let mut req = request("receive");
            req.max_disk_percent = Some(pct);
            let reply = start_receive(state.clone(), req).await;
            assert!(!reply.ok, "max_disk_percent={pct} was accepted");
        }

        let mut req = request("receive");
        req.contacts_only = Some(true);
        let reply = start_receive(state.clone(), req).await;
        assert!(
            !reply.ok,
            "contacts_only without validation_record.cms was accepted"
        );
    }

    #[tokio::test]
    async fn clipboard_report_updates_snapshot_and_broadcasts() {
        let ops = Arc::new(MockOps::new());
        let state = test_state(ops);
        let mut rx = state.lock().await.tx.subscribe();

        let mut req = request("clipboard_report");
        req.clipboard = Some(crate::ipc::ClipboardStatus {
            available: true,
            state: "listening".into(),
            last_item_kind: Some("public.utf8-plain-text".into()),
            last_error: None,
        });
        let reply = report_clipboard(state.clone(), req).await;
        assert!(reply.ok);
        rx.changed().await.unwrap();
        let snap = rx.borrow().clone();
        assert_eq!(
            snap.clipboard.as_ref().map(|c| c.state.as_str()),
            Some("listening")
        );
    }

    #[test]
    fn peers_outlasts_the_radio_and_browse_it_waits_on() {
        // The readiness loop can overrun its own deadline by one probe, so the
        // probe budget counts too; leaving it out is what made 35s too tight.
        let work = RADIO_READY_TIMEOUT + crate::health::PROBE_TIMEOUT + BROWSE_TIMEOUT;
        assert!(
            ctl_timeout("peers") > work,
            "peers deadline {:?} does not cover {:?}",
            ctl_timeout("peers"),
            work
        );
        assert_eq!(ctl_timeout("status"), CTL_TIMEOUT);
        assert_eq!(ctl_timeout("receive"), CTL_TIMEOUT);
    }

    // `start_paused` lets the runtime jump the CTL_TIMEOUT deadline as soon as
    // both sides are idle, so this costs no wall-clock time.
    #[tokio::test(start_paused = true)]
    async fn ctl_gives_up_when_the_daemon_accepts_but_never_answers() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("control.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let mute = tokio::spawn(async move {
            let _connection = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });

        let error = tokio::time::timeout(CTL_TIMEOUT * 4, ctl(socket, request("status")))
            .await
            .expect("ctl ignored its own deadline")
            .unwrap_err();

        assert!(
            error.to_string().contains("did not respond"),
            "unexpected error: {error:#}"
        );
        mute.abort();
    }

    #[test]
    fn publish_truncates_a_pending_snapshot_under_the_panel_display_limit() {
        let state = test_state(Arc::new(MockOps::new()));
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let mut s = state.try_lock().unwrap();
        s.pending = Some(Pending {
            sender: "Test Mac".into(),
            items: (0..350).map(|i| "x".repeat(200) + &i.to_string()).collect(),
            decision: tx,
        });
        s.publish();
        let pending = s.snapshot.pending.as_ref().expect("pending snapshot");
        assert_eq!(pending.total_items, 350);
        assert!(
            pending.items.len() <= MAX_PENDING_ITEMS,
            "{} items were not truncated",
            pending.items.len()
        );
        let json = serde_json::to_string(&s.snapshot).unwrap();
        assert!(
            json.len() < 65536,
            "snapshot is {} bytes, still over the panel's display limit",
            json.len()
        );
    }
}
