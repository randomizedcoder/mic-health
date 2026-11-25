//! Run `pw-dump` and `pw-record` with fixed arguments (no shell). Audio stays in memory.

use mic_health_common::classify::{CAPTURE_BYTES, CHANNELS, RATE};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const PW_DUMP_TIMEOUT: Duration = Duration::from_secs(5);
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(5);

/// Run a command, return stdout, kill it if it exceeds `timeout` or produces more than `max` bytes.
fn run_bounded(cmd: &mut Command, max: usize, timeout: Duration) -> Result<Vec<u8>, String> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn {:?}: {e}", cmd.get_program()))?;
    let mut out = child.stdout.take().ok_or("no stdout")?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = Vec::new();
        // Read at most `max` bytes; dropping `out` afterwards unblocks a writer via SIGPIPE.
        let r = (&mut out)
            .take(max as u64)
            .read_to_end(&mut buf)
            .map(|_| buf);
        let _ = tx.send(r);
    });
    let result = rx.recv_timeout(timeout);
    let _ = child.kill();
    let _ = child.wait();
    match result {
        Ok(Ok(buf)) => Ok(buf),
        Ok(Err(e)) => Err(format!("read: {e}")),
        Err(_) => Err("timed out".into()),
    }
}

pub fn pw_dump(pw_dump: &Path) -> Result<serde_json::Value, String> {
    // pw-dump of a busy desktop is a few hundred KB; 16 MB is a generous cap.
    let bytes = run_bounded(
        Command::new(pw_dump).arg("--no-colors"),
        16 << 20,
        PW_DUMP_TIMEOUT,
    )?;
    serde_json::from_slice(&bytes).map_err(|e| format!("pw-dump JSON: {e}"))
}

/// Capture `CAPTURE_BYTES` of raw s16le stereo 48 kHz from `source`.
pub fn record(pw_record: &Path, source: &str) -> Result<Vec<u8>, String> {
    let rate = RATE.to_string();
    let channels = CHANNELS.to_string();
    run_bounded(
        Command::new(pw_record).args([
            "--raw",
            "--target",
            source,
            "--format",
            "s16",
            "--rate",
            &rate,
            "--channels",
            &channels,
            "-",
        ]),
        CAPTURE_BYTES,
        CAPTURE_TIMEOUT,
    )
}
