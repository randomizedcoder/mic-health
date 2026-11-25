self:
{ config, lib, pkgs, ... }:

# Skeleton: options only. The units, users, udev rules and socket from docs/design.md
# ("Configuration") are not wired up yet, so enabling this does nothing but warn.

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
      type = lib.types.enum [ "alert-only" "dry-run" "enforce" ];
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
    ];
    warnings = [ "services.micHealth: skeleton only; no services are installed yet." ];
  };
}
