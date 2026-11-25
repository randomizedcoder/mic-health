//! Interpret `pw-dump` JSON: is the configured source present, and is anyone using it?

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceState {
    Absent,
    /// Present and nobody is linked to it.
    Idle,
    /// Running, or at least one link reads from it.
    Busy,
}

const NODE: &str = "PipeWire:Interface:Node";
const LINK: &str = "PipeWire:Interface:Link";

/// Find the node whose `node.name` equals `source` and decide whether it is in use.
pub fn source_state(dump: &Value, source: &str) -> SourceState {
    let Some(objects) = dump.as_array() else {
        return SourceState::Absent;
    };
    let node = objects.iter().find(|o| {
        o.get("type").and_then(Value::as_str) == Some(NODE)
            && o.pointer("/info/props/node.name").and_then(Value::as_str) == Some(source)
    });
    let Some(node) = node else {
        return SourceState::Absent;
    };
    let Some(id) = node.get("id").and_then(Value::as_u64) else {
        return SourceState::Absent;
    };
    if node.pointer("/info/state").and_then(Value::as_str) == Some("running") {
        return SourceState::Busy;
    }
    let linked = objects.iter().any(|o| {
        o.get("type").and_then(Value::as_str) == Some(LINK)
            && o.pointer("/info/output-node-id").and_then(Value::as_u64) == Some(id)
    });
    if linked {
        SourceState::Busy
    } else {
        SourceState::Idle
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SRC: &str = "alsa_input.usb-Generic_Blue_Microphones_X-00.iec958-stereo";

    fn node(id: u64, name: &str, state: &str) -> Value {
        json!({"id": id, "type": NODE, "info": {"state": state, "props": {"node.name": name}}})
    }
    fn link(out: u64, inp: u64) -> Value {
        json!({"id": 900 + out, "type": LINK, "info": {"output-node-id": out, "input-node-id": inp, "state": "active"}})
    }

    #[test]
    fn source_state_table() {
        struct Case {
            description: &'static str,
            dump: Value,
            expected: SourceState,
        }
        let cases = vec![
            Case {
                description: "suspended, no links",
                dump: json!([node(280, SRC, "suspended")]),
                expected: SourceState::Idle,
            },
            Case {
                description: "idle state, no links",
                dump: json!([node(280, SRC, "idle")]),
                expected: SourceState::Idle,
            },
            Case {
                description: "running",
                dump: json!([node(280, SRC, "running")]),
                expected: SourceState::Busy,
            },
            Case {
                description: "suspended but a client is linked (e.g. GNOME Settings meter)",
                dump: json!([node(280, SRC, "suspended"), link(280, 140)]),
                expected: SourceState::Busy,
            },
            Case {
                description: "link from another node does not count",
                dump: json!([node(280, SRC, "suspended"), link(49, 140)]),
                expected: SourceState::Idle,
            },
            Case {
                description: "link INTO the source does not count",
                dump: json!([node(280, SRC, "suspended"), link(140, 280)]),
                expected: SourceState::Idle,
            },
            Case {
                description: "no such node",
                dump: json!([node(49, "alsa_input.other", "suspended")]),
                expected: SourceState::Absent,
            },
            Case {
                description: "name is a prefix of the source only",
                dump: json!([node(
                    280,
                    "alsa_input.usb-Generic_Blue_Microphones_X-00",
                    "suspended"
                )]),
                expected: SourceState::Absent,
            },
            Case {
                description: "empty dump",
                dump: json!([]),
                expected: SourceState::Absent,
            },
            Case {
                description: "not an array",
                dump: json!({"oops": 1}),
                expected: SourceState::Absent,
            },
            Case {
                description: "node without numeric id",
                dump: json!([{"type": NODE, "info": {"state": "idle", "props": {"node.name": SRC}}}]),
                expected: SourceState::Absent,
            },
        ];
        for c in cases {
            assert_eq!(source_state(&c.dump, SRC), c.expected, "{}", c.description);
        }
    }
}
