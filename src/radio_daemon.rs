//! Minimal root-only AWDL radio broker (`/run/ac-dc/radio.sock`) for the user-scoped `ac-dc` daemon.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::Mutex,
};

pub const DEFAULT_SOCKET: &str = "/run/ac-dc/radio.sock";
const RADIO_READY_TIMEOUT: Duration = Duration::from_secs(10);
const RADIO_STOP_GRACE: Duration = Duration::from_secs(1);

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Deserialize, Serialize)]
pub struct RadioRequest {
    pub op: String,
    #[serde(default)]
    pub seconds: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RadioReply {
    pub ok: bool,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<Vec<String>>,
}

pub trait RadioChild: Send + Sync {
    fn renew(&mut self) -> BoxFuture<'_, Result<()>>;
    fn stop(&mut self) -> BoxFuture<'_, ()>;
}

pub trait RadioBackend: Send + Sync {
    fn spawn_discoverable(&self, seconds: u64) -> BoxFuture<'_, Result<Box<dyn RadioChild>>>;
    fn run_events(&self, seconds: u64) -> BoxFuture<'_, Result<Vec<String>>>;
}

struct ChildProcess {
    child: tokio::process::Child,
}

impl RadioChild for ChildProcess {
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

struct ProdRadioBackend;

impl RadioBackend for ProdRadioBackend {
    fn spawn_discoverable(&self, seconds: u64) -> BoxFuture<'_, Result<Box<dyn RadioChild>>> {
        Box::pin(async move {
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
                    return Ok(Box::new(ChildProcess { child }) as Box<dyn RadioChild>);
                }
                if tokio::time::Instant::now() >= deadline {
                    let mut handle = ChildProcess { child };
                    handle.stop().await;
                    anyhow::bail!("AWDL radio did not become ready within 10 seconds");
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
    }

    fn run_events(&self, seconds: u64) -> BoxFuture<'_, Result<Vec<String>>> {
        Box::pin(async move {
            let output = tokio::process::Command::new("/usr/bin/awdlctl")
                .args(["events", "--seconds", &seconds.to_string()])
                .kill_on_drop(true)
                .output()
                .await
                .context("capture AWDL discovery events")?;
            ensure!(
                output.status.success(),
                "awdlctl events failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
            Ok(String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(String::from)
                .collect())
        })
    }
}

#[cfg(unix)]
fn wheel_gid() -> Option<u32> {
    let name = c"wheel";
    let grp = unsafe { libc::getgrnam(name.as_ptr()) };
    if grp.is_null() {
        None
    } else {
        Some(unsafe { (*grp).gr_gid })
    }
}

pub async fn run(socket: PathBuf) -> Result<()> {
    run_with_backend(socket, Arc::new(ProdRadioBackend)).await
}

async fn run_with_backend(socket: PathBuf, backend: Arc<dyn RadioBackend>) -> Result<()> {
    if let Some(parent) = socket.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let _ = tokio::fs::remove_file(&socket).await;
    let listener =
        UnixListener::bind(&socket).with_context(|| format!("bind {}", socket.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o660))?;
        if unsafe { libc::geteuid() } == 0 {
            if let Some(gid) = wheel_gid() {
                let meta = std::fs::metadata(&socket)?;
                if meta.gid() != gid {
                    let raw = std::ffi::CString::new(socket.as_os_str().as_encoded_bytes())?;
                    let rc = unsafe { libc::chown(raw.as_ptr(), 0, gid) };
                    if rc != 0 {
                        anyhow::bail!(
                            "set radio socket group: {}",
                            std::io::Error::last_os_error()
                        );
                    }
                }
            }
        }
    }

    let lease: Arc<Mutex<Option<Box<dyn RadioChild>>>> = Arc::new(Mutex::new(None));
    tracing::info!(path = %socket.display(), "ac-dc radio daemon listening");
    loop {
        let (stream, _) = listener.accept().await?;
        let backend = backend.clone();
        let lease = lease.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_client(stream, backend, lease).await {
                tracing::debug!(error = %e, "radio client disconnected");
            }
        });
    }
}

async fn handle_client(
    stream: UnixStream,
    backend: Arc<dyn RadioBackend>,
    lease: Arc<Mutex<Option<Box<dyn RadioChild>>>>,
) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut owns_lease = false;
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            break;
        }
        let req: RadioRequest =
            serde_json::from_str(line.trim()).context("invalid radio daemon request")?;
        let reply = match req.op.as_str() {
            "start" => {
                let seconds = req.seconds.unwrap_or(600).clamp(1, 3600);
                let mut guard = lease.lock().await;
                if guard.is_some() {
                    RadioReply {
                        ok: false,
                        message: "radio lease already active".into(),
                        lines: None,
                    }
                } else {
                    match backend.spawn_discoverable(seconds).await {
                        Ok(child) => {
                            *guard = Some(child);
                            owns_lease = true;
                            RadioReply {
                                ok: true,
                                message: "radio ready".into(),
                                lines: None,
                            }
                        }
                        Err(e) => RadioReply {
                            ok: false,
                            message: format!("{e:#}"),
                            lines: None,
                        },
                    }
                }
            }
            "renew" => {
                let mut guard = lease.lock().await;
                match guard.as_mut() {
                    Some(child) => match child.renew().await {
                        Ok(()) => RadioReply {
                            ok: true,
                            message: "radio renewed".into(),
                            lines: None,
                        },
                        Err(e) => RadioReply {
                            ok: false,
                            message: format!("{e:#}"),
                            lines: None,
                        },
                    },
                    None => RadioReply {
                        ok: false,
                        message: "no active radio lease".into(),
                        lines: None,
                    },
                }
            }
            "stop" => {
                if owns_lease {
                    if let Some(mut child) = lease.lock().await.take() {
                        child.stop().await;
                    }
                    owns_lease = false;
                }
                RadioReply {
                    ok: true,
                    message: "radio stopped".into(),
                    lines: None,
                }
            }
            "events" => {
                let seconds = req.seconds.unwrap_or(8).clamp(1, 30);
                match backend.run_events(seconds).await {
                    Ok(lines) => RadioReply {
                        ok: true,
                        message: format!("{} event(s)", lines.len()),
                        lines: Some(lines),
                    },
                    Err(e) => RadioReply {
                        ok: false,
                        message: format!("{e:#}"),
                        lines: None,
                    },
                }
            }
            other => RadioReply {
                ok: false,
                message: format!("unknown radio op: {other}"),
                lines: None,
            },
        };
        write
            .write_all(serde_json::to_string(&reply)?.as_bytes())
            .await?;
        write.write_all(b"\n").await?;
    }

    if owns_lease {
        if let Some(mut child) = lease.lock().await.take() {
            child.stop().await;
        }
    }
    Ok(())
}

pub struct RemoteRadioClient {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    write: tokio::net::unix::OwnedWriteHalf,
}

impl RemoteRadioClient {
    pub async fn connect_and_start(socket: &Path, seconds: u64) -> Result<Self> {
        let stream = UnixStream::connect(socket).await.with_context(|| {
            format!(
                "connect ac-dc radio daemon at {} (is ac-dc-daemon.service running?)",
                socket.display()
            )
        })?;
        let (read, write) = stream.into_split();
        let mut client = Self {
            reader: BufReader::new(read),
            write,
        };
        let reply = client
            .send(&RadioRequest {
                op: "start".into(),
                seconds: Some(seconds),
            })
            .await?;
        ensure!(reply.ok, "{}", reply.message);
        Ok(client)
    }

    async fn send(&mut self, req: &RadioRequest) -> Result<RadioReply> {
        self.write
            .write_all(serde_json::to_string(req)?.as_bytes())
            .await?;
        self.write.write_all(b"\n").await?;
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).await?;
        ensure!(n > 0, "radio daemon closed connection");
        serde_json::from_str(&line).context("parse radio daemon reply")
    }

    pub async fn renew(&mut self) -> Result<()> {
        let reply = self
            .send(&RadioRequest {
                op: "renew".into(),
                seconds: None,
            })
            .await?;
        ensure!(reply.ok, "{}", reply.message);
        Ok(())
    }

    pub async fn stop(&mut self) {
        let _ = self
            .send(&RadioRequest {
                op: "stop".into(),
                seconds: None,
            })
            .await;
    }
}

pub async fn fetch_events(socket: &Path, seconds: u64) -> Result<Vec<String>> {
    let stream = UnixStream::connect(socket).await.with_context(|| {
        format!(
            "connect ac-dc radio daemon at {} (is ac-dc-daemon.service running?)",
            socket.display()
        )
    })?;
    let (read, mut write) = stream.into_split();
    let req = RadioRequest {
        op: "events".into(),
        seconds: Some(seconds),
    };
    write
        .write_all(serde_json::to_string(&req)?.as_bytes())
        .await?;
    write.write_all(b"\n").await?;
    let mut line = String::new();
    BufReader::new(read).read_line(&mut line).await?;
    let reply: RadioReply = serde_json::from_str(&line).context("parse radio events reply")?;
    ensure!(reply.ok, "{}", reply.message);
    Ok(reply.lines.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct MockChild {
        stopped: Arc<AtomicBool>,
        renewed: Arc<AtomicUsize>,
    }

    impl RadioChild for MockChild {
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

    struct MockBackend {
        stopped: Arc<AtomicBool>,
        renewed: Arc<AtomicUsize>,
    }

    impl RadioBackend for MockBackend {
        fn spawn_discoverable(&self, _seconds: u64) -> BoxFuture<'_, Result<Box<dyn RadioChild>>> {
            let stopped = self.stopped.clone();
            let renewed = self.renewed.clone();
            Box::pin(
                async move { Ok(Box::new(MockChild { stopped, renewed }) as Box<dyn RadioChild>) },
            )
        }
        fn run_events(&self, _seconds: u64) -> BoxFuture<'_, Result<Vec<String>>> {
            Box::pin(async { Ok(vec!["{\"host\":\"fe80::1\"}".into()]) })
        }
    }

    #[tokio::test]
    async fn lease_renews_runs_concurrent_events_and_stops_on_disconnect() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("radio.sock");
        let stopped = Arc::new(AtomicBool::new(false));
        let renewed = Arc::new(AtomicUsize::new(0));
        let backend = Arc::new(MockBackend {
            stopped: stopped.clone(),
            renewed: renewed.clone(),
        });

        let srv_sock = socket.clone();
        let srv = tokio::spawn(async move {
            let _ = run_with_backend(srv_sock, backend).await;
        });
        for _ in 0..50 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let mut client = RemoteRadioClient::connect_and_start(&socket, 60)
            .await
            .unwrap();
        client.renew().await.unwrap();
        assert_eq!(renewed.load(Ordering::SeqCst), 1);

        // Concurrent `events` succeeds while `start` lease is held.
        let ev = fetch_events(&socket, 2).await.unwrap();
        assert_eq!(ev.len(), 1);

        // Dropping client (EOF) triggers automatic radio teardown.
        drop(client);
        for _ in 0..50 {
            if stopped.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(stopped.load(Ordering::SeqCst));
        srv.abort();
    }
}
