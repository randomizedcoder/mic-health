# mic-health

Detects USB microphones that are **enumerated but silent** (every sample exactly zero, typically
after a USB glitch such as `error -71`). It recovers them with a **software replug of that one
device**, using a privilege-separated, rate-limited design.

> Status: **phase 1, alert-only.** `mic-probe` and its user service and timer work: they probe,
> apply the gate and notify, but never reset. `mic-reset` (phase 2) is still a stub, and the module
> rejects any mode other than `alert-only`.

## How it works (short version)

| Part | Runs as | Job |
|---|---|---|
| `mic-probe` | the desktop user (systemd user timer) | Skips mics in use. Captures about 1 s **into memory only**. Flags exact digital silence. Asks for a reset only if the device also **re-enumerated** since it last worked, because a hardware-muted mic is also exact zero. |
| `mic-reset` | dedicated non-root user, socket-activated | Takes a 2-byte request. Checks the peer uid. Looks the device up itself by vendor+product+serial. Allows at most one reset per 30 min and 4 per day, and fails closed. Writes `0` then `1` to that device's sysfs `authorized` file. |
| udev rule | — | Hands **only** the allowlisted devices' `authorized` file to `mic-reset`. |

Full design, threat model, experiment results and test plan: [`docs/design.md`](docs/design.md).

## Use

```nix
# flake.nix
inputs.mic-health.url = "github:randomizedcoder/mic-health";

# configuration
imports = [ inputs.mic-health.nixosModules.default ];
services.micHealth = {
  enable = true;
  user = "alice";
  mode = "alert-only";   # "dry-run" / "enforce" arrive with phase 2
  mics = [
    { name = "desk"; vendor = "046d"; product = "0ab7"; serial = "<serial>";
      source = "alsa_input.usb-..."; }
  ];
};
```

Logs: `journalctl --user -u mic-probe`. State: `~/.local/state/mic-health/probe-state.json`.
Run once by hand: `systemctl --user start mic-probe`.

## Develop

```bash
nix develop          # cargo, clippy, rustfmt, rust-analyzer, cargo-deny
cargo test
nix flake check      # builds, runs tests and clippy (-D warnings)
```

If rustup is installed, `cargo clippy` may run rustup's older clippy from `~/.cargo/bin`; use `nix flake check`.

Tests are table-driven: each case has a `description` and an `expected` value.
