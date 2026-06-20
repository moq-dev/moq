# Accept crane as argument to the overlay
{ crane }:
final: prev:
let
  # Pin crane to rust-overlay's latest stable so `nix build` uses the same
  # toolchain as `nix develop`. Without this, crane falls back to
  # `final.rustc`/`final.cargo`, which nixpkgs resolves to its default Rust
  # (currently 1.94) while the devShell pulls 1.95 from rust-overlay.
  #
  # Add both Apple targets so an aarch64-darwin host can cross-compile the
  # x86_64-darwin release artifacts (Apple's clang is multi-arch, so no
  # emulated x86_64 toolchain is needed). The default profile only ships
  # std for the host triple, which is why the target list is explicit.
  rustToolchain = final.rust-bin.stable.latest.default.override {
    targets = final.lib.optionals final.stdenv.isDarwin [
      "x86_64-apple-darwin"
      "aarch64-apple-darwin"
    ];
  };
  craneLib = (crane.mkLib final).overrideToolchain rustToolchain;

  # Helper function to get crate info from Cargo.toml
  crateInfo = cargoTomlPath: craneLib.crateNameFromCargoToml { cargoToml = cargoTomlPath; };

  # Cross-compile a crate's release artifact to x86_64-darwin from an
  # aarch64-darwin host. The Determinate Nix installer dropped Intel macOS
  # runners, but Apple's clang is multi-arch, so pointing cargo at the
  # target produces a native (non-emulated) x86_64 build. doCheck is off
  # because the x86_64 test binaries can't run in the aarch64 build sandbox.
  # Only valid for pure-Rust artifacts with no cross buildInputs; moq-gst's
  # GStreamer link would need pkgsCross instead.
  crossX86Darwin =
    args:
    args
    // {
      CARGO_BUILD_TARGET = "x86_64-apple-darwin";
      doCheck = false;
    };

  moqRelayArgs = crateInfo ../rs/moq-relay/Cargo.toml // {
    src = craneLib.cleanCargoSource ../.;
    cargoExtraArgs = "-p moq-relay --features jemalloc";
    # Enable frame pointers for profiling support (negligible overhead on x86_64).
    # This also ensures the CDN build matches what Cachix caches.
    RUSTFLAGS = "-C force-frame-pointers=yes";
    # jemalloc's configure uses -O0 test builds, which conflict with
    # Nix's _FORTIFY_SOURCE hardening (requires -O).
    hardeningDisable = [ "fortify" ];
  };

  moqCliArgs = crateInfo ../rs/moq-cli/Cargo.toml // {
    src = craneLib.cleanCargoSource ../.;
    cargoExtraArgs = "-p moq-cli";
  };

  moqTokenCliArgs = crateInfo ../rs/moq-token-cli/Cargo.toml // {
    src = craneLib.cleanCargoSource ../.;
    cargoExtraArgs = "-p moq-token-cli";
    meta.mainProgram = "moq-token-cli";
  };

  libmoqInfo = crateInfo ../rs/libmoq/Cargo.toml;
  libmoqArgs = libmoqInfo // {
    # libmoq's build.rs reads moq.pc.in at compile time to generate the
    # pkgconfig file. craneLib.cleanCargoSource's default filter drops
    # .pc.in files, which makes build.rs silently skip pkgconfig
    # generation (see the `if let Ok(template)` in rs/libmoq/build.rs)
    # and the installPhase's `cp .../moq.pc` then fails.
    src = final.lib.cleanSourceWith {
      src = ../.;
      name = "source";
      filter = path: type: (final.lib.hasSuffix ".pc.in" path) || (craneLib.filterCargoSources path type);
    };
    cargoExtraArgs = "-p libmoq";
    doCheck = false;
    nativeBuildInputs = with final; [ pkg-config ];

    # libmoq.a carries moq-ffi's whole dep tree, so an unstripped build is
    # ~75 MB+. Thin LTO with a single codegen unit dead-strips the unused
    # monomorphizations Rust bakes into a staticlib, halving the artifact
    # with no source or ABI change, which keeps the release tarball and
    # brew download small. Mirrors rs/libmoq/build.sh's Windows cargo path.
    CARGO_PROFILE_RELEASE_LTO = "thin";
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS = "1";

    # libmoq is a staticlib; crane's default install phase only handles
    # binaries. Lay out the artifact tree the way release tarballs and
    # downstream `find_package(moq)` consumers already expect.
    installPhase = ''
      runHook preInstall

      mkdir -p $out/lib/pkgconfig $out/include $out/lib/cmake/moq

      # build.rs derives its output dir from OUT_DIR, so a cross --target
      # build puts the staticlib, header and pkgconfig (under lib/) below
      # target/<triple>/. Keep the prefix target-aware so the native and
      # cross outputs share one installPhase.
      tdir="target''${CARGO_BUILD_TARGET:+/$CARGO_BUILD_TARGET}"
      cp "$tdir/release/libmoq.a" $out/lib/
      cp "$tdir/include/moq.h" $out/include/
      cp "$tdir/lib/pkgconfig/moq.pc" $out/lib/pkgconfig/

      # build.rs points libdir at the raw cargo target tree's profile dir
      # (../../<profile>). The installPhase puts the staticlib in $out/lib
      # alongside pkgconfig/, so rewrite libdir one level up. Match the whole
      # line so this is independent of the profile name and the exact .pc
      # template. Stays relocatable; no build-time path leaks into the store.
      sed -i 's#^libdir=.*#libdir=''${pcfiledir}/..#' $out/lib/pkgconfig/moq.pc

      major_version="$(echo "${libmoqInfo.version}" | cut -d. -f1)"
      substitute ${../rs/libmoq/cmake/moq-config.cmake.in} \
        $out/lib/cmake/moq/moq-config.cmake \
        --subst-var-by LIB_FILE libmoq.a \
        --subst-var-by VERSION "${libmoqInfo.version}"
      substitute ${../rs/libmoq/cmake/moq-config-version.cmake.in} \
        $out/lib/cmake/moq/moq-config-version.cmake \
        --subst-var-by VERSION "${libmoqInfo.version}" \
        --subst-var-by MAJOR_VERSION "$major_version"

      runHook postInstall
    '';
  };

  # Native x86_64-darwin package set (matches cache.nixos.org's prebuilt
  # binaries), used to link the cross moq-gst plugin against an x86_64
  # GStreamer. pkgsCross would rebuild GStreamer from source under a cross
  # stdenv; this fetches it. Lazy, so it's only instantiated when the cross
  # plugin is actually built (aarch64-darwin only, see flake.nix).
  pkgsX86Darwin = import final.path { system = "x86_64-darwin"; };

  moqGstPluginArgs = crateInfo ../rs/moq-gst/Cargo.toml // {
    src = craneLib.cleanCargoSource ../.;
    cargoExtraArgs = "-p moq-gst";
    doCheck = false;

    nativeBuildInputs = with final; [ pkg-config ];
    buildInputs = with final; [
      gst_all_1.gstreamer
      gst_all_1.gst-plugins-base
    ];

    # moq-gst is a cdylib GStreamer plugin. Install into lib/gstreamer-1.0
    # so gst_all_1.gstreamer's nixpkgs setup-hook (which scans every input
    # for that subdir) appends us to GST_PLUGIN_SYSTEM_PATH_1_0. Then
    #   nix shell .#moq-gst .#gst_all_1.gstreamer --command gst-inspect-1.0 moq
    # discovers moqsink/moqsrc without any env-var fiddling. Crane's
    # default install phase only handles binaries, so we copy by hand.
    installPhase = ''
      runHook preInstall

      # A cross --target build puts the cdylib under target/<triple>/.
      tdir="target''${CARGO_BUILD_TARGET:+/$CARGO_BUILD_TARGET}"
      mkdir -p $out/lib/gstreamer-1.0
      if [ -f "$tdir/release/libgstmoq.dylib" ]; then
        cp "$tdir/release/libgstmoq.dylib" $out/lib/gstreamer-1.0/
      else
        cp "$tdir/release/libgstmoq.so" $out/lib/gstreamer-1.0/
      fi

      runHook postInstall
    '';

    # The flake output is meant to load against nix's GStreamer (in a
    # `nix shell .#moq-gst` / cachix-pulled context). `/nix/store` refs
    # are correct there. The only thing we fix is the rustc-emitted
    # self-reference to /nix/var/nix/builds/.../libgstmoq.dylib (the
    # cargo build dir, gone post-build) which would break loading even
    # inside nix. rs/moq-gst/scrub.sh handles tarball / homebrew
    # portability separately. The `[ -f ]` guard skips crane's
    # deps-only stage, whose $out has no plugin.
    postFixup = final.lib.optionalString final.stdenv.isDarwin ''
      dylib="$out/lib/gstreamer-1.0/libgstmoq.dylib"
      if [ -f "$dylib" ]; then
        install_name_tool -id "@rpath/libgstmoq.dylib" "$dylib"

        # The rustc self-ref is the only LC_LOAD_DYLIB whose basename
        # matches our own and isn't already @rpath-prefixed. Rewriting
        # it to @rpath/libgstmoq.dylib matches LC_ID_DYLIB, so dyld
        # dedupes the load against the already-mapped image.
        otool -L "$dylib" \
          | tail -n +2 \
          | awk '{print $1}' \
          | { grep -E '/libgstmoq\.dylib$' || true; } \
          | { grep -v '^@' || true; } \
          | while read -r self_ref; do
              install_name_tool -change "$self_ref" "@rpath/libgstmoq.dylib" "$dylib"
            done

        # Assert no build-sandbox paths leaked. /nix/store refs are
        # fine here, see top comment.
        bad="$(otool -L "$dylib" \
          | tail -n +2 \
          | awk '{print $1}' \
          | { grep '^/nix/var/' || true; })"
        if [ -n "$bad" ]; then
          echo "ERROR: $dylib has /nix/var build-sandbox LC_LOAD_DYLIB entries:" >&2
          echo "$bad" >&2
          exit 1
        fi
      fi
    '';
  };

  # --- CI checks (run via `nix flake check`) -------------------------------
  #
  # The heavy Rust CI (clippy / doc / test) used to run as plain `cargo` on the
  # self-hosted runner, compiling into a persistent CARGO_TARGET_DIR that grew
  # unbounded (every branch's artifacts, never pruned) and was unsafe to share
  # across parallel jobs. Expressing those checks as crane derivations instead
  # makes Nix the cache: dependencies compile exactly once (`checkDeps`), land
  # in the binary cache, are shared across machines, and are LRU-evicted by the
  # store's free-space GC. Only first-party code recompiles, inside Nix's
  # sandbox (which is wiped per build), so there is no target dir to bound.

  # crane's cleanCargoSource keeps only Rust-relevant files, but the test
  # targets compiled under `--all-targets` `include_str!`/`include_bytes!` data
  # fixtures (rs/moq-mux test_data/*.ts, rs/moq-json tests/vectors.json, etc.)
  # and libmoq's build.rs reads moq.pc.in. Rather than enumerate every asset
  # (fragile -- a new fixture would break CI), take the whole tree minus the
  # heavy non-source dirs. Dep caching is unaffected (buildDepsOnly keys on
  # Cargo.lock); only the first-party check rebuilds on a non-Rust change.
  checkSrc = final.lib.cleanSourceWith {
    src = ../.;
    name = "source";
    filter =
      path: _type:
      let
        # Prune a dir both as an entry ("…/target") and its contents
        # ("…/target/…") so we don't walk node_modules etc. at all.
        excluded = name: final.lib.hasSuffix "/${name}" path || final.lib.hasInfix "/${name}/" path;
      in
      !(
        excluded "node_modules"
        || excluded "target"
        || excluded ".direnv"
        || excluded ".venv"
        || excluded ".git"
      );
  };

  # Every system lib the workspace needs to compile with all features on:
  # bindgen (clang) for ffmpeg/vaapi, GStreamer for moq-gst, ALSA for
  # moq-audio's capture feature, libva for moq-video's VAAPI backend. The
  # Linux-only libs match the devShell's `lib.optionals (!isDarwin)` set.
  checkCommonArgs = {
    src = checkSrc;
    # The workspace root Cargo.toml has no [package], so name it explicitly
    # (otherwise crane warns and uses a placeholder).
    pname = "moq-workspace";
    version = "0.0.0";
    strictDeps = true;
    nativeBuildInputs = with final; [
      pkg-config
      clang
      cmake
      # boring-sys (quiche, under --all-features) shells out to `git` to apply
      # its BoringSSL patches; the devShell has it on $PATH, the sandbox doesn't.
      git
    ];
    buildInputs =
      (with final; [
        ffmpeg
        glib
        curl
        gst_all_1.gstreamer
        gst_all_1.gst-plugins-base
      ])
      ++ final.lib.optionals final.stdenv.isLinux (
        with final;
        [
          alsa-lib
          libva
        ]
      );
    LIBCLANG_PATH = "${final.libclang.lib}/lib";
    # jemalloc (pulled in by --all-features) builds -O0 configure tests that
    # clash with Nix's _FORTIFY_SOURCE hardening (needs -O). Same as the
    # package builds and the devShell.
    hardeningDisable = [ "fortify" ];
    # Scope the dep build to the whole workspace with every feature so a single
    # checkDeps covers clippy/doc/test below.
    cargoExtraArgs = "--workspace --all-features";
  };

  # Compile all third-party deps once; the checks reuse this artifact.
  checkDeps = craneLib.buildDepsOnly (checkCommonArgs // { pname = "moq-workspace-deps"; });

  moqChecks = {
    clippy = craneLib.cargoClippy (
      checkCommonArgs
      // {
        cargoArtifacts = checkDeps;
        # --workspace/--all-features come from checkCommonArgs.cargoExtraArgs,
        # which crane prepends; only add the clippy-specific flags here.
        cargoClippyExtraArgs = "--all-targets -- -D warnings";
      }
    );

    doc = craneLib.cargoDoc (
      checkCommonArgs
      // {
        cargoArtifacts = checkDeps;
        # --all-targets is invalid for `cargo doc`, so it stays out of the
        # shared cargoExtraArgs; --workspace/--all-features are inherited.
        cargoDocExtraArgs = "--no-deps";
        RUSTDOCFLAGS = "-D warnings";
      }
    );

    test = craneLib.cargoTest (
      checkCommonArgs
      // {
        cargoArtifacts = checkDeps;
        cargoTestExtraArgs = "--all-targets";
      }
    );
  };
in
{
  inherit moqChecks;

  moq-relay = craneLib.buildPackage moqRelayArgs;
  moq-relay-x86_64-apple-darwin = craneLib.buildPackage (crossX86Darwin moqRelayArgs);

  moq-cli = craneLib.buildPackage moqCliArgs;
  moq-cli-x86_64-apple-darwin = craneLib.buildPackage (crossX86Darwin moqCliArgs);

  moq-token-cli = craneLib.buildPackage moqTokenCliArgs;
  moq-token-cli-x86_64-apple-darwin = craneLib.buildPackage (crossX86Darwin moqTokenCliArgs);

  moq-boy = craneLib.buildPackage (
    crateInfo ../rs/moq-boy/Cargo.toml
    // {
      src = craneLib.cleanCargoSource ../.;
      cargoExtraArgs = "-p moq-boy --features jemalloc";
      nativeBuildInputs = with final; [
        pkg-config
        clang
      ];
      buildInputs = with final; [ ffmpeg ];
      LIBCLANG_PATH = "${final.libclang.lib}/lib";
      # Enable frame pointers for profiling support (negligible overhead on x86_64).
      RUSTFLAGS = "-C force-frame-pointers=yes";
      # jemalloc's configure uses -O0 test builds, which conflict with
      # Nix's _FORTIFY_SOURCE hardening (requires -O).
      hardeningDisable = [ "fortify" ];
    }
  );

  libmoq = craneLib.buildPackage libmoqArgs;
  libmoq-x86_64-apple-darwin = craneLib.buildPackage (crossX86Darwin libmoqArgs);

  moq-gst-plugin = craneLib.buildPackage moqGstPluginArgs;

  # Cross plugin links the x86_64 GStreamer so the cdylib's LC_LOAD_DYLIB
  # entries point at x86_64 libs. The release build (rs/moq-gst/build.sh)
  # scrubs those nix paths to the user's system GStreamer and skips the
  # gst-inspect smoke test, which can't load an x86_64 plugin under the
  # arm runner's arm gst-inspect.
  moq-gst-plugin-x86_64-apple-darwin = craneLib.buildPackage (
    crossX86Darwin (
      moqGstPluginArgs
      // {
        buildInputs = [
          pkgsX86Darwin.gst_all_1.gstreamer
          pkgsX86Darwin.gst_all_1.gst-plugins-base
        ];
      }
    )
  );

  # User-facing flake output. Bundles the plugin with wrapped gstreamer
  # tools so a single `nix shell .#moq-gst` gives you gst-inspect-1.0 /
  # gst-launch-1.0 that already know about the moq plugin plus the usual
  # base/good/bad plugin set, matching the "install a plugin and the
  # standard tools find it" UX. `nix shell` (unlike nix-shell / nix
  # develop) doesn't run nixpkgs setup-hooks, so a bare lib/gstreamer-1.0
  # directory in $out isn't enough on its own.
  moq-gst =
    let
      pluginPaths = final.lib.concatStringsSep ":" [
        "${final.moq-gst-plugin}/lib/gstreamer-1.0"
        # gstreamer.out (vs .bin) holds the core plugins (coreelements,
        # coretracers): identity, queue, fakesink, capsfilter, etc.
        "${final.gst_all_1.gstreamer.out}/lib/gstreamer-1.0"
        "${final.gst_all_1.gst-plugins-base}/lib/gstreamer-1.0"
        "${final.gst_all_1.gst-plugins-good}/lib/gstreamer-1.0"
        "${final.gst_all_1.gst-plugins-bad}/lib/gstreamer-1.0"
      ];
    in
    final.symlinkJoin {
      name = "moq-gst-${final.moq-gst-plugin.version}";
      paths = [ final.moq-gst-plugin ];
      nativeBuildInputs = [ final.makeWrapper ];
      postBuild = ''
        rm -rf $out/bin
        mkdir -p $out/bin
        for tool in gst-inspect-1.0 gst-launch-1.0; do
          makeWrapper "${final.gst_all_1.gstreamer.bin}/bin/$tool" "$out/bin/$tool" \
            --suffix GST_PLUGIN_SYSTEM_PATH_1_0 : "${pluginPaths}"
        done
      '';
      meta.mainProgram = "gst-inspect-1.0";
    };
}
