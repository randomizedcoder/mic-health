//! `mic-probe`: see docs/design.md. Not implemented yet.

use std::process::ExitCode;

fn main() -> ExitCode {
    // Keep the shared crate linked so the workspace builds the real dependency graph.
    let _ = mic_health_common::protocol::VERSION;
    eprintln!("mic-probe: not implemented yet (see docs/design.md)");
    ExitCode::from(2)
}
