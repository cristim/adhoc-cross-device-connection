//! Typed IPC messages and streaming state snapshots shared by the daemon, `ctl`, GPUI, and QML.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiveMode {
    #[default]
    Timed,
    Once,
    Always,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferDirection {
    Receive,
    Send,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct RadioState {
    pub ready: bool,
    pub visible: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct WindowState {
    pub mode: ReceiveMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct PendingTransfer {
    pub sender: String,
    pub items: Vec<String>,
    /// The sender's full item count, which can exceed `items` once it has
    /// been truncated to keep the snapshot under the panel's display limit.
    #[serde(default)]
    pub total_items: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct ActiveTransfer {
    pub direction: TransferDirection,
    pub target: String,
    pub sent_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct PeerSummary {
    pub name: String,
    pub instance: String,
    pub address: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct ClipboardStatus {
    pub available: bool,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_item_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct DaemonSnapshot {
    pub phase: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowState>,
    pub radio: RadioState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending: Option<PendingTransfer>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_transfer: Option<ActiveTransfer>,
    #[serde(default)]
    pub peers: Vec<PeerSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clipboard: Option<ClipboardStatus>,
    #[serde(default)]
    pub contacts_only: bool,
}

impl Default for DaemonSnapshot {
    fn default() -> Self {
        Self {
            phase: "idle".into(),
            window: None,
            radio: RadioState::default(),
            pending: None,
            active_transfer: None,
            peers: Vec::new(),
            last_error: None,
            clipboard: None,
            contacts_only: false,
        }
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
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
    #[serde(default)]
    pub seconds: Option<u64>,
    #[serde(default)]
    pub ble_wake: Option<bool>,
    #[serde(default)]
    pub mode: Option<ReceiveMode>,
    #[serde(default)]
    pub once: Option<bool>,
    #[serde(default)]
    pub contacts_only: Option<bool>,
    #[serde(default)]
    pub max_disk_percent: Option<u8>,
    #[serde(default)]
    pub copy_to_clipboard: Option<bool>,
    #[serde(default)]
    pub clipboard: Option<ClipboardStatus>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Reply {
    pub ok: bool,
    pub state: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<DaemonSnapshot>,
}
