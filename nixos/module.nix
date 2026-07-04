# Called as `import ./nixos/module.nix self` from the flake so the module can
# reference the flake's own package as the default.
self:
{ config, lib, pkgs, ... }:
let
  cfg = config.services.terrarium;

  mirrorEntry = lib.types.submodule {
    options = {
      namespace = lib.mkOption {
        type = lib.types.str;
        description = "Provider namespace (e.g. `hashicorp`).";
      };
      type = lib.mkOption {
        type = lib.types.str;
        description = "Provider type (e.g. `aws`).";
      };
      versions = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [];
        description = "Versions to mirror. Empty list means all available versions.";
      };
      platforms = lib.mkOption {
        type = lib.types.listOf (lib.types.submodule {
          options = {
            os   = lib.mkOption { type = lib.types.str; };
            arch = lib.mkOption { type = lib.types.str; };
          };
        });
        default = [];
        description = "Platforms to mirror. Empty list uses the default five (linux/darwin/windows × amd64/arm64).";
      };
    };
  };

  # Serialise the mirrors list to JSON, omitting empty optional fields so the
  # server's serde_json deserialisation matches MirrorRequest exactly.
  mirrorsJson = pkgs.writeText "terrarium-mirrors.json" (builtins.toJSON (
    map (m: lib.filterAttrs (_: v: v != [] && v != null) {
      namespace = m.namespace;
      "type"    = m.type;
      versions  = if m.versions == [] then null else m.versions;
      platforms = if m.platforms == [] then null else m.platforms;
    }) cfg.mirrors
  ));
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
      description = "Directory used for state files, locks, user data, and the provider registry.";
    };

    mirrors = lib.mkOption {
      type = lib.types.listOf mirrorEntry;
      default = [];
      description = ''
        Providers to mirror automatically from registry.terraform.io on startup.
        Binaries and documentation are fetched and stored in the local registry.

        Example:
        ```nix
        mirrors = [
          { namespace = "hashicorp"; type = "aws";
            versions = [ "5.60.0" ]; }
          { namespace = "hashicorp"; type = "kubernetes"; }
        ];
        ```
      '';
    };

    mirrorInterval = lib.mkOption {
      type = lib.types.nullOr lib.types.ints.positive;
      default = null;
      description = ''
        How often (in seconds) to refresh mirrored providers.
        `null` (the default) mirrors only once on startup.
        86400 = daily, 3600 = hourly.
      '';
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
      } // lib.optionalAttrs (cfg.mirrorInterval != null) {
        TERRARIUM_MIRROR_INTERVAL = toString cfg.mirrorInterval;
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

    # Write mirrors.json into the data directory before the service starts.
    system.activationScripts.terrariumMirrors = lib.mkIf (cfg.mirrors != [])
      (lib.stringAfter [ "users" "groups" ] ''
        install -d -o terrarium -g terrarium -m 750 ${cfg.dataDir}
        install -m 640 -o terrarium -g terrarium \
          ${mirrorsJson} ${cfg.dataDir}/mirrors.json
      '');
  };
}
