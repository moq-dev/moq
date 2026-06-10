{
  description = "MoQ - Media over QUIC";

  # Pre-built binaries live in our Cachix cache. Only tagged releases are
  # pushed (CI fires on moq-relay-v*, moq-cli-v*, etc.), so pin the flake ref
  # to a recent tag to get a hit. The default branch HEAD is not cached and
  # builds from source:
  #   nix run github:moq-dev/moq/moq-relay-v0.12.4#moq-relay --accept-flake-config
  #
  # --accept-flake-config opts into the nixConfig below for one command. To
  # trust the cache permanently instead, run: cachix use kixelated
  nixConfig = {
    extra-substituters = [ "https://kixelated.cachix.org" ];
    extra-trusted-public-keys = [
      "kixelated.cachix.org-1:CmFcV0lyM6KuVM2m9mih0q4SrAa0XyCsiM7GHrz3KKk="
    ];
  };

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
          targets = [
            "wasm32-unknown-unknown"
          ]
          ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
            "x86_64-apple-darwin"
            "aarch64-apple-darwin"
          ];
        };

        # GStreamer dependencies (for moq-gst plugin)
        gstreamerDeps = with pkgs; [
          gst_all_1.gstreamer
          gst_all_1.gstreamer.dev
          gst_all_1.gst-plugins-base
          gst_all_1.gst-plugins-good
          gst_all_1.gst-plugins-bad
        ];

        # Rust dependencies
        rustDeps =
          with pkgs;
          [
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
            cargo-deny
            # Browser/WASM bindings (rs/moq-wasm -> @moq/wasm via `just wasm`).
            # wasm-bindgen-cli must match the `wasm-bindgen` crate version (the
            # crate is pinned to nixpkgs' CLI version); bump both together.
            wasm-bindgen-cli
          ]
          ++ gstreamerDeps
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

        # Tools for producing .deb/.rpm artifacts. Cross-platform so that
        # `just rs package` works from `nix develop` on both Linux and macOS.
        packagingDeps = with pkgs; [
          nfpm
          dpkg
          gettext

          # cargo-zigbuild + zig let CI build a single binary that links
          # against an older glibc (passed as `<triple>.<glibc>`), so the
          # same artifact ships in both .deb and .rpm. No docker needed.
          cargo-zigbuild
          zig
        ];

        # Tools needed to regenerate and sign the apt/rpm repositories.
        # Linux-only because apt and createrepo_c are marked broken on Darwin
        # in nixpkgs. The publish workflows only ever run on Linux runners.
        publishDeps =
          with pkgs;
          lib.optionals (!stdenv.isDarwin) [
            apt
            createrepo_c
            rpm
            rclone
            gnupg
            gzip
          ];

        # Linters / formatters required by `just ci`; `just check` and
        # `just fix` guard each tool with `command -v` so they skip
        # silently when the binary isn't on $PATH.
        lintDeps = with pkgs; [
          shellcheck
          shfmt
          actionlint
          taplo
          nixfmt
        ];

        # Apply our overlay to get the package definitions
        overlayPkgs = pkgs.extend self.overlays.default;
      in
      {
        packages = (rec {
          default = pkgs.symlinkJoin {
            name = "moq-all";
            paths = [
              moq-relay
              moq-cli
              moq-token-cli
            ];
          };

          # Inherit packages from the overlay
          inherit (overlayPkgs)
            moq-relay
            moq-cli
            moq-token-cli
            moq-boy
            libmoq
            moq-gst
            ;

          # Bundle of packaging + repo-publish tooling, pinned via flake.lock.
          # CI builds this and prepends its bin/ to $PATH so subsequent steps
          # use the same versions a local `nix develop` user would.
          packaging = pkgs.symlinkJoin {
            name = "moq-packaging-tools";
            paths = packagingDeps ++ publishDeps;
          };
        })
        # x86_64-darwin release artifacts are cross-compiled from the
        # aarch64-darwin runner (see nix/overlay.nix). The cross outputs only
        # evaluate on an aarch64-darwin host, so gate them on the system to
        # keep `nix flake check` working on Linux and Intel macs.
        // pkgs.lib.optionalAttrs (system == "aarch64-darwin") {
          inherit (overlayPkgs)
            moq-relay-x86_64-apple-darwin
            moq-cli-x86_64-apple-darwin
            moq-token-cli-x86_64-apple-darwin
            libmoq-x86_64-apple-darwin
            moq-gst-plugin-x86_64-apple-darwin
            ;
        };

        # Re-export gst_all_1 so users can pair the plugin with a matching
        # gstreamer in one nix invocation:
        #   nix shell .#moq-gst .#gst_all_1.gstreamer --command gst-inspect-1.0 moq
        # Sourcing from the same nixpkgs the moq-gst build linked against
        # avoids the duplicate-symbol crash you get with
        # `nixpkgs#gst_all_1.gstreamer`, which can resolve to a different
        # store hash. Lives under legacyPackages because nested attrsets
        # are disallowed in the flake `packages` schema.
        legacyPackages = {
          inherit (pkgs) gst_all_1;
        };

        devShells.default = pkgs.mkShell {
          packages = rustDeps ++ jsDeps ++ pyDeps ++ cdnDeps ++ packagingDeps ++ lintDeps;

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
