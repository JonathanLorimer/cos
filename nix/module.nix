self: { config, lib, pkgs, ... }:

with lib;

let
  cfg = config.services.cos;
in
{
  options.services.cos = {
    enable = mkEnableOption "cos chief-of-staff dashboard";

    package = mkOption {
      type = types.package;
      default = self.packages.${pkgs.system}.cos;
      description = "The cos package to run.";
    };

    port = mkOption {
      type = types.port;
      default = 4700;
      description = "Port the dashboard listens on.";
    };

    environmentFile = mkOption {
      type = types.nullOr types.path;
      default = null;
      description = ''
        Path to an EnvironmentFile for systemd, e.g. a sops-nix secret
        containing SLACK_USER_TOKEN / LINEAR_API_KEY / GITHUB_TOKEN /
        ANTHROPIC_API_KEY. Not required to run the dashboard itself — every
        source degrades to "not configured, no data from here" rather than
        failing — but without it there's nothing for the pollers to do.
      '';
    };

    pollIntervalSecs = mkOption {
      type = types.nullOr types.ints.positive;
      default = null;
      description = "Overrides COS_POLL_INTERVAL_SECS (app default: 300s).";
    };

    bulkThreshold = mkOption {
      type = types.nullOr types.ints.positive;
      default = null;
      description = "Overrides COS_BULK_THRESHOLD (app default: 20 pending items).";
    };
  };

  config = mkIf cfg.enable {
    systemd.services.cos = {
      description = "cos chief-of-staff dashboard";
      wantedBy = [ "multi-user.target" ];
      # network.target only means "networking is configured," not "the
      # internet is reachable" — this service calls out to four external
      # APIs on startup, so it needs to wait for an actual route, not just
      # the subsystem being up.
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];

      environment = {
        COS_ADDR = "127.0.0.1:${toString cfg.port}";
        COS_DB_PATH = "/var/lib/cos/cos.sqlite3";
      }
      // optionalAttrs (cfg.pollIntervalSecs != null) {
        COS_POLL_INTERVAL_SECS = toString cfg.pollIntervalSecs;
      }
      // optionalAttrs (cfg.bulkThreshold != null) {
        COS_BULK_THRESHOLD = toString cfg.bulkThreshold;
      };

      serviceConfig = {
        ExecStart = "${cfg.package}/bin/cos";
        DynamicUser = true;
        StateDirectory = "cos";
        Restart = "on-failure";
      } // optionalAttrs (cfg.environmentFile != null) {
        EnvironmentFile = cfg.environmentFile;
      };
    };
  };
}
