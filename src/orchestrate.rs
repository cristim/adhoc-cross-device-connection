//! Milestone 2 orchestration: the state machine that ties M1 + M2 together for
//! the eventual pull-on-copy flow.
//!
//! The end-to-end path is:
//!
//! ```text
//!   M1 scan sees "clipboard available" for a device
//!        -> [AWDL bring-up]        <-- TODO gate, unimplemented today
//!        -> discover companion-link over awdl0
//!        -> pull the pasteboard over companion-link
//!        -> clipboard_out -> Wayland clipboard
//! ```
//!
//! ## What is real vs. gated
//!
//! * **Real (loopback):** the `pull -> clipboard_out` glue. `ac-dc auto
//!   --loopback` runs the whole tail of the flow against the in-process mock
//!   peer ([`companion_client::loopback_demo`]) and lands its text on the
//!   Wayland clipboard — no AWDL, no hardware.
//! * **Gated on AWDL:** [`bring_up_awdl`] is an unimplemented gate (returns a
//!   descriptive error), and [`crate::discover::companion_link_target`] over
//!   `awdl0` errors until the interface exists. So the *real* (non-loopback)
//!   flow deliberately stops at the AWDL stage today. We do NOT fake it.
//!
//! The stage transitions ([`Stage`] / [`next_stage`]) are pure and unit-tested.

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::clipboard_out;
use crate::companion::PairingIdentity;
use crate::companion_client::{self, Pasteboard, SystemInfo};
use crate::discover;

/// The stages of the pull-on-copy flow, in order. A real, walked state machine
/// (see [`run`]/`next_stage`), not decoration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// M1 scan reported a `clipboard_available` advert for a known device.
    ClipboardDetected,
    /// Bring the AWDL interface up so companion-link is reachable. TODO gate.
    AwdlBringUp,
    /// Resolve `_companion-link._tcp` (host/port) over `awdl0`.
    Discover,
    /// Run the companion-link pull (Pair-Verify + system-info + fetch).
    Pull,
    /// Put the fetched item on the Wayland clipboard.
    Copy,
    /// Terminal.
    Done,
}

/// Pure successor function for the state machine. `None` at the terminal stage.
pub fn next_stage(stage: Stage) -> Option<Stage> {
    Some(match stage {
        Stage::ClipboardDetected => Stage::AwdlBringUp,
        Stage::AwdlBringUp => Stage::Discover,
        Stage::Discover => Stage::Pull,
        Stage::Pull => Stage::Copy,
        Stage::Copy => Stage::Done,
        Stage::Done => return None,
    })
}

/// What M1 scan knows the instant it sees a `clipboard_available` advert.
///
/// `scan::Worker::report` logs exactly this today (device label + activity).
/// Having `scan` emit this struct into the orchestrator is the remaining
/// M1->M2 wiring; the BLE scan already runs standalone via `ac-dc scan`, so
/// this is a small follow-up rather than new mechanism. Kept concrete so the
/// seam is obvious.
#[derive(Debug, Clone)]
pub struct CopyTrigger {
    pub device_label: String,
    pub activity: String,
}

impl CopyTrigger {
    /// A placeholder trigger used to drive the real flow manually (via
    /// `ac-dc auto`) until scan feeds real ones.
    fn placeholder() -> Self {
        CopyTrigger { device_label: "(manual `ac-dc auto`)".into(), activity: "copy".into() }
    }
}

/// Config for the orchestrated flow.
pub struct AutoConfig {
    /// RPIdentity keys for Pair-Verify (real flow only).
    pub identity_path: PathBuf,
    /// Drive the `pull -> clipboard_out` glue against the loopback mock instead
    /// of the AWDL-gated real path.
    pub loopback: bool,
}

/// The TODO gate: bring up the AWDL (`awdl0`) interface.
///
/// UNIMPLEMENTED — returns a descriptive error today. `awdl0` does not exist on
/// this host until the OWL / `brcmfmac` driver bring-up lands
/// (docs/m2-awdl-4378-bringup.md). This is the single honest blocker between
/// the working M1 scan and a real end-to-end pull; we do not stub a fake
/// interface.
pub fn bring_up_awdl() -> Result<()> {
    anyhow::bail!(
        "AWDL bring-up is not implemented yet: `awdl0` does not exist until the OWL / \
         brcmfmac driver work lands (see docs/m2-awdl-4378-bringup.md and \
         docs/m2-awdl-plan.md). Until then the real auto-flow cannot reach \
         companion-link; use `ac-dc auto --loopback` to exercise the pull -> \
         clipboard glue."
    )
}

/// Run the orchestrated flow. `--loopback` runs the real tail against the mock
/// peer; otherwise it walks the real state machine, which stops at the AWDL
/// gate today.
pub async fn run(cfg: AutoConfig) -> Result<()> {
    if cfg.loopback {
        run_loopback().await
    } else {
        run_real(&cfg, CopyTrigger::placeholder()).await
    }
}

/// Loopback path: prove the `pull -> clipboard_out` wiring end-to-end without
/// AWDL. Fetches the mock pasteboard and lands it on the Wayland clipboard.
async fn run_loopback() -> Result<()> {
    tracing::info!("auto (loopback): exercising pull -> clipboard_out against the mock peer");
    let (peer, board) = companion_client::loopback_demo()
        .await
        .context("loopback pull failed")?;
    tracing::info!(peer = %peer.name, items = board.items.len(), "loopback pull complete");
    land_on_clipboard(&board);
    Ok(())
}

/// Carries state across the walked stages of the real flow.
#[derive(Default)]
struct RealCtx {
    target: Option<(String, u16)>,
    board: Option<Pasteboard>,
}

/// Real path: walk `Stage` from `ClipboardDetected`, doing each stage's work.
/// Today it errors out at [`Stage::AwdlBringUp`] — by design, honestly.
async fn run_real(cfg: &AutoConfig, trigger: CopyTrigger) -> Result<()> {
    let mut ctx = RealCtx::default();
    let mut stage = Some(Stage::ClipboardDetected);
    while let Some(s) = stage {
        execute_stage(s, cfg, &trigger, &mut ctx).await?;
        stage = next_stage(s);
    }
    Ok(())
}

/// Do the work for one stage. Kept as a single match so the state machine is
/// legible top to bottom.
async fn execute_stage(
    stage: Stage,
    cfg: &AutoConfig,
    trigger: &CopyTrigger,
    ctx: &mut RealCtx,
) -> Result<()> {
    match stage {
        Stage::ClipboardDetected => {
            tracing::info!(
                device = %trigger.device_label,
                activity = %trigger.activity,
                "stage: clipboard available (M1 scan trigger)"
            );
        }
        Stage::AwdlBringUp => {
            tracing::info!("stage: AWDL bring-up");
            bring_up_awdl().context("AWDL bring-up gate")?; // stops here today
        }
        Stage::Discover => {
            tracing::info!("stage: discover companion-link over awdl0");
            // port 0 => automatic awdl0-scoped resolve.
            let target = discover::companion_link_target("", 0)
                .await
                .context("resolving companion-link over awdl0")?;
            ctx.target = Some(target);
        }
        Stage::Pull => {
            let (host, port) = ctx.target.clone().context("no companion-link target resolved")?;
            tracing::info!(%host, port, "stage: pull over companion-link");
            let ident = PairingIdentity::load(&cfg.identity_path)
                .context("loading RPIdentity for Pair-Verify")?;
            let mut session = companion_client::connect(&host, port, ident).await?;
            let _peer = session
                .system_info_exchange(&SystemInfo {
                    name: "ac-dc (Linux)".into(),
                    model: "ac-dc,client".into(),
                    os_version: env!("CARGO_PKG_VERSION").into(),
                })
                .await?;
            ctx.board = Some(session.fetch_pasteboard().await?);
        }
        Stage::Copy => {
            let board = ctx.board.as_ref().context("no pasteboard fetched")?;
            tracing::info!("stage: copy to Wayland clipboard");
            land_on_clipboard(board);
        }
        Stage::Done => tracing::info!("stage: done"),
    }
    Ok(())
}

/// Put a fetched pasteboard on the Wayland clipboard and report what happened.
/// Shared by the loopback and real paths (and mirrors `ac-dc pull`).
fn land_on_clipboard(board: &Pasteboard) {
    match clipboard_out::copy_pasteboard(board) {
        Ok(Some(outcome)) => println!("clipboard: copied {}", outcome.describe()),
        Ok(None) => println!("clipboard: nothing to copy (empty pasteboard)"),
        Err(e) => tracing::error!(error = %e, "failed to put pasteboard on the clipboard"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard_out::{self, PlanKind};

    #[test]
    fn stage_machine_walks_from_detect_to_done() {
        let mut stage = Stage::ClipboardDetected;
        let mut seq = vec![stage];
        while let Some(next) = next_stage(stage) {
            stage = next;
            seq.push(stage);
            assert!(seq.len() < 16, "state machine did not terminate");
        }
        assert_eq!(seq.first(), Some(&Stage::ClipboardDetected));
        assert_eq!(seq.last(), Some(&Stage::Done));
        // The AWDL gate is on the real path.
        assert!(seq.contains(&Stage::AwdlBringUp));
        assert!(seq.contains(&Stage::Pull));
    }

    #[test]
    fn awdl_bring_up_is_gated_today() {
        let err = bring_up_awdl().unwrap_err();
        assert!(err.to_string().contains("AWDL"));
    }

    #[tokio::test]
    async fn loopback_wiring_reaches_a_copy_plan() {
        // Exercise the scan-trigger -> [AWDL skipped] -> pull tail against the
        // mock, then confirm clipboard_out can plan a copy from what came back.
        // Uses plan_copy (pure) rather than copy_pasteboard so the test never
        // spawns wl-copy or touches a real clipboard.
        let (_peer, board) = companion_client::loopback_demo().await.unwrap();
        let plan = clipboard_out::plan_copy(&board).expect("loopback board has an item");
        assert_eq!(plan.kind, PlanKind::Text);
        assert_eq!(plan.mime, None);
    }
}
