self:
{
  config,
  lib,
  pkgs,
  ...
}:

# Phase 1 (alert-only): installs the unprivileged mic-probe user service and timer.
# The actuator (socket, mic-reset service, users, udev rules) comes in phase 2.

let
  cfg = config.services.micHealth;

  micType = lib.types.submodule {
    options = {
      name = lib.mkOption {
        type = lib.types.strMatching "[a-z0-9-]+";
        description = "Short label used in logs and notifications.";
      };
      vendor = lib.mkOption {
        type = lib.types.strMatching "[0-9a-f]{4}";
        description = "USB idVendor (lowercase hex).";
      };
      product = lib.mkOption {
        type = lib.types.strMatching "[0-9a-f]{4}";
        description = "USB idProduct (lowercase hex).";
      };
      serial = lib.mkOption {
        type = lib.types.str;
        description = "USB serial. Devices are matched by vendor+product+serial, never by port.";
      };
      source = lib.mkOption {
        type = lib.types.str;
        description = "PipeWire source node.name for this mic.";
      };
    };
  };

  # The list index of `mics` is the wire-protocol device index, so order is significant.
  configFile = pkgs.writeText "mic-health.json" (
    builtins.toJSON {
      inherit (cfg) mode;
      mics = map (m: {
        inherit (m)
          name
          vendor
          product
          serial
          source
          ;
      }) cfg.mics;
      tools = {
        pw_dump = "${pkgs.pipewire}/bin/pw-dump";
        pw_record = "${pkgs.pipewire}/bin/pw-record";
        notify_send = "${pkgs.libnotify}/bin/notify-send";
      };
    }
  );
in
{
  options.services.micHealth = {
    enable = lib.mkEnableOption "mic-health stuck-microphone detection";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "mic-health.packages.\${system}.default";
      description = "The mic-health package.";
    };

    user = lib.mkOption {
      type = lib.types.str;
      description = "Desktop user whose PipeWire session is probed; the only uid allowed to request resets.";
    };

    mode = lib.mkOption {
      type = lib.types.enum [
        "alert-only"
        "dry-run"
        "enforce"
      ];
      default = "alert-only";
      description = "alert-only installs no actuator; dry-run checks everything but never writes; enforce resets.";
    };

    probeInterval = lib.mkOption {
      type = lib.types.str;
      default = "10min";
      description = "systemd time span between probes.";
    };

    minResetGap = lib.mkOption {
      type = lib.types.str;
      default = "30min";
      description = "Minimum time between resets, per device and globally.";
    };

    maxResetsPerDay = lib.mkOption {
      type = lib.types.ints.between 1 24;
      default = 4;
      description = "Global cap on resets per rolling 24 h.";
    };

    configFile = lib.mkOption {
      type = lib.types.path;
      readOnly = true;
      default = configFile;
      defaultText = lib.literalMD "generated from `mics` and `mode`";
      description = "Rendered JSON config, for other modules, e.g. a hotkey running `mic-probe --config <this> --report`.";
    };

    mics = lib.mkOption {
      type = lib.types.listOf micType;
      default = [ ];
      description = "Allowlist. The list index is the wire-protocol device index.";
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = cfg.mics != [ ] && lib.length cfg.mics <= 256;
        message = "services.micHealth.mics must list 1-256 microphones (index is one byte).";
      }
      {
        assertion = cfg.mode == "alert-only";
        message = "services.micHealth.mode: only \"alert-only\" is implemented so far.";
      }
    ];

    systemd.user.services.mic-probe = {
      description = "Check idle microphones for the all-zero fault";
      after = [ "pipewire.service" ];
      wants = [ "pipewire.service" ];
      unitConfig.ConditionUser = cfg.user;
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${cfg.package}/bin/mic-probe --config ${configFile}";
        # Up to 3 probes 60 s apart per mic, plus capture time.
        TimeoutStartSec = "15min";
        StateDirectory = "mic-health";
        StateDirectoryMode = "0700";
        UMask = "0077";
        Nice = 10;
        NoNewPrivileges = true;
        LockPersonality = true;
        MemoryDenyWriteExecute = true;
        RestrictRealtime = true;
        RestrictSUIDSGID = true;
        RestrictNamespaces = true;
        RestrictAddressFamilies = [ "AF_UNIX" ];
        SystemCallArchitectures = "native";
        SystemCallFilter = [
          "@system-service"
          "~@privileged @resources @mount"
        ];
        # Deny with EPERM rather than SIGSYS: PipeWire tries to raise thread priority and copes with a refusal.
        SystemCallErrorNumber = "EPERM";
        CapabilityBoundingSet = "";
        PrivateTmp = true;
        ProtectSystem = "strict";
        ProtectHome = "read-only";
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectControlGroups = true;
        ProtectClock = true;
        ProtectHostname = true;
      };
    };

    systemd.user.timers.mic-probe = {
      description = "Periodic microphone health probe";
      wantedBy = [ "timers.target" ];
      timerConfig = {
        OnStartupSec = "5min";
        OnUnitInactiveSec = cfg.probeInterval;
        RandomizedDelaySec = "60";
      };
    };
  };
}
