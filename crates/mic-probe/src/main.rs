//! `mic-probe`: unprivileged detector. See docs/design.md, Component 1.
//!
//! Phase 1 (alert-only): probes idle mics, applies the re-enumeration gate and notifies the
//! user. It never contacts the actuator.

mod capture;
mod pipewire;
mod state;
mod sysfs;

use mic_health_common::classify::{classify, Verdict};
use mic_health_common::config::{Config, Mic, Mode};
use mic_health_common::gate::{Decision, Observation};
use pipewire::SourceState;
use state::{Notice, ProbeState};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

/// Pause between re-probes of a dead mic (design: 1 min, then 2 min).
const RECHECK_GAP: Duration = Duration::from_secs(60);
const NOTIFY_TIMEOUT: Duration = Duration::from_secs(5);

/// What to tell the user, if anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Message {
    Recovered,
    Problem(Notice),
}

/// Map a gate decision onto the stored notice. Returns the new notice, and a message only when
/// it changed, so a mic stuck in one state notifies once rather than every probe.
fn transition(prev: Option<Notice>, d: Decision) -> (Option<Notice>, Option<Message>) {
    let next = match d {
        Decision::Healthy => None,
        // No new information: keep whatever the user was last told.
        Decision::Skipped | Decision::Inconclusive | Decision::Recheck => return (prev, None),
        Decision::Uncheckable => Some(Notice::Uncheckable),
        Decision::Absent => Some(Notice::Absent),
        Decision::LikelyMuted => Some(Notice::LikelyMuted),
        Decision::NoBaseline => Some(Notice::NoBaseline),
        Decision::ResetWarranted => Some(Notice::ResetWarranted),
        Decision::StillDeadAfterReset => Some(Notice::StillDeadAfterReset),
    };
    let msg = match (prev, next) {
        (a, b) if a == b => None,
        (_, Some(n)) => Some(Message::Problem(n)),
        (Some(_), None) => Some(Message::Recovered),
        (None, None) => None,
    };
    (next, msg)
}

fn text(m: Message) -> &'static str {
    match m {
        Message::Recovered => "Working again.",
        Message::Problem(Notice::Absent) => "Not present (unplugged, or dropped off USB).",
        Message::Problem(Notice::Uncheckable) => "In use for about 2 hours; can't check it.",
        Message::Problem(Notice::LikelyMuted) => {
            "Silent (exact zeros) at the same USB address it last worked at. Likely hardware-muted: check the mute button."
        }
        Message::Problem(Notice::NoBaseline) => {
            "Silent (exact zeros), and it hasn't worked since tracking started. Check mute, or replug."
        }
        Message::Problem(Notice::ResetWarranted) => {
            "Silent (exact zeros) after a USB re-enumeration: the known fault. Alert-only mode, so no reset: replug it."
        }
        Message::Problem(Notice::StillDeadAfterReset) => "Still silent after the reset/alert. Replug it.",
    }
}

fn observe(cfg: &Config, mic: &Mic) -> Observation {
    let dump = match capture::pw_dump(&cfg.tools.pw_dump) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("<4>{}: pw-dump failed: {e}", mic.name);
            return Observation::Error;
        }
    };
    match pipewire::source_state(&dump, &mic.source) {
        SourceState::Absent => return Observation::Absent,
        SourceState::Busy => return Observation::Busy,
        SourceState::Idle => {}
    }
    let addr = match sysfs::lookup(Path::new(sysfs::USB_DEVICES), mic) {
        sysfs::Lookup::Found(a) => a,
        sysfs::Lookup::NotFound => return Observation::Absent,
        sysfs::Lookup::Ambiguous => {
            eprintln!(
                "<3>{}: more than one USB device matches; refusing to guess",
                mic.name
            );
            return Observation::Error;
        }
    };
    let bytes = match capture::record(&cfg.tools.pw_record, &mic.source) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("<4>{}: capture failed: {e}", mic.name);
            return Observation::Error;
        }
    };
    match classify(&bytes) {
        Verdict::Ok {
            peak_dbfs,
            rms_dbfs,
        } => {
            eprintln!(
                "<6>{}: ok, peak {peak_dbfs:.1} dBFS, rms {rms_dbfs:.1} dBFS, usb {}:{}",
                mic.name, addr.0, addr.1
            );
            Observation::Ok(addr)
        }
        Verdict::Dead => {
            eprintln!(
                "<5>{}: all-zero capture, usb {}:{}",
                mic.name, addr.0, addr.1
            );
            Observation::Dead(addr)
        }
        Verdict::Error => {
            eprintln!("<4>{}: short capture ({} bytes)", mic.name, bytes.len());
            Observation::Error
        }
    }
}

fn notify(tool: &Path, summary: &str, body: &str) {
    let child = Command::new(tool)
        .args([
            "--app-name=mic-health",
            "--icon=audio-input-microphone",
            "--",
            summary,
            body,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        eprintln!("<4>notify-send: spawn failed");
        return;
    };
    let start = Instant::now();
    while start.elapsed() < NOTIFY_TIMEOUT {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    eprintln!("<4>notify-send: timed out");
}

fn probe_mic(cfg: &Config, mic: &Mic, st: &mut state::Entry) {
    let decision = loop {
        let obs = observe(cfg, mic);
        let d = st.gate.step(obs);
        if d != Decision::Recheck {
            if let (Decision::ResetWarranted, Observation::Dead(addr)) = (d, obs) {
                // Alert-only: count the alert as the request so it isn't repeated for this address.
                st.gate.note_reset_requested(addr);
            }
            break d;
        }
        eprintln!(
            "<6>{}: dead streak {}, re-probing in {}s",
            mic.name,
            st.gate.dead_streak,
            RECHECK_GAP.as_secs()
        );
        std::thread::sleep(RECHECK_GAP);
    };
    eprintln!("<6>{}: decision {decision:?}", mic.name);
    let (next, msg) = transition(st.notice, decision);
    st.notice = next;
    if let Some(m) = msg {
        let body = text(m);
        eprintln!("<5>{}: notify: {body}", mic.name);
        notify(
            &cfg.tools.notify_send,
            &format!("Microphone {}", mic.name),
            body,
        );
    }
}

fn state_path() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("STATE_DIRECTORY") {
        // systemd may pass several, colon-separated; ours is the first.
        let first = d.to_str()?.split(':').next()?.to_owned();
        return Some(PathBuf::from(first).join("probe-state.json"));
    }
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(base.join("mic-health/probe-state.json"))
}

fn load_config(path: &str) -> Result<Config, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    Config::from_json(&text).map_err(|e| e.to_string())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = match args.as_slice() {
        [flag, path] if flag == "--config" => path,
        _ => {
            eprintln!("usage: mic-probe --config <file.json>");
            return ExitCode::from(2);
        }
    };
    let cfg = match load_config(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("<3>{path}: {e}");
            return ExitCode::from(2);
        }
    };
    if cfg.mode != Mode::AlertOnly {
        eprintln!(
            "<4>mode {:?} is not implemented in this build; running alert-only",
            cfg.mode
        );
    }
    let Some(sp) = state_path() else {
        eprintln!("<3>no STATE_DIRECTORY, XDG_STATE_HOME or HOME");
        return ExitCode::from(2);
    };
    let mut st = ProbeState::load(&sp);
    // Forget mics that are no longer configured.
    st.mics
        .retain(|name, _| cfg.mics.iter().any(|m| &m.name == name));
    for mic in &cfg.mics {
        let entry = st.mics.entry(mic.name.clone()).or_default();
        probe_mic(&cfg, mic, entry);
        // Save after each mic so a crash or timeout keeps what was learnt.
        if let Err(e) = st.save(&sp) {
            eprintln!("<3>state {}: save failed: {e}", sp.display());
        }
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use Decision as D;
    use Notice as N;

    #[test]
    fn transition_table() {
        struct Case {
            description: &'static str,
            prev: Option<Notice>,
            decision: Decision,
            expected: (Option<Notice>, Option<Message>),
        }
        let cases = [
            Case {
                description: "healthy stays quiet",
                prev: None,
                decision: D::Healthy,
                expected: (None, None),
            },
            Case {
                description: "healthy after a problem -> recovered",
                prev: Some(N::LikelyMuted),
                decision: D::Healthy,
                expected: (None, Some(Message::Recovered)),
            },
            Case {
                description: "first mute -> notify",
                prev: None,
                decision: D::LikelyMuted,
                expected: (Some(N::LikelyMuted), Some(Message::Problem(N::LikelyMuted))),
            },
            Case {
                description: "mute again -> no repeat",
                prev: Some(N::LikelyMuted),
                decision: D::LikelyMuted,
                expected: (Some(N::LikelyMuted), None),
            },
            Case {
                description: "reset warranted, then still dead -> new message",
                prev: Some(N::ResetWarranted),
                decision: D::StillDeadAfterReset,
                expected: (
                    Some(N::StillDeadAfterReset),
                    Some(Message::Problem(N::StillDeadAfterReset)),
                ),
            },
            Case {
                description: "skip keeps the previous notice",
                prev: Some(N::Absent),
                decision: D::Skipped,
                expected: (Some(N::Absent), None),
            },
            Case {
                description: "inconclusive keeps the previous notice",
                prev: Some(N::LikelyMuted),
                decision: D::Inconclusive,
                expected: (Some(N::LikelyMuted), None),
            },
            Case {
                description: "recheck says nothing",
                prev: None,
                decision: D::Recheck,
                expected: (None, None),
            },
            Case {
                description: "unplugged -> absent",
                prev: None,
                decision: D::Absent,
                expected: (Some(N::Absent), Some(Message::Problem(N::Absent))),
            },
            Case {
                description: "uncheckable reported",
                prev: None,
                decision: D::Uncheckable,
                expected: (Some(N::Uncheckable), Some(Message::Problem(N::Uncheckable))),
            },
        ];
        for c in cases {
            assert_eq!(
                transition(c.prev, c.decision),
                c.expected,
                "{}",
                c.description
            );
        }
    }
}
