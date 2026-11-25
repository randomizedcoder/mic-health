//! Look up a USB device's kernel address (`busnum`, `devnum`) by vendor + product + serial.
//! Everything read here is world-readable; the probe needs no privilege.

use mic_health_common::{config::Mic, gate::UsbAddr};
use std::fs;
use std::path::Path;

pub const USB_DEVICES: &str = "/sys/bus/usb/devices";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    Found(UsbAddr),
    NotFound,
    /// More than one device matches; refuse to guess.
    Ambiguous,
}

fn attr(dir: &Path, name: &str) -> Option<String> {
    fs::read_to_string(dir.join(name))
        .ok()
        .map(|s| s.trim_end_matches('\n').to_owned())
}

pub fn lookup(root: &Path, mic: &Mic) -> Lookup {
    let Ok(entries) = fs::read_dir(root) else {
        return Lookup::NotFound;
    };
    let mut found: Option<UsbAddr> = None;
    for e in entries.flatten() {
        let dir = e.path();
        if attr(&dir, "idVendor").as_deref() != Some(&mic.vendor)
            || attr(&dir, "idProduct").as_deref() != Some(&mic.product)
            || attr(&dir, "serial").as_deref() != Some(&mic.serial)
        {
            continue;
        }
        let (Some(bus), Some(dev)) = (
            attr(&dir, "busnum").and_then(|s| s.parse().ok()),
            attr(&dir, "devnum").and_then(|s| s.parse().ok()),
        ) else {
            continue;
        };
        if found.is_some() {
            return Lookup::Ambiguous;
        }
        found = Some((bus, dev));
    }
    found.map_or(Lookup::NotFound, Lookup::Found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct TempTree(PathBuf);
    impl TempTree {
        fn new(tag: &str) -> Self {
            let p =
                std::env::temp_dir().join(format!("mic-probe-test-{}-{tag}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            TempTree(p)
        }
        fn device(
            &self,
            name: &str,
            vendor: &str,
            product: &str,
            serial: Option<&str>,
            bus: &str,
            dev: &str,
        ) {
            let d = self.0.join(name);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("idVendor"), format!("{vendor}\n")).unwrap();
            fs::write(d.join("idProduct"), format!("{product}\n")).unwrap();
            if let Some(s) = serial {
                fs::write(d.join("serial"), format!("{s}\n")).unwrap();
            }
            fs::write(d.join("busnum"), format!("{bus}\n")).unwrap();
            fs::write(d.join("devnum"), format!("{dev}\n")).unwrap();
        }
    }
    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn mic() -> Mic {
        Mic {
            name: "blue".into(),
            vendor: "046d".into(),
            product: "0ab7".into(),
            serial: "LT_X".into(),
            source: "alsa_input.x".into(),
        }
    }

    #[test]
    fn lookup_table() {
        type Setup = fn(&TempTree);
        struct Case {
            description: &'static str,
            setup: Setup,
            expected: Lookup,
        }
        let cases: Vec<Case> = vec![
            Case {
                description: "exact match",
                setup: |t| t.device("5-1.3", "046d", "0ab7", Some("LT_X"), "5", "8"),
                expected: Lookup::Found((5, 8)),
            },
            Case {
                description: "match among other devices (hub, other mic)",
                setup: |t| {
                    t.device("5-1", "05e3", "0610", None, "5", "2");
                    t.device("7-2", "046d", "0acf", Some("NANO"), "7", "3");
                    t.device("5-1.3", "046d", "0ab7", Some("LT_X"), "5", "8");
                },
                expected: Lookup::Found((5, 8)),
            },
            Case {
                description: "serial differs by one character",
                setup: |t| t.device("5-1.3", "046d", "0ab7", Some("LT_Y"), "5", "8"),
                expected: Lookup::NotFound,
            },
            Case {
                description: "serial is a prefix only",
                setup: |t| t.device("5-1.3", "046d", "0ab7", Some("LT_X2"), "5", "8"),
                expected: Lookup::NotFound,
            },
            Case {
                description: "device without a serial attribute",
                setup: |t| t.device("5-1.3", "046d", "0ab7", None, "5", "8"),
                expected: Lookup::NotFound,
            },
            Case {
                description: "same serial, different product",
                setup: |t| t.device("5-1.3", "046d", "0ab8", Some("LT_X"), "5", "8"),
                expected: Lookup::NotFound,
            },
            Case {
                description: "two matching devices",
                setup: |t| {
                    t.device("5-1.3", "046d", "0ab7", Some("LT_X"), "5", "8");
                    t.device("6-1", "046d", "0ab7", Some("LT_X"), "6", "2");
                },
                expected: Lookup::Ambiguous,
            },
            Case {
                description: "unparseable devnum",
                setup: |t| t.device("5-1.3", "046d", "0ab7", Some("LT_X"), "5", "x"),
                expected: Lookup::NotFound,
            },
            Case {
                description: "empty tree",
                setup: |_| {},
                expected: Lookup::NotFound,
            },
        ];
        for (i, c) in cases.into_iter().enumerate() {
            let t = TempTree::new(&i.to_string());
            (c.setup)(&t);
            assert_eq!(lookup(&t.0, &mic()), c.expected, "{}", c.description);
        }
    }

    #[test]
    fn missing_root_is_not_found() {
        assert_eq!(
            lookup(Path::new("/nonexistent/mic-probe"), &mic()),
            Lookup::NotFound
        );
    }
}
