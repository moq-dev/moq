# Accept crane as argument to the overlay
{ crane }:
final: prev:
let
  # Pin crane to rust-overlay's latest stable so `nix build` uses the same
  # toolchain as `nix develop`. Without this, crane falls back to
  # `final.rustc`/`final.cargo`, which nixpkgs resolves to its default Rust
  # (currently 1.94) while the devShell pulls 1.95 from rust-overlay.
  craneLib = (crane.mkLib final).overrideToolchain final.rust-bin.stable.latest.default;

  # Helper function to get crate info from Cargo.toml
  crateInfo = cargoTomlPath: craneLib.crateNameFromCargoToml { cargoToml = cargoTomlPath; };
in
{
  moq-relay = craneLib.buildPackage (
    crateInfo ../rs/moq-relay/Cargo.toml
    // {
      src = craneLib.cleanCargoSource ../.;
      cargoExtraArgs = "-p moq-relay --features jemalloc";
      # Enable frame pointers for profiling support (negligible overhead on x86_64).
      # This also ensures the CDN build matches what Cachix caches.
      RUSTFLAGS = "-C force-frame-pointers=yes";
      # jemalloc's configure uses -O0 test builds, which conflict with
      # Nix's _FORTIFY_SOURCE hardening (requires -O).
      hardeningDisable = [ "fortify" ];
    }
  );

  moq-cli = craneLib.buildPackage (
    crateInfo ../rs/moq-cli/Cargo.toml
    // {
      src = craneLib.cleanCargoSource ../.;
      cargoExtraArgs = "-p moq-cli";
    }
  );

  moq-token-cli = craneLib.buildPackage (
    crateInfo ../rs/moq-token-cli/Cargo.toml
    // {
      src = craneLib.cleanCargoSource ../.;
      cargoExtraArgs = "-p moq-token-cli";
      meta.mainProgram = "moq-token-cli";
    }
  );

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

  libmoq =
    let
      info = crateInfo ../rs/libmoq/Cargo.toml;
    in
    craneLib.buildPackage (
      info
      // {
        # libmoq's build.rs reads moq.pc.in at compile time to generate the
        # pkgconfig file. craneLib.cleanCargoSource's default filter drops
        # .pc.in files, which makes build.rs silently skip pkgconfig
        # generation (see the `if let Ok(template)` in rs/libmoq/build.rs)
        # and the installPhase's `cp target/pkgconfig/moq.pc` then fails.
        src = final.lib.cleanSourceWith {
          src = ../.;
          name = "source";
          filter = path: type: (final.lib.hasSuffix ".pc.in" path) || (craneLib.filterCargoSources path type);
        };
        cargoExtraArgs = "-p libmoq";
        doCheck = false;
        nativeBuildInputs = with final; [ pkg-config ];

        # libmoq is a staticlib; crane's default install phase only handles
        # binaries. Lay out the artifact tree the way release tarballs and
        # downstream `find_package(moq)` consumers already expect.
        installPhase = ''
          runHook preInstall

          mkdir -p $out/lib/pkgconfig $out/include $out/lib/cmake/moq
          cp target/release/libmoq.a $out/lib/
          cp target/include/moq.h $out/include/
          cp target/pkgconfig/moq.pc $out/lib/pkgconfig/

          major_version="$(echo "${info.version}" | cut -d. -f1)"
          substitute ${../rs/libmoq/cmake/moq-config.cmake.in} \
            $out/lib/cmake/moq/moq-config.cmake \
            --subst-var-by LIB_FILE libmoq.a \
            --subst-var-by VERSION "${info.version}"
          substitute ${../rs/libmoq/cmake/moq-config-version.cmake.in} \
            $out/lib/cmake/moq/moq-config-version.cmake \
            --subst-var-by VERSION "${info.version}" \
            --subst-var-by MAJOR_VERSION "$major_version"

          runHook postInstall
        '';
      }
    );

  moq-gst-plugin = craneLib.buildPackage (
    crateInfo ../rs/moq-gst/Cargo.toml
    // {
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

        mkdir -p $out/lib/gstreamer-1.0
        if [ -f target/release/libgstmoq.dylib ]; then
          cp target/release/libgstmoq.dylib $out/lib/gstreamer-1.0/
        else
          cp target/release/libgstmoq.so $out/lib/gstreamer-1.0/
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
    }
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
