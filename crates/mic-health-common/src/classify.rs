//! Decide whether a capture is live audio, the all-zero fault, or unusable (design: step 4).
//!
//! A working ADC always shows some noise. Exact digital zero over a full window means either the
//! fault or a hardware mute. Telling those two apart is the job of `gate`, not this module.

/// Capture format the probe requests from `pw-record`: s16le, 48 kHz, stereo.
pub const RATE: usize = 48_000;
pub const CHANNELS: usize = 2;
pub const BYTES_PER_SAMPLE: usize = 2;
pub const BYTES_PER_FRAME: usize = CHANNELS * BYTES_PER_SAMPLE;

/// Frames discarded from the start: stream startup can legitimately be zero (250 ms).
pub const DISCARD_FRAMES: usize = RATE / 4;
/// Minimum frames that must remain after the discard for a verdict (500 ms).
pub const MIN_FRAMES: usize = RATE / 2;
/// Bytes the probe reads: 1 s of audio.
pub const CAPTURE_BYTES: usize = RATE * BYTES_PER_FRAME;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Verdict {
    /// Non-zero audio. Levels are dBFS of the analysed window.
    Ok { peak_dbfs: f64, rms_dbfs: f64 },
    /// Every analysed sample is exactly zero.
    Dead,
    /// Too little data, or not whole samples. Never treated as dead.
    Error,
}

/// Classify raw interleaved s16le bytes as produced by `pw-record --raw --format s16`.
pub fn classify(bytes: &[u8]) -> Verdict {
    if bytes.len() % BYTES_PER_SAMPLE != 0 {
        return Verdict::Error;
    }
    // Ignore a trailing partial frame; only whole frames are analysed.
    let frames = bytes.len() / BYTES_PER_FRAME;
    if frames < DISCARD_FRAMES + MIN_FRAMES {
        return Verdict::Error;
    }
    let window = &bytes[DISCARD_FRAMES * BYTES_PER_FRAME..frames * BYTES_PER_FRAME];

    let mut peak: u32 = 0;
    let mut sum_sq: f64 = 0.0;
    let mut n: usize = 0;
    for pair in window.chunks_exact(BYTES_PER_SAMPLE) {
        let s = i16::from_le_bytes([pair[0], pair[1]]);
        peak = peak.max(s.unsigned_abs() as u32);
        sum_sq += f64::from(s) * f64::from(s);
        n += 1;
    }
    if peak == 0 {
        return Verdict::Dead;
    }
    let full_scale = 32768.0_f64;
    Verdict::Ok {
        peak_dbfs: 20.0 * (f64::from(peak) / full_scale).log10(),
        rms_dbfs: 20.0 * ((sum_sq / n as f64).sqrt() / full_scale).log10(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build `frames` stereo frames where both channels get `f(i)`.
    fn frames(frames: usize, f: impl Fn(usize) -> i16) -> Vec<u8> {
        let mut v = Vec::with_capacity(frames * BYTES_PER_FRAME);
        for i in 0..frames {
            let b = f(i).to_le_bytes();
            v.extend_from_slice(&b);
            v.extend_from_slice(&b);
        }
        v
    }

    #[derive(Debug, PartialEq)]
    enum Kind {
        Ok,
        Dead,
        Error,
    }

    fn kind(v: Verdict) -> Kind {
        match v {
            Verdict::Ok { .. } => Kind::Ok,
            Verdict::Dead => Kind::Dead,
            Verdict::Error => Kind::Error,
        }
    }

    #[test]
    fn classify_table() {
        let one_second = RATE;
        let boundary = DISCARD_FRAMES + MIN_FRAMES;
        // Deterministic pseudo-noise, roughly -50 dBFS.
        let noise = |i: usize| (((i * 7919) % 201) as i16) - 100;

        struct Case {
            description: &'static str,
            input: Vec<u8>,
            expected: Kind,
        }
        let cases = vec![
            Case {
                description: "all zeros for 1 s",
                input: frames(one_second, |_| 0),
                expected: Kind::Dead,
            },
            Case {
                description: "single +1 LSB sample in the window",
                input: frames(one_second, |i| if i == one_second - 1 { 1 } else { 0 }),
                expected: Kind::Ok,
            },
            Case {
                description: "single -1 LSB sample in the window",
                input: frames(one_second, |i| if i == DISCARD_FRAMES { -1 } else { 0 }),
                expected: Kind::Ok,
            },
            Case {
                description: "typical room noise",
                input: frames(one_second, noise),
                expected: Kind::Ok,
            },
            Case {
                description: "full-scale clipping (i16::MIN)",
                input: frames(one_second, |_| i16::MIN),
                expected: Kind::Ok,
            },
            Case {
                description: "zeros only in the discarded first 250 ms",
                input: frames(
                    one_second,
                    |i| if i < DISCARD_FRAMES { 0 } else { noise(i) },
                ),
                expected: Kind::Ok,
            },
            Case {
                description: "signal only in the discarded first 250 ms",
                input: frames(one_second, |i| if i < DISCARD_FRAMES { 1000 } else { 0 }),
                expected: Kind::Dead,
            },
            Case {
                description: "exactly the minimum frames, all zero (boundary)",
                input: frames(boundary, |_| 0),
                expected: Kind::Dead,
            },
            Case {
                description: "one frame short of the minimum",
                input: frames(boundary - 1, |_| 0),
                expected: Kind::Error,
            },
            Case {
                description: "empty buffer",
                input: Vec::new(),
                expected: Kind::Error,
            },
            Case {
                description: "odd byte count",
                input: {
                    let mut v = frames(one_second, |_| 0);
                    v.push(0);
                    v
                },
                expected: Kind::Error,
            },
            Case {
                description: "trailing half frame is ignored",
                input: {
                    let mut v = frames(one_second, |_| 0);
                    v.extend_from_slice(&[0, 0]);
                    v
                },
                expected: Kind::Dead,
            },
        ];
        for c in cases {
            assert_eq!(kind(classify(&c.input)), c.expected, "{}", c.description);
        }
    }

    #[test]
    fn levels_are_sane() {
        struct Case {
            description: &'static str,
            value: i16,
            expected_peak_dbfs: f64,
        }
        let cases = [
            Case {
                description: "full scale",
                value: i16::MIN,
                expected_peak_dbfs: 0.0,
            },
            Case {
                description: "half scale",
                value: 16384,
                expected_peak_dbfs: -6.0206,
            },
            Case {
                description: "1 LSB",
                value: 1,
                expected_peak_dbfs: -90.309,
            },
        ];
        for c in cases {
            match classify(&frames(RATE, |_| c.value)) {
                Verdict::Ok {
                    peak_dbfs,
                    rms_dbfs,
                } => {
                    assert!(
                        (peak_dbfs - c.expected_peak_dbfs).abs() < 0.01,
                        "{}: peak {peak_dbfs}",
                        c.description
                    );
                    // A constant signal has RMS == peak.
                    assert!(
                        (rms_dbfs - peak_dbfs).abs() < 0.01,
                        "{}: rms {rms_dbfs}",
                        c.description
                    );
                }
                other => panic!("{}: expected Ok, got {other:?}", c.description),
            }
        }
    }
}
