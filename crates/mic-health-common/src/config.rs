//! Configuration shared by both binaries. Rendered to JSON by the NixOS module; the list index
//! of `mics` is the wire-protocol device index.

use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    AlertOnly,
    DryRun,
    Enforce,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mic {
    pub name: String,
    pub vendor: String,
    pub product: String,
    pub serial: String,
    pub source: String,
}

/// Absolute paths to external tools, baked in by Nix so nothing depends on `$PATH`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tools {
    pub pw_dump: PathBuf,
    pub pw_record: PathBuf,
    pub notify_send: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub mode: Mode,
    pub mics: Vec<Mic>,
    pub tools: Tools,
}

#[derive(Debug)]
pub enum ConfigError {
    Parse(String),
    Invalid(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Parse(e) => write!(f, "config parse error: {e}"),
            ConfigError::Invalid(e) => write!(f, "invalid config: {e}"),
        }
    }
}

impl std::error::Error for ConfigError {}

fn is_hex4(s: &str) -> bool {
    s.len() == 4
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Config {
    pub fn from_json(text: &str) -> Result<Self, ConfigError> {
        let cfg: Config =
            serde_json::from_str(text).map_err(|e| ConfigError::Parse(e.to_string()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        let bad = |m: String| Err(ConfigError::Invalid(m));
        if self.mics.is_empty() || self.mics.len() > 256 {
            return bad(format!(
                "mics must have 1-256 entries, got {}",
                self.mics.len()
            ));
        }
        for (i, m) in self.mics.iter().enumerate() {
            if m.name.is_empty()
                || !m
                    .name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            {
                return bad(format!("mics[{i}].name must match [a-z0-9-]+"));
            }
            if !is_hex4(&m.vendor) || !is_hex4(&m.product) {
                return bad(format!(
                    "mics[{i}] vendor/product must be 4 lowercase hex digits"
                ));
            }
            if m.serial.is_empty() || m.serial.contains(['/', '\n', '\0']) {
                return bad(format!(
                    "mics[{i}].serial is empty or contains forbidden characters"
                ));
            }
            if m.source.is_empty() || m.source.starts_with('-') {
                return bad(format!("mics[{i}].source is empty or starts with '-'"));
            }
            if self.mics[..i].iter().any(|o| o.name == m.name) {
                return bad(format!("mics[{i}].name {:?} is duplicated", m.name));
            }
        }
        for (what, p) in [
            ("pw_dump", &self.tools.pw_dump),
            ("pw_record", &self.tools.pw_record),
            ("notify_send", &self.tools.notify_send),
        ] {
            if !p.is_absolute() {
                return bad(format!("tools.{what} must be an absolute path"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(mics: &str, mode: &str) -> String {
        format!(
            r#"{{"mode":"{mode}","mics":[{mics}],"tools":{{"pw_dump":"/x/pw-dump","pw_record":"/x/pw-record","notify_send":"/x/notify-send"}}}}"#
        )
    }
    const BLUE: &str =
        r#"{"name":"blue","vendor":"046d","product":"0ab7","serial":"S1","source":"alsa_input.x"}"#;

    #[test]
    fn config_table() {
        struct Case {
            description: &'static str,
            input: String,
            expected: Result<(), &'static str>,
        }
        let cases = vec![
            Case {
                description: "one valid mic",
                input: json(BLUE, "alert-only"),
                expected: Ok(()),
            },
            Case {
                description: "enforce mode parses",
                input: json(BLUE, "enforce"),
                expected: Ok(()),
            },
            Case {
                description: "unknown mode",
                input: json(BLUE, "yolo"),
                expected: Err("parse"),
            },
            Case {
                description: "no mics",
                input: json("", "alert-only"),
                expected: Err("1-256"),
            },
            Case {
                description: "uppercase hex vendor",
                input: json(&BLUE.replace("046d", "046D"), "alert-only"),
                expected: Err("hex"),
            },
            Case {
                description: "3-digit product",
                input: json(&BLUE.replace("0ab7", "ab7"), "alert-only"),
                expected: Err("hex"),
            },
            Case {
                description: "serial with a slash",
                input: json(&BLUE.replace("\"S1\"", "\"../x\""), "alert-only"),
                expected: Err("serial"),
            },
            Case {
                description: "source that looks like a flag",
                input: json(&BLUE.replace("alsa_input.x", "--help"), "alert-only"),
                expected: Err("source"),
            },
            Case {
                description: "name with uppercase",
                input: json(&BLUE.replace("\"blue\"", "\"Blue\""), "alert-only"),
                expected: Err("name"),
            },
            Case {
                description: "duplicate names",
                input: json(&format!("{BLUE},{BLUE}"), "alert-only"),
                expected: Err("duplicated"),
            },
            Case {
                description: "unknown field rejected",
                input: json(
                    &BLUE.replace("\"name\"", "\"extra\":1,\"name\""),
                    "alert-only",
                ),
                expected: Err("parse"),
            },
            Case {
                description: "relative tool path",
                input: json(BLUE, "alert-only").replace("/x/pw-dump", "pw-dump"),
                expected: Err("absolute"),
            },
        ];
        for c in cases {
            let got = Config::from_json(&c.input);
            match (got, c.expected) {
                (Ok(_), Ok(())) => {}
                (Err(e), Err(needle)) => {
                    assert!(
                        e.to_string().contains(needle),
                        "{}: error {e} lacks {needle:?}",
                        c.description
                    )
                }
                (got, want) => panic!("{}: got {got:?}, want {want:?}", c.description),
            }
        }
    }
}
