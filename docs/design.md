# mic-health: detect and recover stuck USB microphones

Design for a NixOS workstation with USB microphones. Status: **phase 1 (alert-only probe) implemented**; actuator not yet.
The implementation is this repo: a Rust workspace with two binaries, `mic-probe` and `mic-reset`, plus a NixOS module (`nixosModules.default`).

Machine-specific values (serials, PipeWire source names) belong in the host's NixOS config, not here. `<BLUE_SERIAL>` and `<NANO_SERIAL>` below are placeholders.

## Problem

USB mics behind the desk hub can glitch at the USB level. After such a glitch they may come back
**enumerated but silent**:
- the ALSA mixer is unmuted, capture is "Running", and PipeWire shows the source;
- but every sample is exactly `0`.

Seen on 2026-11-21 on the Blue mic. The kernel log showed:

```
usb 5-1.3: device descriptor read/64, error -71
usb 5-1.3: device not accepting address 5, error -71
usb 5-1.3: USB disconnect, device number 5
usb 5-1.3: new high-speed USB device number 7 using xhci_hcd
```

The mic then recorded digital silence (`Peak -inf dB`) until it was physically replugged on
2026-11-24. After the replug it read `Peak -19 dB`, `RMS -40 dB` in a quiet room.

A working ADC always shows some noise. **Exact digital zero over a full window** is therefore a
precise, low-false-positive signature of this fault.

## Devices

| Name | USB ID | Serial | Port (today) | PipeWire source |
|---|---|---|---|---|
| Blue (Yeti family) | `046d:0ab7` | `<BLUE_SERIAL>` | `5-1.3` (via hub `5-1`) | `alsa_input.usb-Generic_Blue_Microphones_<BLUE_SERIAL>-00.iec958-stereo` |
| Yeti Nano | `046d:0acf` | `<NANO_SERIAL>` | `7-2` | `alsa_input.usb-Blue_Microphones_Yeti_Nano_<NANO_SERIAL>-00.analog-stereo` |

**Devices are identified by vendor + product + serial, never by port path.** The port changes
when the mic is moved.

## Goals and non-goals

**Goals**
- Detect the "enumerated but all-zero" fault on idle mics only.
- Recover automatically by doing in software exactly what a replug does.
- Least privilege. The unprivileged side cannot cause any effect beyond a rate-limited reset of
  a mic that is on the allowlist.
- Audit trail of every probe result, request and action in the journal.

**Non-goals**
- Fixing mics that have dropped off the bus entirely. Those are reported, not reset.
- Generic USB device recovery.
- Reloading kernel modules. Rejected, see below.

## Recovery action: de-authorize and re-authorize one USB device

```
echo 0 > /sys/bus/usb/devices/<dev>/authorized   # kernel disconnects the device
sleep 1
echo 1 > /sys/bus/usb/devices/<dev>/authorized   # kernel re-enumerates it
```

This is a software replug of **one** device.

| Option | Blast radius | Privilege needed | Verdict |
|---|---|---|---|
| `rmmod/modprobe snd-usb-audio` | Every USB audio device: Combo384 DAC, both mics, webcam mics | `CAP_SYS_MODULE`, effectively root | **Rejected** |
| Unbind/bind the driver | One interface, but also needs the `bind`/`unbind` files, which are driver-wide | root | Rejected: wider files |
| `USBDEVFS_RESET` ioctl | One device, but a reset is not a full re-enumerate | write on `/dev/bus/usb/...` | Weaker fix |
| **`authorized` 0 then 1** | **One device** | **write on one sysfs file** | **Chosen** |

Key property: a udev rule can `chown` **that single `authorized` file** to a dedicated user.
The match is on vendor, product and serial. The actuator then needs **no root and no
capabilities**.

## Architecture

```
  desktop user session                      system
┌───────────────────────────────┐      ┌───────────────────────────────────────┐
│ mic-probe.timer (every 10 min)│      │ mic-reset.socket                      │
│ mic-probe.service  (user)     │      │  /run/mic-reset/request.sock          │
│                               │      │  SOCK_SEQPACKET, 0660 root:micreq     │
│ 1. pw-dump  -> mic idle?      │      │                                       │
│ 2. pw-record 1 s -> memory    │ 2 B  │ mic-reset.service (User=mic-reset)    │
│ 3. classify: ok/dead/error    ├─────►│ 1. SO_PEERCRED uid == user?           │
│ 4. 3x dead -> request reset   │◄─────┤ 2. parse 2 bytes exactly              │
│ 5. notify-send the user       │ 1 B  │ 3. look up the device in /sys itself  │
└───────────────────────────────┘      │ 4. rate limit (state file, flock)     │
                                       │ 5. authorized 0 -> 1, verify          │
                                       │ 6. reply status, log to journal       │
                                       └───────────────────────────────────────┘
       udev rule (add/bind): chown mic-reset  <dev>/authorized  (allowlisted serials only)
```

### Trust boundaries

| Component | Runs as | Trusted with | Not trusted with |
|---|---|---|---|
| `mic-probe` | the desktop user (user service) | Reading audio from your own session | Anything privileged. It can only *ask*. |
| request socket | root-owned, group `micreq` | Carrying 2 bytes | Paths, commands, free text |
| `mic-reset` | `mic-reset` (system user, no shell, no home) | Writing one sysfs file per allowlisted mic | Anything outside that |
| allowlist | root, in the Nix store (read-only) | Defining which devices exist | Changes at runtime |

**Worst case if `mic-probe` or your account is compromised:** an attacker can replug an
allowlisted mic at most once per 30 min and 4 times per day.

**Worst case if `mic-reset` is compromised:** it can write `authorized` on allowlisted mics only.
No other files are writable, it has no network, and it has no capabilities.

## Component 1: `mic-probe` (unprivileged detector)

A systemd **user** service. PipeWire is per-user, so the probe must run in your session.
A user timer fires it every 10 min with `RandomizedDelaySec=60`.

For each mic on the allowlist:

1. **Check the mic is present.** Read `pw-dump` JSON, parsed with `serde_json`, and find the
   node whose `node.name` equals the configured source.
   - If it's missing: result `absent`. Notify once per transition. Never request a reset,
     because there is nothing enumerated to replug.
2. **Check the mic is idle.** Skip if any PipeWire **link** has this node as its output, or if
   the node state is `running`.
   - Earlier, GNOME Settings' Sound panel held about 20 meter streams open on the mic. While
     those exist, the mic counts as in use and is skipped.
   - After 12 consecutive skips (about 2 h), log `unable to check` once. That is information,
     not an action.
3. **Capture.**
   - Command, with fixed arguments and no shell:
     `pw-record --raw --target <node.name> --format s16 --rate 48000 --channels 2 -`
     (`--raw` is required: without it, `-` emits a big-endian Sun `.au` header; see Phase 0 (d))
   - Store paths are baked in by Nix.
   - Read about 1.0 s from the pipe into memory, then kill the child.
   - **Audio is never written to disk or logged.** Only statistics leave the function.
4. **Classify** with the function `classify(samples) -> Ok | Dead | Error`:
   - Discard the first 250 ms. Stream startup can legitimately be zero.
   - Fewer than 24 000 frames remaining → `Error` (short read or PipeWire hiccup). Never treated as dead.
   - All remaining samples `== 0` → `Dead`.
   - Otherwise → `Ok`, and log peak and RMS in dBFS.
5. **Confirm.**
   - After the first `Dead`, re-probe at 1 min and again at 2 min. This is the fast re-check path.
   - Only **3 consecutive `Dead`** results, all with the mic idle, count as a candidate.
   - **Re-enumeration gate (because hardware mute = exact zeros, see Phase 0 (a)):** a candidate
     produces a reset request **only if the device re-enumerated since the last `Ok` probe**.
     - The probe records `(busnum, devnum)` from sysfs (world-readable) with every `Ok` result.
     - The kernel assigns a new `devnum` on every enumeration. The Nov 21 fault went from devnum
       5 to 7, and today's physical replug went from 7 to 8.
     - A software reset (`authorized` 0→1) does **not** change `devnum` (it stayed 8, verified).
       That alone does not close the gate: the address still differs from the last `Ok` one.
       So the probe also records the address it requested a reset for (`reset_requested_addr`)
       and restarts the dead streak. A mic still dead at that address gets one "still silent"
       notice and **no** second request. A new enumeration (new address) or an `Ok` re-opens
       the gate. That is the loop-breaker, on top of the actuator's rate limit.
     - If the candidate is `Dead` with an unchanged `devnum`, the mic is most likely **muted**.
       Send one notification ("Blue mic silent: muted?"), and **never** a reset request.
     - With no `Ok` probe on record (fresh boot), there is no baseline, so notify only.
   - Any `Ok` or skip resets the counter.
6. **Request.**
   - Connect to `/run/mic-reset/request.sock` and send `[0x01, index]` (protocol version, mic
     index from the shared allowlist).
   - Read a 1-byte status, with a 5 s timeout.
   - Send a desktop notification with the outcome (`notify-send`).
7. **Verify the reset.** Re-probe 30 s after a reset, and notify if the mic is still dead.

**Hardening** (user-manager limits apply; options it can't support are skipped, not faked):
`NoNewPrivileges`, `RestrictAddressFamilies=AF_UNIX`, `LockPersonality`, `MemoryDenyWriteExecute`,
`SystemCallArchitectures=native`, `RestrictRealtime`, `ProtectSystem=strict` (where honoured), no
writable paths beyond `$XDG_RUNTIME_DIR`.

**Privacy note:** each probe opens the mic for about 1 s, so GNOME's microphone indicator will
flash. That is visible and honest by design. Probes never run while another app holds the mic.

## Component 2: the request channel

| Setting | Value | Why |
|---|---|---|
| Unit | `mic-reset.socket`, `ListenSequentialPacket=/run/mic-reset/request.sock` | Message boundaries, so there's no stream framing to get wrong |
| Ownership | `SocketUser=root`, `SocketGroup=micreq`, `SocketMode=0660` | Only members of `micreq` (only the desktop user) can connect |
| Activation | `Accept=no`; the service handles connections **serially** and exits after 60 s idle | One process means no race on the rate-limit state |
| Socket rate limit | `TriggerLimitIntervalSec=60`, `TriggerLimitBurst=5` | Stops activation floods |

Protocol, version 1:
- **Request:** exactly 2 bytes, `version=0x01` and `index` (u8). Anything else, including wrong
  length, unknown version or out-of-range index, is rejected with a status code and the connection
  is closed.
- **Response:** exactly 1 byte.

| Status byte | Meaning |
|---|---|
| `0x00` | reset done, device re-enumerated |
| `0x01` | rate-limited |
| `0x02` | bad request |
| `0x03` | unauthorised peer |
| `0x04` | device not found or ambiguous |
| `0x05` | reset failed or device did not return |
| `0x06` | dry-run: would reset |

The request carries **no path, name, serial or command**. The index only selects an entry in a
list that root controls.

## Component 3: `mic-reset` (actuator)

A system service, `User=mic-reset`, `Group=mic-reset`. Per connection:

1. **Check the peer.**
   - Call `getsockopt(SO_PEERCRED)`; the peer uid must equal the configured uid (the configured desktop user).
   - The group check on the socket is layer 1. This is layer 2.
2. **Parse the request.** Use a 2-byte buffer with a 2 s read timeout. Any other length is
   rejected (see Component 2).
3. **Look up the device itself.**
   - Scan `/sys/bus/usb/devices/*` for `idVendor`, `idProduct` and `serial` **exactly** matching
     the allowlist entry. Nothing from the request is used beyond the index.
   - It must find **exactly one** match, otherwise status `0x04`.
   - Canonicalise the path and require it to start with `/sys/devices/`.
4. **Rate limit.**
   - State lives in `StateDirectory=mic-reset`, file `/var/lib/mic-reset/state.json`. Take an
     exclusive `flock`, and write atomically via a temp file and rename.
   - Rules:
     - at least **30 min** between resets **per device and globally**;
     - at most **4 resets per rolling 24 h** globally.
   - Timestamps are UTC wall-clock, so they survive reboots.
   - **It fails closed:**
     - If `now < last` (the clock went backwards), treat the request as rate-limited.
     - If the state file is unreadable or corrupt, treat it as rate-limited and log at
       `err`. A human deletes the file to recover.
   - Rejected requests are logged but **not** recorded as resets.
5. **Act.**
   - Write `0` to `<dev>/authorized`, sleep 1 s, write `1`.
   - Opens use `O_WRONLY|O_NOFOLLOW` on the canonical path.
   - Record the timestamp **before** writing, so a crash mid-reset still counts.
6. **Verify.** Poll for up to 10 s until the device dir exists again with `authorized == 1`.
   Return `0x00` or `0x05`.
7. **Log.** Every connection produces one journal line: peer uid, index, device serial, decision
   and reason.

**`mode = "dry-run"`** (config) runs every step except the writes in step 5, and returns `0x06`.

### Granting the one writable file (udev)

```
ACTION=="add|bind", SUBSYSTEM=="usb", ENV{DEVTYPE}=="usb_device", \
  ATTR{idVendor}=="046d", ATTR{idProduct}=="0ab7", ATTR{serial}=="<BLUE_SERIAL>", \
  RUN+="<coreutils>/bin/chown mic-reset:mic-reset /sys%p/authorized"
```

The module generates one rule per allowlist entry. The attribute is re-created when the device
re-enumerates, and the rule re-applies on `add`.

**Fallback, if sysfs `chown` doesn't stick:**
- Run `mic-reset` as `User=root` with `CapabilityBoundingSet=` empty and `AmbientCapabilities=` empty.
- Root owns `authorized` (mode 0644), so uid 0 can write it **without any capability**.
- Everything else stays read-only via `ProtectSystem=strict` plus `ReadWritePaths=` limited to the
  allowlisted devices' `authorized` files.

### systemd hardening for `mic-reset.service`

```
NoNewPrivileges=yes            CapabilityBoundingSet=          AmbientCapabilities=
PrivateNetwork=yes             IPAddressDeny=any               RestrictAddressFamilies=AF_UNIX
ProtectSystem=strict           ProtectHome=yes                 PrivateTmp=yes
PrivateDevices=yes             ProtectKernelModules=yes        ProtectKernelLogs=yes
ProtectControlGroups=yes       ProtectClock=yes                ProtectHostname=yes
ProtectProc=invisible          ProcSubset=pid                  RestrictNamespaces=yes
RestrictRealtime=yes           RestrictSUIDSGID=yes            LockPersonality=yes
MemoryDenyWriteExecute=yes     SystemCallArchitectures=native  SystemCallFilter=@system-service
SystemCallFilter=~@privileged @resources @mount @module @reboot @swap @raw-io
UMask=0077                     StateDirectory=mic-reset        StateDirectoryMode=0700
StartLimitIntervalSec=1800     StartLimitBurst=10
```

- `ProtectKernelTunables=yes` makes `/sys` read-only, so it is paired with
  `ReadWritePaths=/sys/bus/usb/devices/` restricted to the allowlisted `authorized` files.
- The module generates those `ReadWritePaths` by serial at start time.
- Alternatively, use `ProtectKernelTunables=no` and rely on file ownership alone. Choose this
  during implementation, after testing which variant systemd accepts on sysfs.

Score target: `systemd-analyze security mic-reset.service` ≤ 1.5 ("OK").

## Configuration (NixOS module `mic-health.nix`)

```nix
services.micHealth = {
  enable = true;
  user = "alice";
  mode = "alert-only";            # "alert-only" | "dry-run" | "enforce"
  probeInterval = "10min";
  minResetGap = "30min";
  maxResetsPerDay = 4;
  mics = [
    { name = "blue";      vendor = "046d"; product = "0ab7";
      serial = "<BLUE_SERIAL>";
      source = "alsa_input.usb-Generic_Blue_Microphones_<BLUE_SERIAL>-00.iec958-stereo"; }
    { name = "yeti-nano"; vendor = "046d"; product = "0acf";
      serial = "<NANO_SERIAL>";
      source = "alsa_input.usb-Blue_Microphones_Yeti_Nano_<NANO_SERIAL>-00.analog-stereo"; }
  ];
};
```

- The module renders `mics` to a JSON file in the Nix store. Both binaries read it via a fixed
  `--config` argument in `ExecStart`, so the list index is the shared contract.
- The module creates:
  - system users and groups: `mic-reset` (system user and group), `micreq` (with the desktop user as a member);
  - the udev rules;
  - `mic-reset.socket` and `mic-reset.service`;
  - `systemd.user.services.mic-probe` and `systemd.user.timers.mic-probe`.
- In `alert-only` mode the socket and actuator are **not installed at all**.
- It follows the repo pattern of one feature per `.nix` file imported by `configuration.nix`.

**Code layout (this repo):** a Rust workspace with crates `mic-probe`, `mic-reset` and
`mic-health-common` (config, protocol, classifier). Hosts consume it as a flake input and import
`nixosModules.default`.

**Dependencies:**
- Keep them minimal: `serde` and `serde_json`; `nix` or `rustix` for `SO_PEERCRED`, `flock`
  and `O_NOFOLLOW`; `time` for time handling.
- No async runtime and no network crates.
- `cargo-deny` / `cargo audit` in CI.

## Rollout

| Phase | What | Exit criteria |
|---|---|---|
| 0. Experiments | (a) Does pressing the Blue mic's **hardware mute** produce exact zeros? Record muted, then unmuted. (b) Does a manual `authorized` 0→1 on a **healthy** mic bring it back working? (c) Does `chown` on the sysfs `authorized` file work and survive re-enumeration? (d) Confirm `pw-record ... -` emits raw s16 on stdout. | Done 2026-11-24, see Phase 0 results |
| 1. `alert-only` | Probe and notifications only, no actuator installed | 1–2 weeks with no false `Dead` |
| 2. `dry-run` | Actuator installed; does every check except the write | Requests, rate limits and logs look right |
| 3. `enforce` | Live resets | — |

**Phase 0 (a) showed that hardware mute produces exact zeros,** so a muted mic looks dead in the
audio alone. This is handled by the **re-enumeration gate** in Component 1, step 5. The rejected
alternatives:
- **Reading mute state over HID:** not possible. No HID event appeared on any mute press. The
  button is handled entirely inside the mic.
- **Requiring kernel-log USB errors:** this needs `systemd-journal` group access for the probe.
  The `devnum` check gives the same signal from world-readable sysfs.
- **Accepting the risk:** this would replug a muted mic every 30 min, up to the daily cap. It is
  noisy, and it might unmute the mic as a side effect.

## Phase 0 results (2026-11-24)

| # | Question | Result |
|---|---|---|
| (a) | Does the Blue mic's hardware mute produce exact zeros? | **Yes, unfortunately.** A 60 s run at 21:39 with the mute button pressed repeatedly read `Peak -inf` (exact digital zero) in every muted window. A one-sample `-90 dB` window (1 LSB) appeared at the transition. Unmuted windows read −15 to −50 dBFS. PipeWire never showed `MUTED`, so the mute happens inside the mic. **No HID event** (`KEY_MICMUTE` or otherwise) appeared on `...-event-if03`, with output line-buffered. Mute state cannot be read from the host. → Mute is indistinguishable from the fault by audio alone, which led to the re-enumeration gate (Component 1, step 5). A quiet unmuted room never read below −45 dBFS (19:20 baseline run). |
| (b) | Does `authorized` 0→1 bring a healthy mic back working? | **Yes.** The kernel logged `usb 5-1.3: authorized to connect`, re-created 4 interfaces and the HID inputs, and logged no errors. PipeWire re-created the source (node 276 → 280). A 1 s capture afterwards read peak −33 dBFS, RMS −50 dBFS. |
| (c) | Can sysfs `authorized` be chowned, and can that user write it? | **Yes.** `chown <user>:users` stuck (mode stays 644), and a non-root owner wrote `0` and `1` successfully, with no capability needed. Ownership **survives** the deauthorize/reauthorize cycle, because the `usb_device` object persists. It does **not** survive a physical replug, which creates a new device, so the udev `add` rule is required, as designed. |
| (d) | What does `pw-record ... -` emit? | A **Sun `.au` header** (`.snd`, big-endian) unless `--raw` is passed. With `--raw` it emits plain s16le. The probe must use `--raw`. 0.6 s of capture yields about 98–110 KB, consistent with 48 kHz × 2 ch × 2 B. |

Implication: the chosen privilege design holds, and detection gained the re-enumeration gate. `mic-reset` can run as a dedicated non-root user, with the
udev rule as its only grant. The root fallback is not needed.

The experiment scripts are summarised in each row above.

## Testing

Table-driven unit tests: each row has a `description`, the input and `expected`.

- **`classify`**
  - all zeros for 1 s → `Dead`
  - a single ±1 LSB sample → `Ok`
  - typical room noise → `Ok`
  - full-scale clipping → `Ok`
  - zeros only in the first 250 ms → `Ok`
  - exactly 24 000 frames after the discard → boundary, `Dead` when all zero
  - 23 999 frames → `Error`
  - empty buffer → `Error`
  - odd byte count → `Error`
- **Re-enumeration gate** (`MicState::step(observation)`, in `mic-health-common/src/gate.rs`)
  - 3× dead and devnum changed → request reset
  - 3× dead and devnum unchanged → notify "muted?" only
  - 3× dead with no `Ok` baseline → notify only
  - 2× dead and devnum changed → wait
  - dead after our own reset (devnum unchanged) → 3 fresh dead probes, then "still silent", no second request
  - re-enumerated again after a reset → gate re-opens
  - device absent → notify absent, no reset
- **Request parser**
  - `[0x01, 0]` → ok
  - `[0x01, n]` with n out of range → `0x02`
  - `[0x02, 0]` (bad version) → `0x02`
  - empty, 1 byte, or 3 bytes → `0x02`
- **Peer check:** matching uid → ok; root → rejected (only the desktop user is expected); other uid → `0x03`.
- **Rate limiter:**
  - first ever → allow
  - 29:59 after the last → deny
  - exactly 30:00 → allow
  - fifth reset inside 24 h → deny
  - the 24 h window rolling over → allow
  - `now < last` → deny
  - corrupt state → deny
  - another device inside the global gap → deny
- **Device lookup:**
  - exact match → path
  - serial differs by one character → not found
  - two matches → ambiguous
  - symlink escaping `/sys/devices` → rejected

These use a fake sysfs tree in a temp dir.

**Integration:** a NixOS VM test (`nixosTest`) with fake `authorized` files exercises the socket,
the peer check, the rate limit and dry-run end to end.

**Manual checks on the host:**
- `systemd-analyze security mic-reset.service`
- `systemctl --user start mic-probe` with both mics idle
- the Phase 0 experiments

## Operating notes

| Task | Command |
|---|---|
| Probe logs | `journalctl --user -u mic-probe` |
| Actuator logs | `journalctl -u mic-reset` |
| Check USB errors | `journalctl -k \| grep '5-1.3'` (`error -71` → flaky cable, hub or port) |
| Manual reset (root) | `echo 0 > /sys/bus/usb/devices/5-1.3/authorized; sleep 1; echo 1 > …/authorized` |
| Unlock after a corrupt state file | `sudo rm /var/lib/mic-reset/state.json` |

The root cause is still the hub or cable. If `error -71` keeps appearing, move the mic to a
direct port. This service is a safety net, not a fix.
