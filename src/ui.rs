//! Native GPUI desktop controls. Protocol and radio work runs on a private Tokio runtime.
use crate::airdrop::peers::Peer;
use crate::daemon::{self, DaemonSnapshot, ReceiveMode};
use crate::ui_widgets as w;
use anyhow::{Context as _, Result};
use gpui::{
    actions, div, prelude::*, px, rgb, size, white, App, Bounds, Context, Entity, FocusHandle,
    Focusable, KeyBinding, MouseButton, MouseUpEvent, PathPromptOptions, TitlebarOptions, Window,
    WindowBounds, WindowOptions,
};
use gpui_platform::application;
use std::{
    path::PathBuf,
    rc::Rc,
    sync::{atomic::AtomicBool, atomic::Ordering, mpsc, Arc},
    time::Duration,
};

actions!(ac_dc, [Quit]);

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Preferences {
    name: String,
    directory: PathBuf,
    #[serde(default = "default_ble_wake")]
    ble_wake: bool,
}
fn default_ble_wake() -> bool {
    true
}
impl Default for Preferences {
    fn default() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        Self {
            name: "Linux".into(),
            directory: home.join("Downloads/Adhoc"),
            ble_wake: true,
        }
    }
}
enum Action {
    Start(Preferences),
    Stop,
    Browse,
    Send(Peer, Preferences, Vec<PathBuf>, Vec<String>),
    Quit,
    Cancel,
    Approve(usize, u64),
    Decline(usize, u64),
}
enum Event {
    Status(String),
    Radio(String),
    Peers(Vec<Peer>),
    Incoming(String, Vec<String>, usize, u64),
    IncomingCleared(usize),
    IncomingAllCleared,
}
fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn format_radio_banner(snap: &DaemonSnapshot) -> String {
    match snap.phase.as_str() {
        "starting" => "Radio: starting AWDL…".into(),
        "receiving" => {
            let win = match snap.window.as_ref() {
                Some(w) => match w.mode {
                    ReceiveMode::Once => "Once".to_string(),
                    ReceiveMode::Always => "Always".to_string(),
                    ReceiveMode::Timed => match w.remaining_seconds {
                        Some(rem) => format!("{}:{:02} remaining", rem / 60, rem % 60),
                        None => "Timed".to_string(),
                    },
                },
                None => "Active".to_string(),
            };
            let vis = match snap.radio.visible {
                Some(true) => "visible",
                Some(false) => "not advertising",
                None => "radio unknown",
            };
            format!("Radio: {win} · {vis}")
        }
        "sending" => {
            if let Some(t) = snap.active_transfer.as_ref() {
                match t.total_bytes {
                    Some(total) if total > 0 => format!(
                        "Sending to {} ({}%)",
                        t.target,
                        (t.sent_bytes.saturating_mul(100) / total).min(100)
                    ),
                    _ => format!("Sending to {}…", t.target),
                }
            } else {
                "Sending…".into()
            }
        }
        "error" => snap
            .last_error
            .as_deref()
            .map(|e| format!("Error: {e}"))
            .unwrap_or_else(|| "Radio error".into()),
        _ => "Radio window closed".into(),
    }
}

fn apply_snapshot(
    snap: &DaemonSnapshot,
    last_pending: &mut Option<(String, Vec<String>, usize, u64)>,
    last_active_send: &mut bool,
    events: &mpsc::Sender<Event>,
    suppress_finished: &AtomicBool,
) {
    let _ = events.send(Event::Radio(format_radio_banner(snap)));
    let pending = snap
        .pending
        .as_ref()
        .map(|p| (p.sender.clone(), p.items.clone(), p.total_items, p.id));
    if pending != *last_pending {
        match &pending {
            Some((sender, items, total_items, id)) => {
                let _ = events.send(Event::Incoming(
                    sender.clone(),
                    items.clone(),
                    *total_items,
                    *id,
                ));
            }
            None => {
                let _ = events.send(Event::IncomingAllCleared);
            }
        }
        *last_pending = pending;
    }
    let active_now = snap.active_transfer.is_some();
    if let Some(err) = snap.last_error.as_ref() {
        let _ = events.send(Event::Status(format!("Error: {err}")));
    } else if let Some(t) = snap.active_transfer.as_ref() {
        let msg = match t.total_bytes {
            Some(total) if total > 0 => format!(
                "Sending to {}: {:.1} / {:.1} MiB ({}%)",
                t.target,
                t.sent_bytes as f64 / 1_048_576.0,
                total as f64 / 1_048_576.0,
                (t.sent_bytes.saturating_mul(100) / total).min(100)
            ),
            _ => format!("Preparing transfer for {}…", t.target),
        };
        let _ = events.send(Event::Status(msg));
    } else if *last_active_send && !suppress_finished.swap(false, Ordering::Relaxed) {
        // The send just ended without an error; otherwise the last "Sending…"
        // status would stay on screen forever. A user-initiated cancel
        // already announced its own status and sets the flag we just read.
        let _ = events.send(Event::Status("Send finished".into()));
    }
    *last_active_send = active_now;
}

async fn subscribe_loop(
    socket: PathBuf,
    events: mpsc::Sender<Event>,
    suppress_finished: Arc<AtomicBool>,
) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let mut last_pending: Option<(String, Vec<String>, usize, u64)> = None;
    let mut last_active_send = false;
    loop {
        if let Ok(mut stream) = tokio::net::UnixStream::connect(&socket).await {
            if stream.write_all(b"{\"op\":\"subscribe\"}\n").await.is_ok() {
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                while let Ok(n) = reader.read_line(&mut line).await {
                    if n == 0 {
                        break;
                    }
                    if let Ok(snap) = serde_json::from_str::<DaemonSnapshot>(line.trim()) {
                        apply_snapshot(
                            &snap,
                            &mut last_pending,
                            &mut last_active_send,
                            &events,
                            &suppress_finished,
                        );
                    }
                    line.clear();
                }
            }
        }
        let _ = events.send(Event::Radio("Daemon unavailable".into()));
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Recipient discovery is brokered by the root daemon. The full GUI must
/// not invoke pkexec merely to browse nearby devices.
async fn daemon_peers() -> Result<Vec<Peer>> {
    let output = tokio::process::Command::new("/usr/bin/ac-dc")
        .args(["ctl", "peers"])
        .kill_on_drop(true)
        .output()
        .await?;
    anyhow::ensure!(
        output.status.success(),
        "ac-dc peers failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let reply: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    anyhow::ensure!(
        reply["ok"] == true,
        "{}",
        reply["message"]
            .as_str()
            .unwrap_or("recipient discovery failed")
    );
    Ok(serde_json::from_value(reply["data"].clone())?)
}
async fn engine(
    mut actions: tokio::sync::mpsc::UnboundedReceiver<Action>,
    events: std::sync::mpsc::Sender<Event>,
    socket: PathBuf,
) {
    let mut browsing: Option<tokio::task::JoinHandle<()>> = None;
    let suppress_finished = Arc::new(AtomicBool::new(false));
    let sub_task = tokio::spawn(subscribe_loop(
        socket.clone(),
        events.clone(),
        suppress_finished.clone(),
    ));
    loop {
        let Some(action) = actions.recv().await else {
            break;
        };
        match action {
            Action::Start(p) => {
                // Visibility is the daemon's job; it owns the radio window and
                // the receiver, so no pkexec prompt is needed.
                let result = daemon::ctl(
                    socket.clone(),
                    daemon::Request {
                        op: "receive".into(),
                        name: Some(p.name),
                        directory: Some(p.directory),
                        seconds: Some(600),
                        ble_wake: Some(p.ble_wake),
                        ..Default::default()
                    },
                )
                .await;
                let _ = events.send(Event::Status(match result {
                    Ok(reply) if reply.ok => {
                        if reply.message == "already receiving" {
                            "Receive window is already active".into()
                        } else {
                            "Receiving for 10 minutes. Incoming transfers require your approval. Open Airdrop-compatible on your iPhone or Mac.".into()
                        }
                    }
                    Ok(reply) => format!("Could not start receiving: {}", reply.message),
                    Err(e) => format!("Could not reach the ac-dc daemon: {e:#}"),
                }));
            }
            Action::Stop => {
                if let Some(h) = browsing.take() {
                    h.abort();
                    let _ = h.await;
                }
                match daemon::ctl(socket.clone(), daemon::request("stop")).await {
                    Ok(reply) => {
                        let _ = events.send(Event::Status(match reply.message.as_str() {
                            "stopped" => "Airdrop-compatible stopped".into(),
                            message => message.into(),
                        }));
                        let _ = events.send(Event::IncomingAllCleared);
                    }
                    Err(e) => {
                        let _ = events.send(Event::Status(format!(
                            "Could not stop the ac-dc daemon: {e:#}"
                        )));
                    }
                }
            }
            Action::Quit => {
                if let Some(h) = browsing.take() {
                    h.abort();
                    let _ = h.await;
                }
                break;
            }
            Action::Approve(id, pending_id) | Action::Decline(id, pending_id) => {
                let accepted = matches!(action, Action::Approve(..));
                let mut req = daemon::request(if accepted { "approve" } else { "reject" });
                req.id = Some(pending_id);
                match daemon::ctl(socket.clone(), req).await {
                    Ok(reply) => {
                        let _ = events.send(Event::Status(reply.message));
                        let _ = events.send(Event::IncomingCleared(id));
                    }
                    Err(e) => {
                        // The daemon never saw the decision, so keep showing the
                        // card: clearing it here would hide a request that is
                        // still pending approval.
                        let _ = events.send(Event::Status(format!(
                            "Could not reach the ac-dc daemon: {e:#}"
                        )));
                    }
                }
            }
            Action::Browse => {
                let _ = events.send(Event::Status(
                    "Looking for nearby recipients. Set the Apple device to Everyone for 10 Minutes.".into(),
                ));
                if browsing.as_ref().is_some_and(|h| !h.is_finished()) {
                    continue;
                }
                let ev = events.clone();
                browsing = Some(tokio::spawn(async move {
                    match daemon_peers().await {
                        Ok(peers) => {
                            let _ = ev
                                .send(Event::Status(format!("Found {} recipient(s)", peers.len())));
                            let _ = ev.send(Event::Peers(peers));
                        }
                        Err(e) => {
                            let _ = ev.send(Event::Status(format!("Discovery: {e:#}")));
                        }
                    }
                }));
            }
            Action::Cancel => match daemon::ctl(socket.clone(), daemon::request("cancel")).await {
                Ok(reply) => {
                    // Suppress the heuristic "Send finished" the next snapshot
                    // would otherwise announce once active_transfer clears.
                    suppress_finished.store(true, Ordering::Relaxed);
                    let _ = events.send(Event::Status(reply.message));
                }
                Err(e) => {
                    let _ = events.send(Event::Status(format!("Could not cancel transfer: {e:#}")));
                }
            },
            Action::Send(peer, p, files, links) => {
                let host = match peer.address {
                    std::net::SocketAddr::V6(v6) if v6.scope_id() != 0 => {
                        format!("{}%{}", v6.ip(), v6.scope_id())
                    }
                    other => other.ip().to_string(),
                };
                let result = daemon::ctl(
                    socket.clone(),
                    daemon::Request {
                        op: "send".into(),
                        host: Some(host),
                        port: Some(peer.address.port()),
                        name: Some(p.name),
                        files,
                        links,
                        ..Default::default()
                    },
                )
                .await;
                let _ = events.send(Event::Status(match result {
                    Ok(reply) if reply.ok => format!("Sending to {}…", peer.name),
                    Ok(reply) => format!("Transfer failed: {}", reply.message),
                    Err(e) => format!("Could not reach the ac-dc daemon: {e:#}"),
                }));
            }
        }
    }
    sub_task.abort();
    if let Some(h) = browsing {
        h.abort();
    }
}

struct IncomingCard {
    id: usize,
    /// The daemon's pending-transfer id this card is showing, so approve/
    /// reject can be bound to the transfer actually displayed.
    pending_id: u64,
    label: String,
}

struct AcDcApp {
    name: Entity<w::TextField>,
    directory: Entity<w::TextField>,
    link: Entity<w::TextField>,
    ble_wake_checked: bool,
    peers: Vec<Peer>,
    peers_display: Vec<String>,
    peer_selected: usize,
    peer_drop_open: bool,
    files: Vec<PathBuf>,
    file_label: String,
    status: String,
    radio_text: String,
    incoming: Vec<IncomingCard>,
    incoming_counter: usize,
    actions_tx: tokio::sync::mpsc::UnboundedSender<Action>,
    prefs_path: PathBuf,
    focus_handle: FocusHandle,
}

impl AcDcApp {
    fn new(
        actions_tx: tokio::sync::mpsc::UnboundedSender<Action>,
        events_rx: mpsc::Receiver<Event>,
        prefs_path: PathBuf,
        prefs: &Preferences,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| w::TextField::new(&prefs.name, "Your device name", cx));
        let directory = cx
            .new(|cx| w::TextField::new(&prefs.directory.to_string_lossy(), "Receive folder", cx));
        let link = cx.new(|cx| w::TextField::new("", "Or paste an https:// link", cx));
        let view = Self {
            name,
            directory,
            link,
            ble_wake_checked: prefs.ble_wake,
            peers: Vec::new(),
            peers_display: Vec::new(),
            peer_selected: 0,
            peer_drop_open: false,
            files: Vec::new(),
            file_label: "No files selected".into(),
            status: "Ready. Everyone mode only; Apple-device compatibility is being tested.".into(),
            radio_text: "Radio window closed".into(),
            incoming: Vec::new(),
            incoming_counter: 0,
            actions_tx,
            prefs_path,
            focus_handle: cx.focus_handle(),
        };

        // Event pump: forwards engine events into the view and requests a redraw. Polling
        // is cheap (a non-blocking channel read) so a steady cadence only matters while idle.
        cx.spawn(async move |weak, cx| {
            let rx = events_rx;
            loop {
                let mut changed = false;
                while let Ok(event) = rx.try_recv() {
                    changed = true;
                    cx.update(|cx| {
                        if let Some(entity) = weak.upgrade() {
                            entity.update(cx, |this, cx| {
                                match event {
                                    Event::Status(text) => this.status = text,
                                    Event::Radio(text) => this.radio_text = text,
                                    Event::Peers(list) => {
                                        this.peers = list;
                                        this.peers_display = this
                                            .peers
                                            .iter()
                                            .map(|p| format!("{} — {}", p.name, p.address))
                                            .collect();
                                        this.peer_selected = 0;
                                        this.peer_drop_open = false;
                                    }
                                    Event::Incoming(sender, items, total_items, pending_id) => {
                                        this.incoming_counter += 1;
                                        let more = total_items.saturating_sub(items.len());
                                        let suffix = if more > 0 {
                                            format!("\n… and {more} more")
                                        } else {
                                            String::new()
                                        };
                                        this.incoming.push(IncomingCard {
                                            id: this.incoming_counter,
                                            pending_id,
                                            label: format!(
                                                "{sender} wants to share:\n{}{suffix}",
                                                items.join("\n")
                                            ),
                                        });
                                    }
                                    Event::IncomingCleared(id) => {
                                        this.incoming.retain(|card| card.id != id);
                                    }
                                    Event::IncomingAllCleared => this.incoming.clear(),
                                }
                                cx.notify();
                            });
                        }
                    });
                }
                if !changed {
                    cx.background_executor()
                        .timer(Duration::from_millis(100))
                        .await;
                }
            }
        })
        .detach();

        view
    }

    fn read_prefs(&self, cx: &App) -> Result<Preferences> {
        let p = Preferences {
            name: self.name.read(cx).text.to_string(),
            directory: PathBuf::from(self.directory.read(cx).text.to_string()),
            ble_wake: self.ble_wake_checked,
        };
        anyhow::ensure!(
            !p.name.is_empty() && p.name.len() <= 63,
            "Use a device name between 1 and 63 bytes"
        );
        anyhow::ensure!(
            p.directory.is_absolute(),
            "Use an absolute receive folder path"
        );
        Ok(p)
    }

    fn save_prefs(&self, cx: &App) -> Result<()> {
        let prefs = self.read_prefs(cx)?;
        let dir = self.prefs_path.parent().context("preferences directory")?;
        std::fs::create_dir_all(dir)?;
        std::fs::write(&self.prefs_path, serde_json::to_vec_pretty(&prefs)?)?;
        Ok(())
    }

    fn on_receive(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        match self.read_prefs(cx) {
            Ok(prefs) => {
                let _ = self.save_prefs(cx);
                let _ = self.actions_tx.send(Action::Start(prefs));
            }
            Err(e) => {
                self.status = e.to_string();
                cx.notify();
            }
        }
    }

    fn on_stop(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        let _ = self.actions_tx.send(Action::Stop);
    }

    fn on_find(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        let _ = self.actions_tx.send(Action::Browse);
    }

    fn on_choose_files(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Choose files to send".into()),
        });
        cx.spawn(async move |weak, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                cx.update(|cx| {
                    if let Some(view) = weak.upgrade() {
                        view.update(cx, |this, cx| {
                            let count = paths.len();
                            this.files = paths;
                            this.file_label = format!("{count} file(s) selected");
                            cx.notify();
                        });
                    }
                });
            }
        })
        .detach();
    }

    fn on_choose_folder(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose folder to send".into()),
        });
        cx.spawn(async move |weak, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                cx.update(|cx| {
                    if let Some(view) = weak.upgrade() {
                        view.update(cx, |this, cx| {
                            if let Some(path) = paths.into_iter().next() {
                                this.file_label = format!("Folder: {}", path.display());
                                this.files = vec![path];
                            }
                            cx.notify();
                        });
                    }
                });
            }
        })
        .detach();
    }

    fn on_send(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let peer = self.peers.get(self.peer_selected).cloned();
        let Some(peer) = peer else {
            self.status = "Choose a recipient first".into();
            cx.notify();
            return;
        };
        let link_text = self.link.read(cx).text.clone();
        let links = if link_text.trim().is_empty() {
            vec![]
        } else {
            vec![link_text.trim().into()]
        };
        let files = if links.is_empty() {
            self.files.clone()
        } else {
            vec![]
        };
        let prefs = match self.read_prefs(cx) {
            Ok(p) => p,
            Err(e) => {
                self.status = e.to_string();
                cx.notify();
                return;
            }
        };
        let _ = self.save_prefs(cx);
        let _ = self
            .actions_tx
            .send(Action::Send(peer, prefs, files, links));
    }

    fn on_cancel(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        let _ = self.actions_tx.send(Action::Cancel);
    }

    fn on_toggle_ble(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.ble_wake_checked = !self.ble_wake_checked;
        let _ = self.save_prefs(cx);
        cx.notify();
    }
}

impl Focusable for AcDcApp {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for AcDcApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let weak = cx.entity().downgrade();

        let on_toggle_peer = {
            let weak = weak.clone();
            Rc::new(move |cx: &mut App| {
                if let Some(view) = weak.upgrade() {
                    view.update(cx, |this, cx| {
                        this.peer_drop_open = !this.peer_drop_open;
                        cx.notify();
                    });
                }
            })
        };
        let on_select_peer = {
            let weak = weak.clone();
            Rc::new(move |idx: usize, cx: &mut App| {
                if let Some(view) = weak.upgrade() {
                    view.update(cx, |this, cx| {
                        this.peer_selected = idx;
                        this.peer_drop_open = false;
                        cx.notify();
                    });
                }
            })
        };
        let on_accept = {
            let weak = weak.clone();
            let actions = self.actions_tx.clone();
            Rc::new(move |id: usize, cx: &mut App| {
                if let Some(view) = weak.upgrade() {
                    view.update(cx, |this, cx| {
                        if let Some(card) = this.incoming.iter().find(|c| c.id == id) {
                            let _ = actions.send(Action::Approve(id, card.pending_id));
                        }
                        this.incoming.retain(|card| card.id != id);
                        cx.notify();
                    });
                }
            })
        };
        let on_decline = {
            let weak = weak.clone();
            let actions = self.actions_tx.clone();
            Rc::new(move |id: usize, cx: &mut App| {
                if let Some(view) = weak.upgrade() {
                    view.update(cx, |this, cx| {
                        if let Some(card) = this.incoming.iter().find(|c| c.id == id) {
                            let _ = actions.send(Action::Decline(id, card.pending_id));
                        }
                        this.incoming.retain(|card| card.id != id);
                        cx.notify();
                    });
                }
            })
        };

        let file_label = self.file_label.clone();
        let radio_text = self.radio_text.clone();
        let status = self.status.clone();
        let peer_display = self.peers_display.clone();
        let peer_selected = self.peer_selected;
        let peer_drop_open = self.peer_drop_open;
        let ble_checked = self.ble_wake_checked;
        let cards: Vec<_> = self
            .incoming
            .iter()
            .map(|card| {
                let id = card.id;
                let label = card.label.clone();
                let on_accept = on_accept.clone();
                let on_decline = on_decline.clone();
                div()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(0xBBBBBB))
                    .bg(rgb(0xFFF8E1))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().text_sm().child(label))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                div()
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .bg(rgb(0x007AFF))
                                    .text_color(white())
                                    .text_sm()
                                    .cursor_pointer()
                                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                        on_accept(id, cx)
                                    })
                                    .child("Accept"),
                            )
                            .child(
                                div()
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .bg(rgb(0xCCCCCC))
                                    .text_color(rgb(0x333333))
                                    .text_sm()
                                    .cursor_pointer()
                                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                        on_decline(id, cx)
                                    })
                                    .child("Decline"),
                            ),
                    )
            })
            .collect();

        div().flex().flex_col().w_full().h_full().child(
            div()
                .id("ac-dc-scroll")
                .flex()
                .flex_col()
                .w_full()
                .h_full()
                .overflow_y_scroll()
                .p_6()
                .gap_4()
                .child(div().text_xl().child("Airdrop-compatible"))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(0x666666))
                        .child("Share files and links with nearby Apple devices"),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(0x444444))
                        .child("Device name"),
                )
                .child(self.name.clone())
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(0x444444))
                        .child("Receive folder"),
                )
                .child(self.directory.clone())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .on_mouse_up(MouseButton::Left, cx.listener(Self::on_toggle_ble))
                        .child(
                            div()
                                .w(px(20.))
                                .h(px(20.))
                                .rounded_md()
                                .border_1()
                                .border_color(rgb(0x666666))
                                .when(ble_checked, |d| {
                                    d.bg(rgb(0x007AFF))
                                        .child(div().text_size(px(12.)).child("✓"))
                                }),
                        )
                        .child(
                            div()
                                .text_sm()
                                .child("Use Bluetooth to help nearby devices find me"),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            div()
                                .h(px(34.))
                                .px_4()
                                .rounded_md()
                                .bg(rgb(0x007AFF))
                                .text_color(white())
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .on_mouse_up(MouseButton::Left, cx.listener(Self::on_receive))
                                .child("Receive for 10 minutes"),
                        )
                        .child(
                            div()
                                .h(px(34.))
                                .px_4()
                                .rounded_md()
                                .bg(rgb(0xCCCCCC))
                                .text_color(rgb(0x333333))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .on_mouse_up(MouseButton::Left, cx.listener(Self::on_stop))
                                .child("Stop receiving"),
                        ),
                )
                .child(div().h(px(1.)).w_full().bg(rgb(0xDDDDDD)))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(w::dropdown(
                            &peer_display,
                            peer_selected,
                            peer_drop_open,
                            on_toggle_peer,
                            on_select_peer,
                        ))
                        .child(
                            div()
                                .h(px(34.))
                                .px_4()
                                .rounded_md()
                                .bg(rgb(0xCCCCCC))
                                .text_color(rgb(0x333333))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .on_mouse_up(MouseButton::Left, cx.listener(Self::on_find))
                                .child("Find recipients"),
                        ),
                )
                .child(
                    div()
                        .h(px(34.))
                        .px_4()
                        .rounded_md()
                        .bg(rgb(0xCCCCCC))
                        .text_color(rgb(0x333333))
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .on_mouse_up(MouseButton::Left, cx.listener(Self::on_choose_files))
                        .child("Choose files…"),
                )
                .child(
                    div()
                        .h(px(34.))
                        .px_4()
                        .rounded_md()
                        .bg(rgb(0xCCCCCC))
                        .text_color(rgb(0x333333))
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .on_mouse_up(MouseButton::Left, cx.listener(Self::on_choose_folder))
                        .child("Choose folder…"),
                )
                .child(div().text_sm().text_color(rgb(0x444444)).child(file_label))
                .child(self.link.clone())
                .child(
                    div()
                        .h(px(34.))
                        .px_4()
                        .rounded_md()
                        .bg(rgb(0x007AFF))
                        .text_color(white())
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .on_mouse_up(MouseButton::Left, cx.listener(Self::on_send))
                        .child("Send to selected recipient"),
                )
                .child(
                    div()
                        .h(px(34.))
                        .px_4()
                        .rounded_md()
                        .bg(rgb(0xCCCCCC))
                        .text_color(rgb(0x333333))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .on_mouse_up(MouseButton::Left, cx.listener(Self::on_cancel))
                        .child("Cancel outgoing transfer"),
                )
                .child(div().text_sm().child(radio_text))
                .child(div().text_sm().child(status))
                .children(cards),
        )
    }
}

pub fn run() -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    let prefs_path = home().join(".config/ac-dc/ui.json");
    let prefs: Preferences = std::fs::read(&prefs_path)
        .ok()
        .and_then(|v| serde_json::from_slice(&v).ok())
        .unwrap_or_default();
    let (actions_tx, actions_rx) = tokio::sync::mpsc::unbounded_channel();
    let (events_tx, events_rx) = mpsc::channel();
    let worker = runtime.spawn(engine(actions_rx, events_tx, daemon::default_socket()));

    let shutdown = actions_tx.clone();
    let final_shutdown = shutdown.clone();
    application().run(move |cx: &mut App| {
        w::bind_text_input_keys(cx);
        cx.bind_keys([
            KeyBinding::new("escape", Quit, None),
            KeyBinding::new("secondary-q", Quit, None),
        ]);

        let close_actions = shutdown.clone();
        cx.on_window_closed(move |cx, _window_id| {
            let _ = close_actions.send(Action::Quit);
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let quit_actions = shutdown.clone();
        cx.on_action(move |_: &Quit, cx| {
            let _ = quit_actions.send(Action::Quit);
            cx.quit();
        });

        let bounds = Bounds::centered(None, size(px(640.), px(720.)), cx);
        cx.open_window(
            WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some(
                        format!("ac-dc {} · Airdrop-compatible", env!("CARGO_PKG_VERSION")).into(),
                    ),
                    appears_transparent: false,
                    traffic_light_position: None,
                }),
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            move |window, cx| {
                let actions = shutdown.clone();
                let view = cx.new(|cx| AcDcApp::new(actions, events_rx, prefs_path, &prefs, cx));
                window.focus(&view.focus_handle(cx), cx);
                cx.activate(true);
                view
            },
        )
        .unwrap();
    });

    // The window has closed. Tell the engine to tear down local tasks and join it
    // without stopping an active daemon receive window.
    let _ = final_shutdown.send(Action::Quit);
    runtime.block_on(worker)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::{ActiveTransfer, PendingTransfer, RadioState, TransferDirection, WindowState};

    #[test]
    fn apply_snapshot_emits_radio_and_pending_transitions() {
        let (tx, rx) = mpsc::channel();
        let mut last_pending = None;
        let mut last_active_send = false;
        let snap = DaemonSnapshot {
            phase: "receiving".into(),
            window: Some(WindowState {
                mode: ReceiveMode::Timed,
                remaining_seconds: Some(125),
                expires_at_ms: None,
            }),
            radio: RadioState {
                ready: true,
                visible: Some(true),
                reason: None,
            },
            pending: Some(PendingTransfer {
                id: 1,
                sender: "Test Mac".into(),
                items: vec!["photo.jpg".into()],
                total_items: 1,
            }),
            ..Default::default()
        };
        let suppress = AtomicBool::new(false);
        apply_snapshot(
            &snap,
            &mut last_pending,
            &mut last_active_send,
            &tx,
            &suppress,
        );
        let events: Vec<Event> = rx.try_iter().collect();
        assert!(matches!(&events[0], Event::Radio(s) if s == "Radio: 2:05 remaining · visible"));
        assert!(
            matches!(&events[1], Event::Incoming(s, items, total, id) if s == "Test Mac" && items == &vec!["photo.jpg".to_string()] && *total == 1 && *id == 1)
        );

        let cleared = DaemonSnapshot::default();
        apply_snapshot(
            &cleared,
            &mut last_pending,
            &mut last_active_send,
            &tx,
            &suppress,
        );
        let events2: Vec<Event> = rx.try_iter().collect();
        assert!(matches!(&events2[0], Event::Radio(s) if s == "Radio window closed"));
        assert!(matches!(&events2[1], Event::IncomingAllCleared));
    }

    #[test]
    fn apply_snapshot_announces_completion_when_the_active_transfer_clears() {
        let (tx, rx) = mpsc::channel();
        let mut last_pending = None;
        let mut last_active_send = false;
        let sending = DaemonSnapshot {
            phase: "sending".into(),
            active_transfer: Some(ActiveTransfer {
                direction: TransferDirection::Send,
                target: "iPhone".into(),
                sent_bytes: 100,
                total_bytes: Some(100),
            }),
            ..Default::default()
        };
        let suppress = AtomicBool::new(false);
        apply_snapshot(
            &sending,
            &mut last_pending,
            &mut last_active_send,
            &tx,
            &suppress,
        );
        let _: Vec<Event> = rx.try_iter().collect();
        assert!(last_active_send, "active send was not tracked");

        let done = DaemonSnapshot::default();
        apply_snapshot(
            &done,
            &mut last_pending,
            &mut last_active_send,
            &tx,
            &suppress,
        );
        let events: Vec<Event> = rx.try_iter().collect();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Status(s) if s == "Send finished")),
            "no completion status emitted once the active transfer cleared"
        );
        assert!(!last_active_send);
    }

    #[test]
    fn apply_snapshot_suppresses_finished_right_after_a_cancel() {
        let (tx, rx) = mpsc::channel();
        let mut last_pending = None;
        let mut last_active_send = false;
        let suppress = AtomicBool::new(false);
        let sending = DaemonSnapshot {
            phase: "sending".into(),
            active_transfer: Some(ActiveTransfer {
                direction: TransferDirection::Send,
                target: "iPhone".into(),
                sent_bytes: 100,
                total_bytes: Some(100),
            }),
            ..Default::default()
        };
        apply_snapshot(
            &sending,
            &mut last_pending,
            &mut last_active_send,
            &tx,
            &suppress,
        );
        let _: Vec<Event> = rx.try_iter().collect();

        // The UI's own Cancel handler sets this before the next snapshot lands.
        suppress.store(true, Ordering::Relaxed);
        let done = DaemonSnapshot::default();
        apply_snapshot(
            &done,
            &mut last_pending,
            &mut last_active_send,
            &tx,
            &suppress,
        );
        let events: Vec<Event> = rx.try_iter().collect();
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Event::Status(s) if s == "Send finished")),
            "a cancelled send must not also be announced as finished"
        );
        assert!(
            !suppress.load(Ordering::Relaxed),
            "the flag must be consumed, not left set for the next real finish"
        );
    }

    #[tokio::test]
    async fn approve_failure_keeps_the_incoming_card() {
        // No socket listens here, so every daemon::ctl call fails the way an
        // unreachable daemon would; a per-test tempdir avoids touching the
        // process-global XDG_RUNTIME_DIR, which other tests read concurrently.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("control.sock");

        let (actions_tx, actions_rx) = tokio::sync::mpsc::unbounded_channel();
        let (events_tx, events_rx) = mpsc::channel();
        let engine_task = tokio::spawn(engine(actions_rx, events_tx, socket));

        actions_tx.send(Action::Decline(7, 42)).unwrap();

        let mut saw_cleared = false;
        let mut saw_transport_error = false;
        for _ in 0..50 {
            match events_rx.try_recv() {
                Ok(Event::IncomingCleared(7)) => saw_cleared = true,
                Ok(Event::Status(s)) if s.contains("Could not reach") => {
                    saw_transport_error = true;
                }
                Ok(_) => {}
                Err(mpsc::TryRecvError::Empty) => {
                    // Once the transport-error status has arrived, keep
                    // draining without sleeping so a same-tick
                    // `IncomingCleared` (the bug this guards against) is not
                    // missed by stopping too early.
                    if saw_transport_error {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(mpsc::TryRecvError::Disconnected) => break,
            }
        }

        let _ = actions_tx.send(Action::Quit);
        let _ = engine_task.await;

        assert!(saw_transport_error, "expected a daemon-unreachable status");
        assert!(!saw_cleared, "card was cleared despite the ctl failure");
    }

    #[tokio::test]
    async fn approve_sends_the_displayed_transfer_s_pending_id() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("control.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut reader = BufReader::new(read);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let req: daemon::Request = serde_json::from_str(line.trim()).unwrap();
            let reply = daemon::Reply {
                ok: true,
                state: "idle".into(),
                message: "transfer approved".into(),
                data: None,
                snapshot: None,
            };
            write
                .write_all(serde_json::to_string(&reply).unwrap().as_bytes())
                .await
                .unwrap();
            write.write_all(b"\n").await.unwrap();
            req
        });

        let (actions_tx, actions_rx) = tokio::sync::mpsc::unbounded_channel();
        let (events_tx, events_rx) = mpsc::channel();
        let engine_task = tokio::spawn(engine(actions_rx, events_tx, socket));

        // The card being accepted shows pending id 99; a stale local card id
        // (3) must not leak into the wire request in its place.
        actions_tx.send(Action::Approve(3, 99)).unwrap();

        for _ in 0..50 {
            if let Ok(Event::Status(s)) = events_rx.try_recv() {
                assert_eq!(s, "transfer approved");
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let _ = actions_tx.send(Action::Quit);
        let _ = engine_task.await;
        let req = server.await.unwrap();

        assert_eq!(req.op, "approve");
        assert_eq!(req.id, Some(99));
    }
}
