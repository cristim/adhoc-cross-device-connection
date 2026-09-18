//! Native GTK desktop controls. Protocol and radio work runs on the Tokio runtime.
use crate::airdrop::{self, peers::Peer};
use anyhow::{Context, Result};
use gtk::{glib, prelude::*};
use std::{cell::RefCell, path::PathBuf, rc::Rc, time::Duration};
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
}
enum Event {
    Status(String),
    Radio(String),
    Peers(Vec<Peer>),
    Incoming(airdrop::Incoming),
}
fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}
fn identity() -> PathBuf {
    home().join(".local/share/ac-dc/airdrop")
}
async fn command(program: &str, args: &[&str]) -> Result<()> {
    let result = tokio::time::timeout(
        Duration::from_secs(120),
        tokio::process::Command::new(program)
            .args(args)
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    anyhow::ensure!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr).trim()
    );
    Ok(())
}

/// Recipient discovery is brokered by the root daemon.  The full GUI must
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
async fn radio_start() -> Result<()> {
    let requested = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    command(
        "pkexec",
        &[
            "/usr/bin/systemctl",
            "reload-or-restart",
            "ac-dc-airdrop-radio.service",
        ],
    )
    .await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
    loop {
        let announced = airdrop::radio_state().is_some_and(|s| {
            s["expires_at"]
                .as_u64()
                .is_some_and(|end| end >= requested + 590)
        });
        if announced && crate::health::radio().await?["ready"] == true {
            return Ok(());
        }
        anyhow::ensure!(
            tokio::time::Instant::now() < deadline,
            "Airdrop-compatible radio did not become ready; inspect journalctl -u ac-dc-airdrop-radio"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
async fn engine(
    mut actions: tokio::sync::mpsc::UnboundedReceiver<Action>,
    events: std::sync::mpsc::Sender<Event>,
) {
    let mut receiver: Option<tokio::task::JoinHandle<()>> = None;
    let mut radio_owned = false;
    let mut transfers: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    let mut browsing: Option<tokio::task::JoinHandle<()>> = None;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        let action = tokio::select! {
            action=actions.recv()=>{let Some(a)=action else{break};a},
            _=tick.tick()=>{
                let text=if let Some(state)=airdrop::radio_state(){
                    let now=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
                    let remaining=state["expires_at"].as_u64().unwrap_or(now).saturating_sub(now);
                    format!("Radio: {}:{:02} remaining · received frames: {} · TX completions: {}{}",remaining/60,remaining%60,state["received_frames"],state["tx_completions"],if state["phase"]=="data_observed"{" · traffic observed"}else{" · transport unproven"})
                }else{"Radio window closed".into()};
                let _=events.send(Event::Radio(text));continue;
            }
        };
        transfers.retain(|h| !h.is_finished());
        match action {
            Action::Start(p) => {
                let _ = events.send(Event::Status(
                    "Starting Airdrop-compatible radio — authentication may be required…".into(),
                ));
                match radio_start().await {
                    Err(e) => {
                        let _ = events.send(Event::Status(format!("Could not start radio: {e:#}")));
                    }
                    Ok(()) => {
                        radio_owned = true;
                        if receiver.as_ref().is_some_and(|h| !h.is_finished()) {
                            let _ = events.send(Event::Status(
                                "Receive window extended by 10 minutes".into(),
                            ));
                            continue;
                        }
                        // Avoid racing the optional CLI service for the listening port.
                        let _ = command("systemctl", &["--user", "stop", "ac-dc-receive.service"])
                            .await;
                        let (approval, mut requests) = tokio::sync::mpsc::channel(8);
                        let ev = events.clone();
                        receiver = Some(tokio::spawn(async move {
                            let config = airdrop::Config {
                                iface: "awdl0".into(),
                                directory: p.directory,
                                identity: identity(),
                                name: p.name,
                                port: 8771,
                                seconds: 600,
                                once: false,
                                notify: true,
                                open_destination: true,
                                radio_managed: true,
                                ble_wake: p.ble_wake,
                                approval: Some(approval),
                            };
                            let task = airdrop::run(config);
                            tokio::pin!(task);
                            loop {
                                tokio::select! {
                                    result=&mut task=>{let msg=match result {Ok(())=>"Receiving window ended".into(),Err(e)=>format!("Receiver stopped: {e:#}")};let _=ev.send(Event::Status(msg));break;}
                                    Some(request)=requests.recv()=>{let _=ev.send(Event::Incoming(request));}
                                }
                            }
                        }));
                        let _=events.send(Event::Status("Receiving for 10 minutes. Incoming transfers require your approval. Open Airdrop-compatible on your iPhone or Mac.".into()));
                    }
                }
            }
            Action::Stop | Action::Quit => {
                let quit = matches!(action, Action::Quit);
                for h in transfers.drain(..) {
                    h.abort();
                    let _ = h.await;
                }
                if let Some(h) = browsing.take() {
                    h.abort();
                    let _ = h.await;
                }
                if let Some(h) = receiver.take() {
                    h.abort();
                    let _ = h.await;
                }
                if radio_owned {
                    let result = command(
                        "pkexec",
                        &["/usr/bin/systemctl", "stop", "ac-dc-airdrop-radio.service"],
                    )
                    .await;
                    radio_owned = false;
                    let _ = events.send(Event::Status(match result {
                        Ok(()) => "Airdrop-compatible stopped".into(),
                        Err(e) => format!("Receiver stopped; radio cleanup: {e:#}"),
                    }));
                }
                if quit {
                    break;
                }
            }
            Action::Browse => {
                let _=events.send(Event::Status("Looking for nearby recipients. Set the Apple device to Everyone for 10 Minutes.".into()));
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
            Action::Cancel => {
                for h in transfers.drain(..) {
                    h.abort();
                    let _ = h.await;
                }
                let _ = events.send(Event::Status("Outgoing transfer cancelled".into()));
            }
            Action::Send(peer, p, files, links) => {
                if !transfers.is_empty() {
                    let _ = events.send(Event::Status(
                        "A transfer is already running; cancel it before sending again".into(),
                    ));
                    continue;
                }
                if let Err(e) = radio_start().await {
                    let _ = events.send(Event::Status(format!("Radio: {e:#}")));
                    continue;
                }
                radio_owned = true;
                let ev = events.clone();
                transfers.push(tokio::spawn(async move {
                    let _ = ev.send(Event::Status(format!(
                        "Preparing transfer for {}…",
                        peer.name
                    )));
                    let result = async {
                        let client = airdrop::send::Client::new(peer.address, &identity())?;
                        let(tx,mut progress)=tokio::sync::watch::channel((0u64,0u64));
                        let transfer=client.transfer_with_progress(&p.name, files, links,Some(tx));
                        let wake=crate::advertise::broadcast(crate::advertise::airdrop_wake(),600,100);
                        tokio::pin!(transfer);tokio::pin!(wake);
                        let mut wake_done=false;
                        let mut interval=tokio::time::interval(Duration::from_millis(250));
                        loop {tokio::select! {
                            result=&mut transfer=>break result,
                            result=&mut wake,if !wake_done=>{wake_done=true;if let Err(error)=result{tracing::warn!(%error,"Bluetooth wake failed");}},
                            _=interval.tick()=>{let(sent,total)=*progress.borrow_and_update();if total>0{let _=ev.send(Event::Status(if sent==0 {format!("Waiting for {} to accept…",peer.name)}else if sent==total {format!("Upload sent to {}; waiting for confirmation…",peer.name)}else{format!("Sending to {}: {:.1} / {:.1} MiB ({}%)",peer.name,sent as f64/1048576.0,total as f64/1048576.0,sent*100/total)}));}}
                        }}
                    }
                    .await;
                    let _ = ev.send(Event::Status(match result {
                        Ok(()) => format!("Transfer delivered to {}", peer.name),
                        Err(e) => format!("Transfer failed: {e:#}"),
                    }));
                }));
            }
        }
    }
    for h in transfers {
        h.abort();
    }
    if let Some(h) = browsing {
        h.abort();
    }
    if let Some(h) = receiver {
        h.abort();
    }
}
pub async fn run() -> Result<()> {
    let runtime = tokio::runtime::Handle::current();
    let prefs_path = home().join(".config/ac-dc/ui.json");
    let prefs: Preferences = std::fs::read(&prefs_path)
        .ok()
        .and_then(|v| serde_json::from_slice(&v).ok())
        .unwrap_or_default();
    let (actions, rx) = tokio::sync::mpsc::unbounded_channel();
    let (events, results) = std::sync::mpsc::channel();
    let worker = runtime.spawn(engine(rx, events));
    let results = Rc::new(RefCell::new(Some(results)));
    let application = gtk::Application::builder()
        .application_id("org.omarchy.adhoccrossdeviceconnection")
        .build();
    let shutdown = actions.clone();
    application.connect_activate(move |app| {
        let Some(results) = results.borrow_mut().take() else {
            if let Some(window) = app.active_window() {
                window.present();
            }
            return;
        };
        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .title(concat!(
                "ac-dc ",
                env!("CARGO_PKG_VERSION"),
                " · Airdrop-compatible"
            ))
            .default_width(640)
            .default_height(620)
            .build();
        let column = gtk::Box::new(gtk::Orientation::Vertical, 14);
        column.set_margin_top(24);
        column.set_margin_bottom(24);
        column.set_margin_start(24);
        column.set_margin_end(24);
        let heading = gtk::Label::new(Some("Airdrop-compatible"));
        heading.add_css_class("title-1");
        heading.set_xalign(0.0);
        column.append(&heading);
        let subtitle = gtk::Label::new(Some("Share files and links with nearby Apple devices"));
        subtitle.set_xalign(0.0);
        column.append(&subtitle);
        let name = gtk::Entry::builder()
            .placeholder_text("Your device name")
            .text(&prefs.name)
            .build();
        column.append(&name);
        let folder = gtk::Entry::builder()
            .placeholder_text("Receive folder")
            .text(prefs.directory.to_string_lossy())
            .build();
        column.append(&folder);
        let ble_wake = gtk::CheckButton::with_label("Use Bluetooth to help nearby devices find me");
        ble_wake.set_active(prefs.ble_wake);
        column.append(&ble_wake);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let receive = gtk::Button::with_label("Receive for 10 minutes");
        receive.add_css_class("suggested-action");
        let stop = gtk::Button::with_label("Stop receiving");
        row.append(&receive);
        row.append(&stop);
        column.append(&row);
        let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
        column.append(&separator);
        let peers = Rc::new(RefCell::new(Vec::<Peer>::new()));
        let recipients = gtk::DropDown::from_strings(&[]);
        recipients.set_hexpand(true);
        let find = gtk::Button::with_label("Find recipients");
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.append(&recipients);
        row.append(&find);
        column.append(&row);
        let files = Rc::new(RefCell::new(Vec::<PathBuf>::new()));
        let file_label = gtk::Label::new(Some("No files selected"));
        file_label.set_wrap(true);
        file_label.set_xalign(0.0);
        let choose = gtk::Button::with_label("Choose files…");
        column.append(&choose);
        let choose_folder = gtk::Button::with_label("Choose folder…");
        column.append(&choose_folder);
        column.append(&file_label);
        let link = gtk::Entry::builder()
            .placeholder_text("Or paste an https:// link")
            .build();
        column.append(&link);
        let send = gtk::Button::with_label("Send to selected recipient");
        send.add_css_class("suggested-action");
        column.append(&send);
        let cancel = gtk::Button::with_label("Cancel outgoing transfer");
        column.append(&cancel);
        let radio_label = gtk::Label::new(Some("Radio window closed"));
        radio_label.set_wrap(true);
        radio_label.set_xalign(0.0);
        column.append(&radio_label);
        let status = gtk::Label::new(Some(
            "Ready. Everyone mode only; Apple-device compatibility is being tested.",
        ));
        status.set_wrap(true);
        status.set_xalign(0.0);
        status.set_selectable(true);
        column.append(&status);
        let approval_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
        column.append(&approval_box);
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&column)
            .build();
        window.set_child(Some(&scroller));
        let preferences = {
            let name = name.clone();
            let folder = folder.clone();
            let ble_wake = ble_wake.clone();
            let path = prefs_path.clone();
            move || -> Result<Preferences> {
                let p = Preferences {
                    name: name.text().to_string(),
                    directory: PathBuf::from(folder.text().as_str()),
                    ble_wake: ble_wake.is_active(),
                };
                anyhow::ensure!(
                    !p.name.is_empty() && p.name.len() <= 63,
                    "Use a device name between 1 and 63 bytes"
                );
                anyhow::ensure!(
                    p.directory.is_absolute(),
                    "Use an absolute receive folder path"
                );
                std::fs::create_dir_all(path.parent().context("preferences directory")?)?;
                std::fs::write(&path, serde_json::to_vec_pretty(&p)?)?;
                Ok(p)
            }
        };
        let preferences = Rc::new(preferences);
        {
            let tx = actions.clone();
            let p = preferences.clone();
            let label = status.clone();
            receive.connect_clicked(move |_| match p() {
                Ok(p) => {
                    let _ = tx.send(Action::Start(p));
                }
                Err(e) => label.set_text(&e.to_string()),
            });
        }
        {
            let tx = actions.clone();
            stop.connect_clicked(move |_| {
                let _ = tx.send(Action::Stop);
            });
        }
        {
            let tx = actions.clone();
            find.connect_clicked(move |_| {
                let _ = tx.send(Action::Browse);
            });
        }
        {
            let parent = window.clone();
            let files = files.clone();
            let label = file_label.clone();
            choose.connect_clicked(move |_| {
                let parent = parent.clone();
                let files = files.clone();
                let label = label.clone();
                glib::spawn_future_local(async move {
                    let dialog = gtk::FileDialog::builder()
                        .title("Choose files to send")
                        .build();
                    if let Ok(selection) = dialog.open_multiple_future(Some(&parent)).await {
                        let selected = (0..selection.n_items())
                            .filter_map(|i| {
                                selection.item(i)?.downcast::<gtk::gio::File>().ok()?.path()
                            })
                            .collect::<Vec<_>>();
                        label.set_text(&format!("{} file(s) selected", selected.len()));
                        *files.borrow_mut() = selected;
                    }
                });
            });
        }
        {
            let tx = actions.clone();
            let peers = peers.clone();
            let recipients = recipients.clone();
            let prefs = preferences.clone();
            let files = files.clone();
            let link = link.clone();
            let label = status.clone();
            send.connect_clicked(move |_| {
                let result = (|| -> Result<Action> {
                    let peer = peers
                        .borrow()
                        .get(recipients.selected() as usize)
                        .cloned()
                        .context("Choose a recipient first")?;
                    let url = link.text().to_string();
                    let links = if url.trim().is_empty() {
                        vec![]
                    } else {
                        vec![url.trim().into()]
                    };
                    let files = if links.is_empty() {
                        files.borrow().clone()
                    } else {
                        vec![]
                    };
                    Ok(Action::Send(peer, prefs()?, files, links))
                })();
                match result {
                    Ok(action) => {
                        let _ = tx.send(action);
                    }
                    Err(e) => label.set_text(&e.to_string()),
                }
            });
        }
        {
            let tx = actions.clone();
            cancel.connect_clicked(move |_| {
                let _ = tx.send(Action::Cancel);
            });
        }
        {
            let parent = window.clone();
            let selected = files.clone();
            let label = file_label.clone();
            choose_folder.connect_clicked(move |_| {
                let parent = parent.clone();
                let selected = selected.clone();
                let label = label.clone();
                glib::spawn_future_local(async move {
                    if let Ok(folder) = gtk::FileDialog::builder()
                        .title("Choose folder to send")
                        .build()
                        .select_folder_future(Some(&parent))
                        .await
                    {
                        if let Some(path) = folder.path() {
                            label.set_text(&format!("Folder: {}", path.display()));
                            *selected.borrow_mut() = vec![path];
                        }
                    }
                });
            });
        }
        type Decision = Rc<RefCell<Option<tokio::sync::oneshot::Sender<bool>>>>;
        let mut decisions: Vec<(gtk::Box, Decision)> = Vec::new();
        glib::timeout_add_local(Duration::from_millis(100), move || {
            while let Ok(event) = results.try_recv() {
                match event {
                    Event::Status(text) => status.set_text(&text),
                    Event::Radio(text) => radio_label.set_text(&text),
                    Event::Peers(list) => {
                        let names = list
                            .iter()
                            .map(|peer| format!("{} — {}", peer.name, peer.address))
                            .collect::<Vec<_>>();
                        let refs = names.iter().map(String::as_str).collect::<Vec<_>>();
                        recipients.set_model(Some(&gtk::StringList::new(&refs)));
                        if !list.is_empty() {
                            recipients.set_selected(0);
                        }
                        *peers.borrow_mut() = list;
                    }
                    Event::Incoming(request) => {
                        let card = gtk::Box::new(gtk::Orientation::Vertical, 6);
                        let text = gtk::Label::new(Some(&format!(
                            "{} wants to share:\n{}",
                            request.sender,
                            request.items.join("\n")
                        )));
                        text.set_wrap(true);
                        card.append(&text);
                        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                        let accept = gtk::Button::with_label("Accept");
                        let decline = gtk::Button::with_label("Decline");
                        row.append(&accept);
                        row.append(&decline);
                        card.append(&row);
                        approval_box.append(&card);
                        let decision = Rc::new(RefCell::new(Some(request.decision)));
                        for (button, value) in [(accept, true), (decline, false)] {
                            let d = decision.clone();
                            let card = card.clone();
                            let parent = approval_box.clone();
                            button.connect_clicked(move |_| {
                                if let Some(tx) = d.borrow_mut().take() {
                                    let _ = tx.send(value);
                                }
                                parent.remove(&card);
                            });
                        }
                        decisions.push((card, decision));
                    }
                }
            }
            decisions.retain(|(card, decision)| {
                let expired = decision.borrow().as_ref().is_none_or(|tx| tx.is_closed());
                if expired && card.parent().is_some() {
                    approval_box.remove(card);
                }
                !expired
            });
            glib::ControlFlow::Continue
        });
        let tx = actions.clone();
        window.connect_close_request(move |_| {
            let _ = tx.send(Action::Quit);
            glib::Propagation::Proceed
        });
        window.present();
    });
    application.run_with_args::<&str>(&[]);
    // A secondary invocation forwards activation to the existing GTK instance
    // and returns without a window. Its worker must still be shut down.
    let _ = shutdown.send(Action::Quit);
    worker.await?;
    Ok(())
}
