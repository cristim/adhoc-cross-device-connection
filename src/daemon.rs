//! Privileged, single-owner Airdrop-compatible protocol service for the Omarchy widget and CLI.
use crate::airdrop;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::Mutex,
};

/// Wait for the asynchronous AWDL setup before the protocol checks awdl0.
async fn start_radio(seconds: u64) -> Result<tokio::process::Child> {
    let mut child = tokio::process::Command::new("/usr/bin/awdlctl")
        .args(["discoverable", "--seconds", &seconds.to_string()])
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("start AWDL radio")?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait()? {
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
            return Ok(child);
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("AWDL radio did not become ready within 10 seconds");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Request {
    pub op: String,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub directory: Option<PathBuf>,
    #[serde(default)]
    pub files: Vec<PathBuf>,
    #[serde(default)]
    pub links: Vec<String>,
}

#[derive(Debug, Serialize)]
struct Reply {
    ok: bool,
    state: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<serde_json::Value>,
}

struct State {
    phase: String,
    task: Option<tokio::task::JoinHandle<()>>,
}

const SOCKET: &str = "/run/ac-dc/control.sock";
const DIRECTORY: &str = "/home/cristi/Downloads/Adhoc";
const IDENTITY: &str = "/home/cristi/.local/share/ac-dc/airdrop";

pub async fn run(socket: PathBuf) -> Result<()> {
    if let Some(parent) = socket.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let _ = tokio::fs::remove_file(&socket).await;
    let listener =
        UnixListener::bind(&socket).with_context(|| format!("bind {}", socket.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o660))?;
        let raw = std::ffi::CString::new(socket.as_os_str().as_encoded_bytes())?;
        // The desktop user is granted access through the local administrators
        // group used by this installation; the daemon itself remains root.
        let rc = unsafe { libc::chown(raw.as_ptr(), 0, 998) };
        if rc != 0 {
            anyhow::bail!(
                "set daemon socket group: {}",
                std::io::Error::last_os_error()
            );
        }
    }
    let state = Arc::new(Mutex::new(State {
        phase: "idle".into(),
        task: None,
    }));
    tracing::info!(path=%socket.display(), "ac-dc daemon listening");
    loop {
        let (stream, _) = listener.accept().await?;
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
    let mut line = String::new();
    BufReader::new(read).read_line(&mut line).await?;
    let request: Request = serde_json::from_str(line.trim()).context("invalid daemon request")?;
    let reply = match request.op.as_str() {
        "status" => {
            let s = state.lock().await;
            Reply {
                ok: true,
                state: s.phase.clone(),
                message: s.phase.clone(),
                data: None,
            }
        }
        "receive" => start_receive(state.clone(), request).await,
        "stop" => stop(state.clone()).await,
        "send" => start_send(state.clone(), request).await,
        "peers" => discover(state.clone()).await,
        _ => Reply {
            ok: false,
            state: "idle".into(),
            message: "unknown operation".into(),
            data: None,
        },
    };
    write
        .write_all(serde_json::to_string(&reply)?.as_bytes())
        .await?;
    write.write_all(b"\n").await?;
    Ok(())
}

async fn start_receive(state: Arc<Mutex<State>>, request: Request) -> Reply {
    let directory = request
        .directory
        .unwrap_or_else(|| PathBuf::from(DIRECTORY));
    let name = request.name.unwrap_or_else(|| "Omarchy".into());
    let mut s = state.lock().await;
    if s.task.as_ref().is_some_and(|t| !t.is_finished()) {
        return Reply {
            ok: true,
            state: s.phase.clone(),
            message: "already receiving".into(),
            data: None,
        };
    }
    s.phase = "starting".into();
    let task_state = state.clone();
    s.task = Some(tokio::spawn(async move {
        let result = async {
            let mut radio = start_radio(600).await?;
            {
                task_state.lock().await.phase = "receiving".into();
            }
            let result = airdrop::run(airdrop::Config {
                radio_managed: true,
                ble_wake: true,
                approval: None,
                iface: "awdl0".into(),
                directory,
                identity: PathBuf::from(IDENTITY),
                name,
                port: 8771,
                seconds: 600,
                once: false,
                notify: true,
            })
            .await;
            let _ = radio.kill().await;
            result
        }
        .await;
        let mut s = task_state.lock().await;
        s.phase = if result.is_ok() { "idle" } else { "error" }.into();
        s.task = None;
        if let Err(e) = result {
            tracing::error!(error=%e, "Airdrop-compatible receiver stopped");
        }
    }));
    Reply {
        ok: true,
        state: s.phase.clone(),
        message: "receive window starting".into(),
        data: None,
    }
}

async fn stop(state: Arc<Mutex<State>>) -> Reply {
    let mut s = state.lock().await;
    if let Some(task) = s.task.take() {
        task.abort();
    }
    s.phase = "idle".into();
    Reply {
        ok: true,
        state: "idle".into(),
        message: "stopped".into(),
        data: None,
    }
}

async fn discover(state: Arc<Mutex<State>>) -> Reply {
    let mut radio = match start_radio(30).await {
        Ok(child) => child,
        Err(e) => {
            return Reply {
                ok: false,
                state: state.lock().await.phase.clone(),
                message: format!("start radio: {e:#}"),
                data: None,
            }
        }
    };
    let result = tokio::time::timeout(
        Duration::from_secs(20),
        airdrop::peers::browse("awdl0", &PathBuf::from(IDENTITY), 8),
    )
    .await;
    let _ = radio.kill().await;
    match result {
        Ok(Ok(peers)) => Reply {
            ok: true,
            state: state.lock().await.phase.clone(),
            message: format!("{} recipient(s)", peers.len()),
            data: Some(serde_json::to_value(peers).unwrap_or_default()),
        },
        Ok(Err(e)) => Reply {
            ok: false,
            state: state.lock().await.phase.clone(),
            message: e.to_string(),
            data: None,
        },
        Err(e) => Reply {
            ok: false,
            state: state.lock().await.phase.clone(),
            message: e.to_string(),
            data: None,
        },
    }
}

async fn start_send(state: Arc<Mutex<State>>, request: Request) -> Reply {
    let host = match request.host {
        Some(v) => v,
        None => {
            return Reply {
                ok: false,
                state: "idle".into(),
                message: "send requires host".into(),
                data: None,
            }
        }
    };
    let port = request.port.unwrap_or(8770);
    let name = request.name.unwrap_or_else(|| "Linux".into());
    let files = request.files;
    let links = request.links;
    let task_state = state.clone();
    {
        let mut s = state.lock().await;
        if s.task.as_ref().is_some_and(|t| !t.is_finished()) {
            return Reply {
                ok: false,
                state: s.phase.clone(),
                message: "another operation is active".into(),
                data: None,
            };
        }
        s.phase = "sending".into();
        s.task = Some(tokio::spawn(async move {
            let result = async {
                let mut radio = start_radio(600).await?;
                let address = crate::discover::socket_target(&host, port).await?;
                let client = airdrop::send::Client::new(address, &PathBuf::from(IDENTITY))?;
                let receiver = client.discover().await?;
                client.transfer(&name, files, links).await?;
                let _ = radio.kill().await;
                Ok::<String, anyhow::Error>(receiver)
            }
            .await;
            let mut s = task_state.lock().await;
            s.phase = if result.is_ok() { "idle" } else { "error" }.into();
            s.task = None;
            if let Err(e) = result {
                tracing::error!(error=%e, "Airdrop-compatible send failed");
            }
        }));
    }
    Reply {
        ok: true,
        state: "sending".into(),
        message: "send started".into(),
        data: None,
    }
}

pub async fn ctl(socket: PathBuf, request: Request) -> Result<()> {
    let mut stream = UnixStream::connect(socket)
        .await
        .context("connect ac-dc daemon")?;
    stream
        .write_all(serde_json::to_string(&request)?.as_bytes())
        .await?;
    stream.write_all(b"\n").await?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).await?;
    print!("{}", line);
    Ok(())
}

pub fn default_socket() -> PathBuf {
    PathBuf::from(SOCKET)
}
