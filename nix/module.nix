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
        containing SLACK_USER_TOKEN / LINEAR_API_KEY / ANTHROPIC_API_KEY.
        Not required to run the dashboard itself — only the live pollers,
        once wired up, read these.
      '';
    };
  };

  config = mkIf cfg.enable {
    systemd.services.cos = {
      description = "cos chief-of-staff dashboard";
      wantedBy = [ "multi-user.target" ];
      after = [ "network.target" ];

      environment = {
        COS_ADDR = "127.0.0.1:${toString cfg.port}";
        COS_DB_PATH = "/var/lib/cos/cos.sqlite3";
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
