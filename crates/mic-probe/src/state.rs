//! Persisted probe state: per-mic gate state plus the last notice sent (for de-duplication).

use mic_health_common::gate::MicState;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;

/// What the user was last told about a mic. Notices are sent only on change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Notice {
    Absent,
    Uncheckable,
    LikelyMuted,
    NoBaseline,
    ResetWarranted,
    StillDeadAfterReset,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    #[serde(flatten)]
    pub gate: MicState,
    pub notice: Option<Notice>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeState {
    pub mics: BTreeMap<String, Entry>,
}

impl ProbeState {
    /// A missing or corrupt file starts fresh: the probe only ever notifies, so losing state is
    /// safe (the gate then reports `NoBaseline` rather than warranting a reset).
    pub fn load(path: &Path) -> Self {
        match fs::read(path) {
            Ok(b) => serde_json::from_slice(&b).unwrap_or_else(|e| {
                eprintln!("state {}: unreadable ({e}); starting fresh", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// Atomic write: temp file in the same directory, fsync, rename.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let dir = path.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(dir)?;
        let tmp = dir.join(".probe-state.json.tmp");
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(&serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_table() {
        struct Case {
            description: &'static str,
            content: Option<&'static [u8]>,
            expected_mics: usize,
        }
        let cases = [
            Case { description: "missing file", content: None, expected_mics: 0 },
            Case { description: "corrupt JSON", content: Some(b"{not json"), expected_mics: 0 },
            Case { description: "empty object", content: Some(b"{}"), expected_mics: 0 },
            Case {
                description: "one mic, older file without reset_requested_addr",
                content: Some(br#"{"mics":{"blue":{"last_ok_addr":[5,8],"dead_streak":0,"skip_streak":0,"notice":null}}}"#),
                expected_mics: 1,
            },
        ];
        for (i, c) in cases.iter().enumerate() {
            let dir =
                std::env::temp_dir().join(format!("mic-probe-state-{}-{i}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            let p = dir.join("probe-state.json");
            if let Some(b) = c.content {
                fs::write(&p, b).unwrap();
            }
            assert_eq!(
                ProbeState::load(&p).mics.len(),
                c.expected_mics,
                "{}",
                c.description
            );
            let _ = fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = std::env::temp_dir().join(format!("mic-probe-state-rt-{}", std::process::id()));
        let p = dir.join("probe-state.json");
        let mut st = ProbeState::default();
        let mut e = Entry::default();
        e.gate.last_ok_addr = Some((5, 8));
        e.gate.reset_requested_addr = Some((5, 9));
        e.notice = Some(Notice::LikelyMuted);
        st.mics.insert("blue".into(), e);
        st.save(&p).unwrap();
        assert_eq!(ProbeState::load(&p), st);
        let _ = fs::remove_dir_all(&dir);
    }
}
