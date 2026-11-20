use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::time::Duration;
/// How long a single `awdlctl status` probe may take. Callers that wait on a
/// readiness loop have to add this to their own budget: the loop can start a
/// probe just before its deadline and still owe it this long.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

pub async fn radio() -> Result<Value> {
    let output = tokio::time::timeout(
        PROBE_TIMEOUT,
        tokio::process::Command::new("awdlctl")
            .args(["status", "--json"])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("awdlctl status timed out")?
    .context("awdlctl is not installed; build the sibling brcmfmac-awdl repository")?;
    ensure!(
        output.status.success(),
        "awdlctl: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}
#[allow(dead_code)]
pub async fn require_radio() -> Result<()> {
    let r = radio().await?;
    ensure!(
        r["ready"] == true,
        "AWDL link unavailable: {}",
        r["reason"].as_str().unwrap_or("unknown")
    );
    Ok(())
}
pub async fn report(keys: Option<&std::path::Path>, identity: Option<&std::path::Path>) -> Value {
    let radio = radio().await;
    let mut missing = Vec::new();
    match &radio {
        Ok(r) if r["ready"] == true => {}
        Ok(r) => missing.push(format!("radio: {}", r["reason"])),
        Err(e) => missing.push(format!("radio: {e:#}")),
    }
    for (label, path) in [("BLE keys", keys), ("Pair-Verify identity", identity)] {
        if let Some(p) = path {
            let valid = if label == "BLE keys" {
                crate::keystore::KeyStore::load(p).map(|k| !k.is_empty())
            } else {
                crate::companion::PairingIdentity::load(p).map(|_| true)
            };
            if !matches!(valid, Ok(true)) {
                missing.push(format!("{label}: missing or invalid at {}", p.display()));
            }
        }
    }
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        missing.push("Wayland display unavailable".into());
    }
    if !std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|p| p.join("wl-copy").is_file()))
    {
        missing.push("wl-copy is not installed".into());
    }
    json!({"ready":missing.is_empty(),"missing":missing,"radio":radio.ok(),"protocol_validated":false})
}

const DEBUG_DROPIN_DIR: &str = "/etc/systemd/system/ac-dc-daemon.service.d";
const DEBUG_DROPIN_FILE: &str = "/etc/systemd/system/ac-dc-daemon.service.d/debug.conf";

#[derive(Debug, serde::Serialize)]
pub struct DebugReport {
    pub daemon_active: String,
    pub user_daemon_active: String,
    pub radio_socket_present: bool,
    pub socket_present: bool,
    pub socket_info: String,
    pub ctl_status: Option<serde_json::Value>,
    pub ctl_error: Option<String>,
    pub radio: Option<Value>,
    pub radio_error: Option<String>,
    pub journal_tail: Vec<String>,
    pub user_journal_tail: Vec<String>,
}

pub fn redact_sensitive(line: &str) -> String {
    let lower = line.to_ascii_lowercase();
    if lower.contains("private_key")
        || lower.contains("pasteboard")
        || lower.contains("key_hex")
        || lower.contains("secret")
        || lower.contains("token")
    {
        "[redacted sensitive log line]".to_string()
    } else {
        line.to_string()
    }
}

pub fn set_daemon_debug(enable: bool) -> Result<()> {
    #[cfg(unix)]
    if unsafe { libc::geteuid() } != 0 {
        let exe = std::env::current_exe().context("resolve ac-dc binary path")?;
        let flag = if enable { "--enable" } else { "--disable" };
        let status = std::process::Command::new("pkexec")
            .arg(exe)
            .args(["debug", flag])
            .status()
            .context("invoke pkexec for ac-dc debug")?;
        ensure!(status.success(), "pkexec exited with {status}");
        return Ok(());
    }

    if enable {
        std::fs::create_dir_all(DEBUG_DROPIN_DIR)
            .with_context(|| format!("create {DEBUG_DROPIN_DIR}"))?;
        std::fs::write(
            DEBUG_DROPIN_FILE,
            "[Service]\nEnvironment=RUST_LOG=ac_dc=debug\n",
        )
        .with_context(|| format!("write {DEBUG_DROPIN_FILE}"))?;
    } else if std::path::Path::new(DEBUG_DROPIN_FILE).exists() {
        std::fs::remove_file(DEBUG_DROPIN_FILE)
            .with_context(|| format!("remove {DEBUG_DROPIN_FILE}"))?;
    }

    let reload = std::process::Command::new("systemctl")
        .arg("daemon-reload")
        .status()
        .context("run systemctl daemon-reload")?;
    ensure!(reload.success(), "systemctl daemon-reload failed: {reload}");
    let restart = std::process::Command::new("systemctl")
        .args(["restart", "ac-dc-daemon.service"])
        .status()
        .context("restart ac-dc-daemon.service")?;
    ensure!(restart.success(), "systemctl restart failed: {restart}");
    Ok(())
}

pub async fn debug_report(socket: &std::path::Path) -> DebugReport {
    let daemon_active = tokio::process::Command::new("systemctl")
        .args(["is-active", "ac-dc-daemon.service"])
        .output()
        .await
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|e| format!("unavailable ({e})"));

    let user_daemon_active = tokio::process::Command::new("systemctl")
        .args(["--user", "is-active", "ac-dc-user.service"])
        .output()
        .await
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|e| format!("unavailable ({e})"));

    let radio_socket_present = std::path::Path::new(crate::radio_daemon::DEFAULT_SOCKET).exists();

    let (socket_present, socket_info) = match std::fs::metadata(socket) {
        Ok(meta) => {
            #[cfg(unix)]
            let info = {
                use std::os::unix::fs::MetadataExt;
                format!(
                    "uid={} gid={} mode={:o} path={}",
                    meta.uid(),
                    meta.gid(),
                    meta.mode() & 0o777,
                    socket.display()
                )
            };
            #[cfg(not(unix))]
            let info = format!("present path={}", socket.display());
            (true, info)
        }
        Err(_) => (false, format!("MISSING: {}", socket.display())),
    };

    let (ctl_status, ctl_error) =
        match crate::daemon::ctl(socket.to_path_buf(), crate::daemon::request("status")).await {
            Ok(reply) => (serde_json::to_value(reply).ok(), None),
            Err(e) => (None, Some(format!("{e:#}"))),
        };

    let (radio, radio_error) = match radio().await {
        Ok(r) => (Some(r), None),
        Err(e) => (None, Some(format!("{e:#}"))),
    };

    let journal_tail = tokio::process::Command::new("journalctl")
        .args([
            "-u",
            "ac-dc-daemon.service",
            "--since",
            "2 minutes ago",
            "--no-pager",
            "-o",
            "short-precise",
            "-n",
            "40",
        ])
        .output()
        .await
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(redact_sensitive)
                .collect()
        })
        .unwrap_or_default();

    // The clipboard and transfer paths run in the user session, not the
    // privileged daemon, so their logs live in a separate unit.
    let user_journal_tail = tokio::process::Command::new("journalctl")
        .args([
            "--user",
            "-u",
            "ac-dc-user.service",
            "--since",
            "2 minutes ago",
            "--no-pager",
            "-o",
            "short-precise",
            "-n",
            "40",
        ])
        .output()
        .await
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(redact_sensitive)
                .collect()
        })
        .unwrap_or_default();

    DebugReport {
        daemon_active,
        user_daemon_active,
        radio_socket_present,
        socket_present,
        socket_info,
        ctl_status,
        ctl_error,
        radio,
        radio_error,
        journal_tail,
        user_journal_tail,
    }
}

pub fn format_debug_report(report: &DebugReport, json: bool) -> Result<String> {
    if json {
        return Ok(serde_json::to_string_pretty(report)?);
    }
    let mut out = String::new();
    out.push_str(&format!(
        "== Daemons ==\nradio (system): {}\nuser (session): {}\nradio socket: {}\n",
        report.daemon_active,
        report.user_daemon_active,
        if report.radio_socket_present {
            crate::radio_daemon::DEFAULT_SOCKET
        } else {
            "MISSING"
        }
    ));
    out.push_str(&format!("\n== Control socket ==\n{}\n", report.socket_info));
    out.push_str("\n== CLI status ==\n");
    if let Some(status) = &report.ctl_status {
        out.push_str(&serde_json::to_string(status)?);
        out.push('\n');
    } else if let Some(err) = &report.ctl_error {
        out.push_str(&format!("error: {err}\n"));
    }
    out.push_str("\n== AWDL health ==\n");
    if let Some(radio) = &report.radio {
        out.push_str(&serde_json::to_string(radio)?);
        out.push('\n');
    } else if let Some(err) = &report.radio_error {
        out.push_str(&format!("error: {err}\n"));
    }
    if !report.journal_tail.is_empty() {
        out.push_str("\n== Recent daemon journal ==\n");
        for line in &report.journal_tail {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !report.user_journal_tail.is_empty() {
        out.push_str("\n== Recent user-service journal ==\n");
        for line in &report.user_journal_tail {
            out.push_str(line);
            out.push('\n');
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_sensitive_masks_secret_bearing_lines() {
        assert_eq!(
            redact_sensitive("loaded private_key=deadbeef"),
            "[redacted sensitive log line]"
        );
        assert_eq!(
            redact_sensitive("pasteboard item text=secret"),
            "[redacted sensitive log line]"
        );
        assert_eq!(
            redact_sensitive("ac-dc daemon listening"),
            "ac-dc daemon listening"
        );
    }

    #[test]
    fn format_debug_report_supports_json_and_text() {
        let report = DebugReport {
            daemon_active: "active".into(),
            user_daemon_active: "active".into(),
            radio_socket_present: true,
            socket_present: true,
            socket_info: "uid=1000 gid=1000 mode=600 path=/run/user/1000/ac-dc/control.sock".into(),
            ctl_status: Some(json!({"ok": true, "state": "idle"})),
            ctl_error: None,
            radio: Some(json!({"ready": false, "reason": "no_awdl0"})),
            radio_error: None,
            journal_tail: vec!["line1".into()],
            user_journal_tail: vec![],
        };
        let text = format_debug_report(&report, false).unwrap();
        assert!(text.contains("radio (system): active"));
        assert!(text.contains("user (session): active"));
        assert!(text.contains("== Control socket =="));
        let json_str = format_debug_report(&report, true).unwrap();
        let parsed: Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(parsed["daemon_active"], "active");
        assert_eq!(parsed["user_daemon_active"], "active");
        assert_eq!(parsed["socket_present"], true);
    }

    #[test]
    fn format_debug_report_includes_the_user_service_journal() {
        let report = DebugReport {
            daemon_active: "active".into(),
            user_daemon_active: "active".into(),
            radio_socket_present: true,
            socket_present: true,
            socket_info: "uid=1000 gid=1000 mode=600 path=/run/user/1000/ac-dc/control.sock".into(),
            ctl_status: None,
            ctl_error: None,
            radio: None,
            radio_error: None,
            journal_tail: vec![],
            user_journal_tail: vec!["clipboard watcher stopped".into()],
        };
        let text = format_debug_report(&report, false).unwrap();
        assert!(text.contains("== Recent user-service journal =="));
        assert!(text.contains("clipboard watcher stopped"));
        assert!(!text.contains("== Recent daemon journal =="));
    }
}
