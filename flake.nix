{
  description = "MoQ - Media over QUIC";

  # For pre-built binaries (faster builds), add our Cachix cache to your Nix config:
  #   extra-substituters = https://kixelated.cachix.org
  #   extra-trusted-public-keys = kixelated.cachix.org-1:CmFcV0lyM6KuVM2m9mih0q4SrAa0XyCsiM7GHrz3KKk=
  #
  # Or run: cachix use kixelated

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    crane.url = "github:ipetkov/crane";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      crane,
      rust-overlay,
      ...
    }:
    {
      nixosModules = {
        moq-relay = import ./nix/modules/moq-relay.nix;
      };

      overlays.default = import ./nix/overlay.nix { inherit crane; };
    }
    // flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };

        rust-toolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [
            "rust-src"
            "rust-analyzer"
          ];
          targets = pkgs.lib.optionals pkgs.stdenv.isDarwin [
            "x86_64-apple-darwin"
            "aarch64-apple-darwin"
          ];
        };

        # Rust dependencies
        rustDeps = with pkgs; [
          rust-toolchain
          just
          git
          cmake
          pkg-config
          glib
          libressl
          ffmpeg
          curl
          cargo-sort
          cargo-shear
          cargo-edit
          cargo-sweep
          cargo-semver-checks
        ]
        ++ pkgs.lib.optionals (!pkgs.stdenv.isDarwin) [
          # Marked broken on Darwin in nixpkgs, but builds fine on Linux.
          pkgs.release-plz
        ];

        # JavaScript dependencies
        jsDeps = with pkgs; [
          bun
          # Only for NPM publishing
          nodejs_24
        ];

        # Python dependencies
        pyDeps = with pkgs; [
          uv
          python3
        ];

        # CDN/deployment dependencies
        cdnDeps = with pkgs; [
          opentofu
        ];

        # Apply our overlay to get the package definitions
        overlayPkgs = pkgs.extend self.overlays.default;
      in
      {
        packages = rec {
          default = pkgs.symlinkJoin {
            name = "moq-all";
            paths = [
              moq-relay
              moq-clock
              moq-cli
              moq-token-cli
            ];
          };

          # Inherit packages from the overlay
          inherit (overlayPkgs)
            moq-relay
            moq-clock
            moq-cli
            moq-token-cli
            moq-boy
            ;
        };

        devShells.default = pkgs.mkShell {
          packages = rustDeps ++ jsDeps ++ pyDeps ++ cdnDeps;

          # jemalloc's configure uses -O0 test builds, which conflict with
          # Nix's _FORTIFY_SOURCE hardening (requires -O).
          hardeningDisable = [ "fortify" ];

          shellHook = ''
            export LIBCLANG_PATH="${pkgs.libclang.lib}/lib"
          '';
        };

        formatter = pkgs.nixfmt-tree;
      }
    );
}
