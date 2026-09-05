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
    let
      eachSupportedSystem = flake-utils.lib.eachSystem [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
    in
    {
      nixosModules = {
        moq-relay = import ./nix/modules/moq-relay.nix;
      };

      overlays.default = import ./nix/overlay.nix { inherit crane; };
    }
    // eachSupportedSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };

        # Pinned build toolchain (not latest stable) so `nix develop` and CI
        # compile against a fixed rustc and the relay's MSRV can't creep up
        # unnoticed. Set to moq-relay's 1.95 (the highest crate MSRV in the
        # workspace) so the whole workspace, including the relay, builds; the
        # library crates declare a lower 1.91 floor (Cargo.toml rust-version).
        rust-toolchain = pkgs.rust-bin.stable."1.95.0".default.override {
          extensions = [
            "rust-src"
            "rust-analyzer"
          ];
          targets = [
            "wasm32-unknown-unknown"
          ]
          ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isDarwin [
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

        tsduck = pkgs.tsduck.overrideAttrs (
          old:
          pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isDarwin {
            makeFlags = old.makeFlags ++ [ "CXXFLAGS_WARNINGS=" ];
          }
        );

        # Rust dependencies
        rustDeps =
          with pkgs;
          [
            rust-toolchain
            just
            git
            cmake
            pkg-config
            # Sets LIBCLANG_PATH + BINDGEN_EXTRA_CLANG_ARGS so ffmpeg-sys-next's
            # bindgen finds libc headers (<errno.h>) on hosts without system
            # headers in /usr/include, e.g. the self-hosted runner.
            rustPlatform.bindgenHook
            glib
            libressl
            ffmpeg
            curl
            # MPEG-TS validation (tsp, tsanalyze) for the ts-compliance harness.
            tsduck
            cargo-sort
            cargo-shear
            cargo-edit
            cargo-semver-checks
            cargo-deny
            cargo-nextest
            # Browser/WASM bindings (rs/moq-wasm -> @moq/wasm via `just wasm`).
            # wasm-bindgen-cli must match the `wasm-bindgen` crate version (the
            # crate is pinned to nixpkgs' CLI version); bump both together.
            wasm-bindgen-cli
          ]
          ++ gstreamerDeps
          ++ pkgs.lib.optionals (!pkgs.stdenv.hostPlatform.isDarwin) [
            # Marked broken on Darwin in nixpkgs, but builds fine on Linux.
            pkgs.release-plz
            # cpal's `alsa-sys` (moq-audio `capture` / `playback` features) links
            # libasound on Linux via pkg-config; macOS uses CoreAudio, so no dep
            # there. See `alsaPlugins` below for reaching the default device at
            # runtime.
            pkgs.alsa-lib
            # The `pulse` PCM, for a desktop whose default device routes through
            # PulseAudio rather than the `pipewire` PCM below. Loaded at runtime
            # only, via `alsaPlugins`.
            pkgs.alsa-plugins
            # moq-video's `pipewire` screen-capture feature: the pipewire crate
            # links libpipewire-0.3 via pkg-config and generates bindings at build
            # time (bindgenHook above provides libclang). Linux-only; macOS uses
            # ScreenCaptureKit.
            pkgs.pipewire
          ];

        # Where the shell's libasound looks for PCM plugins.
        #
        # ALSA's default device is a plugin on any desktop running a sound
        # server: `pipewire` (shipped by pipewire itself) or `pulse` (from
        # alsa-plugins). The shell's alsa-lib only searches its own store path,
        # which holds neither, so opening the default device fails with ENXIO
        # and audio only works against raw `hw:` devices. The host's plugins are
        # not a substitute: they link the host's libasound and libpipewire, so
        # loading them into a Nix-built binary hits a glibc version mismatch.
        #
        # ALSA_PLUGIN_DIR takes one directory, hence the join.
        alsaPlugins = pkgs.symlinkJoin {
          name = "moq-alsa-plugins";
          paths = [
            pkgs.alsa-plugins
            pkgs.pipewire
          ];
        };

        # JavaScript dependencies
        jsDeps = with pkgs; [
          bun
          # Only for NPM publishing
          nodejs_24
          # JSR publishing. We call `deno publish` directly instead of `bunx jsr`
          # so the release doesn't race on a runtime binary download.
          deno
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

        # IETF Internet-Draft tooling (drafts/justfile). kramdown-rfc renders
        # the kramdown-rfc markdown to RFC XML; xml2rfc produces the txt/html;
        # mmark covers any mmark-format drafts; libxml2 provides xmllint.
        draftsDeps = with pkgs; [
          rubyPackages.kramdown-rfc2629
          xml2rfc
          mmark
          libxml2
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
          lib.optionals (!stdenv.hostPlatform.isDarwin) [
            apt
            createrepo_c
            rpm
            rclone
            gnupg
            gzip
          ];

        # Developer workflow tooling not needed for builds: jq reads
        # `cargo metadata` in `just rs check-changed`.
        devTools = with pkgs; [
          jq
        ];

        # Linters / formatters used by `just check` and `just fix`, which
        # guard each tool with `command -v` so they skip silently when the
        # binary isn't on $PATH. CI sets MOQ_STRICT=1, which turns that skip
        # into an error (see `_tools` in the root justfile), so this list and
        # that one have to stay in step.
        lintDeps = with pkgs; [
          shellcheck
          shfmt
          actionlint
          taplo
          nixfmt
        ];

        # Kotlin wrapper (kt/) toolchain so `just kt check` actually compiles
        # the wrapper and runs :moq:jvmTest instead of silently skipping.
        # Pinned to gradle 8.x (Kotlin 2.0.21's Gradle plugin predates Gradle
        # 9) and JDK 17 (the wrapper's jvmTarget). Cross-platform: the kt check
        # builds moq-ffi for the host and runs on both Linux and macOS.
        ktDeps = with pkgs; [
          jdk17
          gradle_8
        ];

        # uniffi-bindgen-go renders rs/moq-ffi into the generated half of
        # go/ffi. Not in nixpkgs, so build it from source; without it `just go
        # check` skips itself, which reads as a pass in CI.
        #
        # The tag pairs the generator's own version with the uniffi release it
        # targets (v0.8.0+v0.32.0 -> uniffi 0.32), and it only understands
        # metadata emitted by that uniffi, so it moves with the `uniffi`
        # dependency in rs/moq-ffi/Cargo.toml. Five other places name the same
        # generator version and must be bumped together: the repo and revision
        # in release-go-ffi.yml, and the `cargo install` line in
        # rs/moq-ffi/build.sh, go/ffi/README.md, go/scripts/check.sh, and
        # doc/lib/go/index.md.
        #
        # This points at a fork rather than NordSecurity because upstream has no
        # uniffi 0.32 generator: the metadata encoding changed in 0.32 even
        # though the contract version did not, so v0.7.1+v0.31.0 fails to read a
        # 0.32-built cdylib at all. The fork carries the port, tracked upstream
        # as NordSecurity/uniffi-bindgen-go#96. Move back to NordSecurity once
        # they tag a 0.32 release.
        uniffi-bindgen-go = pkgs.rustPlatform.buildRustPackage rec {
          pname = "uniffi-bindgen-go";
          version = "0.8.0+v0.32.0";

          src = pkgs.fetchFromGitHub {
            owner = "kixelated";
            repo = "uniffi-bindgen-go";
            rev = "v${version}";
            hash = "sha256-BBa47Ib8dQb8GSSqaQv3xxR0RYjiseM3L7ND1HhQcVI=";
          };

          cargoHash = "sha256-U7JLPB83CknoIf5nHoXBzqY6O2YveZ6HNOkYVKukY0Q=";

          # The tag is a virtual workspace whose other members are uniffi test
          # fixtures. Building from the root would compile all of them, and CI
          # has no Nix binary cache, so it would pay for that on every run.
          buildAndTestSubdir = "bindgen";

          # The upstream test suite generates fixtures and compiles them with a
          # Go toolchain, which is a lot of build for a binary we only invoke.
          doCheck = false;
        };

        # Go toolchain (go/) so `just go check` regenerates the bindings and
        # runs `go test -race` on the wrapper instead of skipping. Cross-platform:
        # the check builds moq-ffi for the host and runs on Linux and macOS.
        goDeps = [
          pkgs.go
          uniffi-bindgen-go
        ];

        # uniffi-bindgen-dart renders rs/moq-ffi into dart/moq_ffi. The fork
        # carries the uniffi 0.32 port and library-mode CLI while those changes
        # remain open upstream.
        uniffi-bindgen-dart = pkgs.rustPlatform.buildRustPackage rec {
          pname = "uniffi-bindgen-dart";
          version = "0.3.0+v0.32.0";

          src = pkgs.fetchFromGitHub {
            owner = "kixelated";
            repo = "uniffi-dart";
            rev = "v${version}";
            hash = "sha256-jvVEZVZLorj+GPUXL6Y4riCLsbJcWWbQgIIUoK/ZSEo=";
          };

          # The upstream repository ignores Cargo.lock so cargo installs test
          # the unlocked resolver. Nix still consumes committed lock data.
          cargoLock.lockFile = ./nix/uniffi-dart-Cargo.lock;
          postPatch = ''
            cp ${./nix/uniffi-dart-Cargo.lock} Cargo.lock
          '';
          buildFeatures = [ "binary" ];
          cargoBuildFlags = [ "--bin=uniffi_bindgen_dart" ];
          doCheck = false;
        };

        # Dart bindings plus the pinned external generator.
        dartDeps = [
          pkgs.dart
          uniffi-bindgen-dart
        ];

        # The libobs headers, unpacked from the same OBS release
        # cpp/obs/buildspec.json downloads. Headers only: nothing here links, so
        # it works on Darwin even though obs-studio itself does not build there,
        # and it costs ~2 MB of store instead of the multi-hundred-MB obs-deps
        # bundle. `just obs compile` type-checks the plugin against it.
        #
        # Keep `version` equal to buildspec.json's obs-studio version; `just obs
        # check` fails when the two drift, and when either drifts a minor
        # release from the nixpkgs obs-studio below.
        obs-headers = pkgs.stdenvNoCC.mkDerivation rec {
          pname = "libobs-headers";
          version = "32.2.2";

          src = pkgs.fetchzip {
            url = "https://github.com/obsproject/obs-studio/archive/refs/tags/${version}.tar.gz";
            hash = "sha256-miLe4MhiVhLlPvwzDjL31BwdcjVhgWvmcSzH9pgZV6U=";
          };

          dontBuild = true;

          # The layout matches a real OBS install (everything flat under
          # include/obs, frontend API included), so a plugin's include paths are
          # the same here as against the system package on Linux.
          installPhase = ''
            runHook preInstall
            (cd libobs && find . \( -name '*.h' -o -name '*.hpp' \) -exec install -Dm444 {} $out/include/obs/{} \;)
            install -Dm444 frontend/api/obs-frontend-api.h $out/include/obs/obs-frontend-api.h
            # obs.h includes obsconfig.h, which the OBS build generates. Only the
            # two version macros have to hold a value; everything else it
            # configures is internal to libobs, so leave the #cmakedefines unset.
            sed -e 's/^#cmakedefine.*$//' -e 's/@OBS_RELEASE_CANDIDATE@/0/' -e 's/@OBS_BETA@/0/' \
              libobs/obsconfig.h.in > $out/include/obs/obsconfig.h
            runHook postInstall
          '';
        };

        # Dependencies for the OBS plugin (`just obs build`, `just obs compile`,
        # `just obs check`). ffmpeg + cmake come from rustDeps.
        #
        # Only the *build* is Linux-only: nixpkgs' obs-studio lists no Darwin
        # platform, so macOS and Windows link libobs/Qt6 from the OBS buildspec
        # bundle instead (see cpp/obs/buildspec.json and doc/bin/obs.md).
        # Type-checking needs headers rather than libraries, and those are
        # cross-platform -- obs-headers above, plus qt6.qtbase, which does build
        # on Darwin. So `just obs compile` and the lints run everywhere while
        # `just obs build` stays native.
        obsDeps =
          with pkgs;
          [
            obs-headers
            # libobs' public headers include <simde/x86/sse2.h> as of OBS 32,
            # which used to be vendored under libobs/util/ and so travelled with
            # obs-headers. obs-deps ships it beside libobs for the macOS and
            # Windows builds, and nixpkgs' obs-studio propagates it for the
            # Linux one; listed here so the header-only path has it on Darwin
            # too. Same 0.8.2 that obs-deps carries.
            simde
            qt6.qtbase
            clang-tools
            gersemi
          ]
          ++ lib.optionals (!stdenv.hostPlatform.isDarwin) [
            obs-studio
            ninja
          ];

        # The obs-studio `just obs ci` links against on Linux, which is a third
        # version alongside buildspec.json and obs-headers and the only one this
        # repo doesn't choose. Read the string on every platform, Darwin
        # included, so `just obs check` compares it everywhere rather than only
        # where the package builds. It moves when flake.lock does, so the guard
        # fails on the nixpkgs bump that opens the gap.
        obs-linked-version = pkgs.obs-studio.version;

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
              moq-token
            ];
          };

          # Inherit packages from the overlay
          inherit (overlayPkgs)
            moq-relay
            moq-cli
            moq-bench
            moq-token
            moq-token-cli
            moq-boy
            libmoq
            moq-gst
            ;

          inherit uniffi-bindgen-dart;

          # Bundle of packaging + repo-publish tooling, pinned via flake.lock.
          # CI builds this and prepends its bin/ to $PATH so subsequent steps
          # use the same versions a local `nix develop` user would.
          packaging = pkgs.symlinkJoin {
            name = "moq-packaging-tools";
            paths = packagingDeps ++ publishDeps;
          };
        });

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
          packages =
            rustDeps
            ++ jsDeps
            ++ pyDeps
            ++ cdnDeps
            ++ draftsDeps
            ++ packagingDeps
            ++ lintDeps
            ++ obsDeps
            ++ ktDeps
            ++ goDeps
            ++ dartDeps
            ++ devTools;

          # jemalloc's configure uses -O0 test builds, which conflict with
          # Nix's _FORTIFY_SOURCE hardening (requires -O).
          hardeningDisable = [ "fortify" ];

          # The pinned Rust toolchain goes on PATH ahead of everything the
          # host had, which shadows the Cargo shim `mbx setup` installs. Put it
          # back in front, so a bare `cargo` in this shell reaches the same
          # wrapper it reaches outside. `setup --status` is what knows where
          # that shim lives; it exits non-zero when the host has none, which is
          # every machine that made a different caching choice.
          #
          # Never in CI, where check.yml enters this shell and the `target/`
          # the workflow restored is the one the job has to build in. A shim
          # would silently move it into a machine-wide managed target instead,
          # on whichever runner happens to have been set up that way.
          shellHook = ''
            if [ -z "''${CI:-}" ] && status=$(mbx setup --status 2>/dev/null); then
              shim=$(printf '%s\n' "$status" | sed -n '1s/.*: //p')
              if [ -x "$shim" ]; then
                export PATH="$(dirname "$shim"):$PATH"
              fi
            fi
          '';

          env = {
            # Where `just obs compile` and `just obs test` look for libobs. Set
            # on every platform so the plugin type-checks against the pinned OBS
            # release everywhere, rather than whatever the host happens to have.
            OBS_INCLUDE_DIR = "${obs-headers}/include/obs";

            # What `just obs check` compares the two in-repo OBS pins against.
            # Exported rather than read back out of nix, so the guard costs a
            # variable lookup instead of a nested evaluation of this flake.
            OBS_LINKED_VERSION = obs-linked-version;
          }
          // pkgs.lib.optionalAttrs (!pkgs.stdenv.hostPlatform.isDarwin) {
            ALSA_PLUGIN_DIR = "${alsaPlugins}/lib/alsa-lib";
          };
        };

        formatter = pkgs.nixfmt-tree;

        # Heavy Rust CI (clippy / doc / test) runs via `just check` and `just
        # test` (see rs/justfile). CI and local commands default to plain Cargo;
        # local development can select a compatible wrapper with RUST_CARGO.
        # Neither path goes through crane.
        # `nix flake check` is kept -- it still validates flake eval + builds the
        # dev shell -- but no longer compiles the workspace, so it's cheap
        # enough that `just check` runs it on any Nix/Rust input change. Release
        # artifacts still build via crane `buildPackage` (see `packages` above /
        # release-*.yml).
        #
        # Which compiler cache those runs get is a workflow concern
        # (`.github/actions/rust-cache`); nothing here configures it.
        checks = {
          package-source-assets = pkgs.runCommand "package-source-assets" { } ''
            for asset in \
              rs/libmoq/moq.pc.in \
              rs/libmoq/native-libs/apple.txt \
              rs/libmoq/native-libs/linux.txt \
              rs/libmoq/native-libs/windows.txt
            do
              test -f "${overlayPkgs.libmoq.src}/$asset"
            done
            test -f "${overlayPkgs.moq-boy.src}/rs/moq-video/src/frame/nv12_resize.ptx"
            touch "$out"
          '';
        };
      }
    );
}
