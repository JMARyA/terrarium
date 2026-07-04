# Called as `import ./nixos/module.nix self` from the flake so the module can
# reference the flake's own package as the default.
self:
{ config, lib, pkgs, ... }:
let
  cfg = config.services.terrarium;
in
{
  options.services.terrarium = {
    enable = lib.mkEnableOption "terrarium Terraform HTTP state backend";

    package = lib.mkOption {
      type = lib.types.package;
      description = "The terrarium package to use.";
      default = self.packages.${pkgs.system}.default;
      defaultText = lib.literalExpression "self.packages.\${system}.default";
    };

    dataDir = lib.mkOption {
      type = lib.types.path;
      default = "/var/lib/terrarium";
      description = "Directory used for state files, locks, and user data.";
    };
  };

  config = lib.mkIf cfg.enable {
    users.users.terrarium = {
      isSystemUser = true;
      group = "terrarium";
    };
    users.groups.terrarium = { };

    systemd.services.terrarium = {
      description = "Terrarium Terraform state backend";
      wantedBy = [ "multi-user.target" ];
      after = [ "network.target" ];
      environment = {
        TERRARIUM_DATA = cfg.dataDir;
        RUST_LOG = "info";
      };
      serviceConfig = {
        ExecStart = "${cfg.package}/bin/terra serve";
        User = "terrarium";
        Group = "terrarium";
        StateDirectory = "terrarium";
        StateDirectoryMode = "0750";
        Restart = "on-failure";
        RestartSec = "5s";
        NoNewPrivileges = true;
        PrivateTmp = true;
        ProtectSystem = "strict";
        ProtectHome = true;
      };
    };
  };
}
