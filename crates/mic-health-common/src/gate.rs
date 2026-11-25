//! Per-mic state machine: the re-enumeration gate (design: Component 1, step 5).
//!
//! Hardware mute on these mics yields exact digital zero, the same as the fault. The fault is
//! preceded by a USB re-enumeration (new kernel `devnum`); a mute is not. So a dead mic warrants a
//! reset only if its USB address changed since it last produced audio. A software reset
//! (`authorized` 0 -> 1) keeps the address, so the gate also stops repeat resets.

use serde::{Deserialize, Serialize};

/// Dead results in a row needed before acting.
pub const DEAD_STREAK_REQUIRED: u32 = 3;
/// Consecutive "in use" skips before reporting that the mic cannot be checked.
pub const SKIPS_BEFORE_UNCHECKABLE: u32 = 12;

/// Kernel USB address: `(busnum, devnum)`. A new enumeration gets a new devnum.
pub type UsbAddr = (u32, u32);

/// One probe's raw observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Observation {
    /// PipeWire node or USB device missing.
    Absent,
    /// Another client holds the mic; not probed.
    Busy,
    /// Capture failed or too short; no conclusion.
    Error,
    Ok(UsbAddr),
    Dead(UsbAddr),
}

/// What the probe should do about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Healthy,
    Skipped,
    /// Skipped for too long; tell the user once.
    Uncheckable,
    Inconclusive,
    Absent,
    /// Dead, but not enough in a row yet: re-probe soon.
    Recheck,
    /// Dead with the same USB address it last worked at: most likely hardware-muted.
    LikelyMuted,
    /// Dead with no record of it ever working (fresh state): can't apply the gate.
    NoBaseline,
    /// Dead and re-enumerated since it last worked: matches the fault.
    ResetWarranted,
    /// Still dead at the address a reset was already requested for: don't ask again.
    StillDeadAfterReset,
}

/// Persisted per-mic state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MicState {
    pub last_ok_addr: Option<UsbAddr>,
    pub dead_streak: u32,
    pub skip_streak: u32,
    /// Address a reset was last requested for. A software reset keeps the address, so this
    /// stops repeat requests; a new enumeration (new address) re-opens the gate.
    #[serde(default)]
    pub reset_requested_addr: Option<UsbAddr>,
}

impl MicState {
    /// Record that a reset was requested (or, in alert-only mode, reported) for `addr`.
    /// The streak restarts: after a reset the mic needs three fresh dead probes again.
    pub fn note_reset_requested(&mut self, addr: UsbAddr) {
        self.reset_requested_addr = Some(addr);
        self.dead_streak = 0;
    }

    pub fn step(&mut self, obs: Observation) -> Decision {
        match obs {
            Observation::Absent => {
                self.dead_streak = 0;
                self.skip_streak = 0;
                Decision::Absent
            }
            Observation::Busy => {
                self.dead_streak = 0;
                self.skip_streak = self.skip_streak.saturating_add(1);
                if self.skip_streak == SKIPS_BEFORE_UNCHECKABLE {
                    Decision::Uncheckable
                } else {
                    Decision::Skipped
                }
            }
            Observation::Error => Decision::Inconclusive,
            Observation::Ok(addr) => {
                self.last_ok_addr = Some(addr);
                self.reset_requested_addr = None;
                self.dead_streak = 0;
                self.skip_streak = 0;
                Decision::Healthy
            }
            Observation::Dead(addr) => {
                self.skip_streak = 0;
                self.dead_streak = self.dead_streak.saturating_add(1);
                if self.dead_streak < DEAD_STREAK_REQUIRED {
                    return Decision::Recheck;
                }
                if self.reset_requested_addr == Some(addr) {
                    return Decision::StillDeadAfterReset;
                }
                match self.last_ok_addr {
                    None => Decision::NoBaseline,
                    Some(last) if last != addr => Decision::ResetWarranted,
                    Some(_) => Decision::LikelyMuted,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Observation::*;

    const A7: UsbAddr = (5, 7);
    const A8: UsbAddr = (5, 8);
    const OTHER_BUS: UsbAddr = (6, 7);

    #[test]
    fn gate_table() {
        struct Case {
            description: &'static str,
            start: MicState,
            inputs: Vec<Observation>,
            expected: Vec<Decision>,
        }
        let worked_at = |a| MicState {
            last_ok_addr: Some(a),
            ..Default::default()
        };
        let cases = vec![
            Case {
                description: "3x dead after re-enumeration -> reset",
                start: worked_at(A7),
                inputs: vec![Dead(A8), Dead(A8), Dead(A8)],
                expected: vec![
                    Decision::Recheck,
                    Decision::Recheck,
                    Decision::ResetWarranted,
                ],
            },
            Case {
                description: "3x dead at same address -> likely muted, never reset",
                start: worked_at(A8),
                inputs: vec![Dead(A8), Dead(A8), Dead(A8), Dead(A8)],
                expected: vec![
                    Decision::Recheck,
                    Decision::Recheck,
                    Decision::LikelyMuted,
                    Decision::LikelyMuted,
                ],
            },
            Case {
                description: "same devnum on another bus counts as re-enumerated",
                start: worked_at(A7),
                inputs: vec![Dead(OTHER_BUS), Dead(OTHER_BUS), Dead(OTHER_BUS)],
                expected: vec![
                    Decision::Recheck,
                    Decision::Recheck,
                    Decision::ResetWarranted,
                ],
            },
            Case {
                description: "3x dead with no baseline -> notify only",
                start: MicState::default(),
                inputs: vec![Dead(A8), Dead(A8), Dead(A8)],
                expected: vec![Decision::Recheck, Decision::Recheck, Decision::NoBaseline],
            },
            Case {
                description: "2x dead is not enough (boundary)",
                start: worked_at(A7),
                inputs: vec![Dead(A8), Dead(A8)],
                expected: vec![Decision::Recheck, Decision::Recheck],
            },
            Case {
                description: "ok in the middle resets the streak and moves the baseline",
                start: worked_at(A7),
                inputs: vec![Dead(A8), Dead(A8), Ok(A8), Dead(A8), Dead(A8), Dead(A8)],
                expected: vec![
                    Decision::Recheck,
                    Decision::Recheck,
                    Decision::Healthy,
                    Decision::Recheck,
                    Decision::Recheck,
                    Decision::LikelyMuted,
                ],
            },
            Case {
                description: "busy resets the dead streak",
                start: worked_at(A7),
                inputs: vec![Dead(A8), Dead(A8), Busy, Dead(A8)],
                expected: vec![
                    Decision::Recheck,
                    Decision::Recheck,
                    Decision::Skipped,
                    Decision::Recheck,
                ],
            },
            Case {
                description: "error does not reset the dead streak",
                start: worked_at(A7),
                inputs: vec![Dead(A8), Error, Dead(A8), Dead(A8)],
                expected: vec![
                    Decision::Recheck,
                    Decision::Inconclusive,
                    Decision::Recheck,
                    Decision::ResetWarranted,
                ],
            },
            Case {
                description: "absent resets the dead streak",
                start: worked_at(A7),
                inputs: vec![Dead(A8), Dead(A8), Absent, Dead(A8)],
                expected: vec![
                    Decision::Recheck,
                    Decision::Recheck,
                    Decision::Absent,
                    Decision::Recheck,
                ],
            },
        ];
        for c in cases {
            let mut st = c.start.clone();
            let got: Vec<_> = c.inputs.iter().map(|o| st.step(*o)).collect();
            assert_eq!(got, c.expected, "{}", c.description);
        }
    }

    #[test]
    fn reset_gate_closes_after_request() {
        struct Case {
            description: &'static str,
            after_reset: Vec<Observation>,
            expected: Vec<Decision>,
        }
        let cases = vec![
            Case {
                description: "still dead at the same address -> no second request",
                after_reset: vec![Dead(A8), Dead(A8), Dead(A8), Dead(A8)],
                expected: vec![
                    Decision::Recheck,
                    Decision::Recheck,
                    Decision::StillDeadAfterReset,
                    Decision::StillDeadAfterReset,
                ],
            },
            Case {
                description: "re-enumerated again (new address) -> gate re-opens",
                after_reset: vec![Dead(OTHER_BUS), Dead(OTHER_BUS), Dead(OTHER_BUS)],
                expected: vec![
                    Decision::Recheck,
                    Decision::Recheck,
                    Decision::ResetWarranted,
                ],
            },
            Case {
                description: "recovers, then mute at the same address -> likely muted",
                after_reset: vec![Ok(A8), Dead(A8), Dead(A8), Dead(A8)],
                expected: vec![
                    Decision::Healthy,
                    Decision::Recheck,
                    Decision::Recheck,
                    Decision::LikelyMuted,
                ],
            },
        ];
        for c in cases {
            let mut st = MicState {
                last_ok_addr: Some(A7),
                ..Default::default()
            };
            for _ in 0..2 {
                st.step(Dead(A8));
            }
            assert_eq!(
                st.step(Dead(A8)),
                Decision::ResetWarranted,
                "{}: setup",
                c.description
            );
            st.note_reset_requested(A8);
            let got: Vec<_> = c.after_reset.iter().map(|o| st.step(*o)).collect();
            assert_eq!(got, c.expected, "{}", c.description);
        }
    }

    #[test]
    fn uncheckable_reported_once() {
        let mut st = MicState::default();
        let got: Vec<_> = (0..SKIPS_BEFORE_UNCHECKABLE + 2)
            .map(|_| st.step(Busy))
            .collect();
        let n = got.iter().filter(|d| **d == Decision::Uncheckable).count();
        assert_eq!(n, 1, "exactly one Uncheckable in {got:?}");
        assert_eq!(
            got[(SKIPS_BEFORE_UNCHECKABLE - 1) as usize],
            Decision::Uncheckable,
            "on the 12th skip"
        );
    }
}
