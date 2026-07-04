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
          ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
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
          ];
          config = {
            Cmd = [
              "/bin/terra"
              "serve"
            ];
            WorkingDir = "/app";
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

        nixosTest = import ./nixos/test.nix { inherit pkgs nixosModule terrarium; };
      in
      {
        checks = {
          inherit terrarium;
        } // pkgs.lib.optionalAttrs pkgs.stdenv.isLinux {
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
