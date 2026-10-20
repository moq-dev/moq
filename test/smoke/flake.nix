{
  description = "moq smoke - interop test for the public moq packages";

  # nixpkgs-unstable, intentionally NOT locked: this repo ships no flake.lock so
  # every `nix develop` resolves the latest toolchain (and the latest Chromium),
  # matching the "always test latest" policy. The one version that has to line up
  # (npm `playwright` <-> this Chromium) is exported below as PLAYWRIGHT_VERSION
  # and enforced by smoke.sh, so a nixpkgs bump can't silently drift.
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    { nixpkgs, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
      in
      {
        devShells.default = pkgs.mkShell {
          # smoke.sh installs the clients from public registries; this shell only
          # provides the toolchains those installs need. The Rust binaries under
          # test (moq-relay, moq) come from a channel (cargo/brew/apt), not from
          # here, though `cargo` is included so `cargo install moq-cli` works.
          packages = with pkgs; [
            # orchestrator + harness tools
            just
            git
            ffmpeg
            curl
            openssl # throwaway localhost certificate for cloudflare.sh
            jq
            coreutils # GNU `timeout` (macOS lacks it)
            procps # `pgrep`

            # rust client + a way to `cargo install` the binaries under test
            cargo
            rustc

            # python client (PyPI moq-rs)
            uv
            python3

            # go client (go get moq.dev/moq); cgo links the prebuilt libmoq_ffi.a
            go

            # kotlin client (dev.moq:moq from Maven Central) on the JVM
            jdk
            gradle

            # c client (libmoq prebuilt release): cc comes from stdenv; these fetch
            # + extract the tarball. (The swift client uses the system Xcode
            # toolchain, not nix: nixpkgs swift on Darwin clashes with the Xcode SDK.)
            gnutar

            # browser client (npm @moq/watch + @moq/publish)
            bun
            nodejs_24
            # Headless Chromium. The npm `playwright` must match this build; rather
            # than hardcode it, smoke.sh reads $PLAYWRIGHT_VERSION (below) and fails
            # if clients/js/package.json pins anything else.
            playwright-driver.browsers

            # `just check` linters
            shellcheck
            shfmt
            actionlint
          ];

          shellHook = ''
            # Use the nix-provided Chromium instead of letting Playwright download one.
            export PLAYWRIGHT_BROWSERS_PATH="${pkgs.playwright-driver.browsers}"
            export PLAYWRIGHT_SKIP_VALIDATE_HOST_REQUIREMENTS=true
            # The exact npm `playwright` version that matches the Chromium above.
            # smoke.sh asserts clients/js/package.json pins this, so a floating
            # nixpkgs can never leave the pin stale.
            export PLAYWRIGHT_VERSION="${pkgs.playwright-driver.version}"
          '';
        };
      }
    );
}
