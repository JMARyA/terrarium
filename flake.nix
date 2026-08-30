{
  description = "Terrarium Flake";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

    crane.url = "github:ipetkov/crane";

    flake-utils.url = "github:numtide/flake-utils";

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      crane,
      flake-utils,
      rust-overlay,
      ...
    }@inputs:
    let
      nixosModule = import ./nixos/module.nix self;
    in
    {
      nixosModules.default = nixosModule;

      # Read by moira as `.#moiraFlake` — system-agnostic, so it belongs here
      # rather than inside eachDefaultSystem. Both `.moira/container.yaml` and
      # `.moira/container-release.yaml` declare
      # `needs_flake: packages.containerImage`; declaring the spec is what turns
      # that into an evaluated, scheduled derivation graph instead of an
      # on-demand `nix build` inside the push step.
      #
      # `checks.nixos-integration` is a NixOS VM test and needs KVM. moira maps
      # `requiredSystemFeatures` onto the same label vocabulary as agent
      # placement, so if no agent advertises it the derivation is unclaimable
      # and sits in the queue rather than failing. It blocks nothing — nothing
      # depends on it — but if it shows as stranded on the derivations page,
      # `exclude = [ "checks.nixos-integration" ]` is the fix.
      moiraFlake = {
        include = [
          "packages.*"
          "checks.*"
        ];
      };
    }
    // flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };

        rustToolchain = pkgs.rust-bin.nightly.latest.default.override {
          extensions = [
            "rust-src"
            "rust-analyzer"
          ];
        };

        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        commonArgs = {
          src = craneLib.cleanCargoSource ./.;
          strictDeps = true;

          OPENSSL_NO_VENDOR = "1";

          PKG_CONFIG_PATH = "${pkgs.openssl.dev}/lib/pkgconfig";

          nativeBuildInputs = [
            pkgs.pkg-config
          ];

          buildInputs = [
            pkgs.openssl
          ]
          ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isDarwin [
            pkgs.libiconv
          ];
        };

        terrarium = craneLib.buildPackage (
          commonArgs
          // {
            cargoArtifacts = craneLib.buildDepsOnly commonArgs;
          }
        );

        dockerImage = pkgs.dockerTools.buildLayeredImage {
          name = "terrarium";
          tag = "latest-${pkgs.stdenv.hostPlatform.linuxArch}";
          contents = [
            terrarium
            pkgs.cacert
          ];
          config = {
            Cmd = [
              "/bin/terra"
              "serve"
            ];
            WorkingDir = "/app";
            Env = [
              "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
              "SSL_CERT_DIR=/etc/ssl/certs"
            ];
          };

          fakeRootCommands = ''
            mkdir -p /usr /usr/bin /etc /tmp /root /app
            ln -s /bin /usr/bin
            chmod 700 /root
            echo "root:x:0:0:root:/root:/bin/sh" > /etc/passwd
            echo "root:x:0:" > /etc/group
          '';

          enableFakechroot = true;
        };

        nixosTest = import ./nixos/test.nix { inherit pkgs nixosModule terrarium dockerImage; };
      in
      {
        checks = {
          inherit terrarium;
        } // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
          nixos-integration = nixosTest;
        };

        packages.default = terrarium;
        packages.containerImage = dockerImage;

        apps.default = flake-utils.lib.mkApp {
          drv = terrarium;
        };

        devShells.default = craneLib.devShell {
          checks = self.checks.${system};

          packages = [ ];
        };
      }
    );
}
