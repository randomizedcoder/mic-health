//! Request/response wire protocol between `mic-probe` and `mic-reset` (design: "Component 2").

/// Protocol version carried in byte 0 of every request.
pub const VERSION: u8 = 0x01;

/// A request is exactly `[VERSION, mic_index]`.
pub const REQUEST_LEN: usize = 2;

/// One-byte response from `mic-reset`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Status {
    /// Reset done and the device re-enumerated.
    ResetDone = 0x00,
    /// Refused by the rate limiter.
    RateLimited = 0x01,
    /// Wrong length, unknown version or out-of-range index.
    BadRequest = 0x02,
    /// Peer uid is not the configured user.
    Unauthorised = 0x03,
    /// No device, or more than one, matches the allowlist entry.
    DeviceNotFound = 0x04,
    /// Write failed, or the device did not come back.
    ResetFailed = 0x05,
    /// Dry-run mode: every check passed, nothing was written.
    DryRun = 0x06,
}

impl Status {
    /// Decode a response byte. Unknown values are `None`; callers treat them as failure.
    pub fn from_byte(b: u8) -> Option<Self> {
        Some(match b {
            0x00 => Self::ResetDone,
            0x01 => Self::RateLimited,
            0x02 => Self::BadRequest,
            0x03 => Self::Unauthorised,
            0x04 => Self::DeviceNotFound,
            0x05 => Self::ResetFailed,
            0x06 => Self::DryRun,
            _ => return None,
        })
    }

    /// Encode for the wire.
    pub fn as_byte(self) -> u8 {
        self as u8
    }
}

#[cfg(test)]
mod tests {
    use super::Status;

    #[test]
    fn status_from_byte() {
        struct Case {
            description: &'static str,
            input: u8,
            expected: Option<Status>,
        }
        let cases = [
            Case {
                description: "lowest code: reset done",
                input: 0x00,
                expected: Some(Status::ResetDone),
            },
            Case {
                description: "rate limited",
                input: 0x01,
                expected: Some(Status::RateLimited),
            },
            Case {
                description: "bad request",
                input: 0x02,
                expected: Some(Status::BadRequest),
            },
            Case {
                description: "unauthorised peer",
                input: 0x03,
                expected: Some(Status::Unauthorised),
            },
            Case {
                description: "device not found",
                input: 0x04,
                expected: Some(Status::DeviceNotFound),
            },
            Case {
                description: "reset failed",
                input: 0x05,
                expected: Some(Status::ResetFailed),
            },
            Case {
                description: "highest code: dry run",
                input: 0x06,
                expected: Some(Status::DryRun),
            },
            Case {
                description: "first unknown code (boundary)",
                input: 0x07,
                expected: None,
            },
            Case {
                description: "max byte is unknown",
                input: 0xff,
                expected: None,
            },
        ];
        for c in cases {
            assert_eq!(Status::from_byte(c.input), c.expected, "{}", c.description);
        }
    }

    #[test]
    fn status_round_trips() {
        for b in 0u8..=0xff {
            if let Some(s) = Status::from_byte(b) {
                assert_eq!(s.as_byte(), b, "round trip of {b:#04x}");
            }
        }
    }
}
