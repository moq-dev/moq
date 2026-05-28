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

  moq-gst = craneLib.buildPackage (
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

      # moq-gst is a cdylib GStreamer plugin. Pick up the produced shared
      # library; crane's default install phase only handles binaries.
      installPhase = ''
        runHook preInstall

        mkdir -p $out/lib
        if [ -f target/release/libgstmoq.dylib ]; then
          cp target/release/libgstmoq.dylib $out/lib/
        else
          cp target/release/libgstmoq.so $out/lib/
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
        dylib="$out/lib/libgstmoq.dylib"
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
}
