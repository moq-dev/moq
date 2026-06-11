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

  # ffmpeg for the moq-cli `capture` feature (camera + H.264 encode), built so
  # we can statically link it: the shipped CLI then has no runtime ffmpeg
  # dependency. We disable nixpkgs' dependency presets and re-enable only the
  # libav* libraries ffmpeg-next links (codec/format/filter/device plus
  # swscale/swresample). v4l2 covers Linux camera capture and the v4l2m2m
  # hardware encoder; videotoolbox is auto-detected on darwin; both are
  # link-dep-free. Everything else (x265/aom/dav1d/gnutls/bzip2/...) stays off,
  # which keeps the static link closure small and avoids dragging in libraries
  # that have no static archive on the link path.
  #
  # `gpl` adds libx264, the software H.264 encoder, which is GPL. The shipped
  # binary keeps it OFF so the distributed artifact stays LGPL (the hardware
  # encoders h264_videotoolbox / h264_v4l2m2m are LGPL ffmpeg built-ins, as are
  # the mjpeg/rawvideo decoders the camera input needs). The dev shell turns it
  # ON: that ffmpeg is a local build tool, never distributed, and the test
  # suite and `demo/pub` recipes both want a software H.264 encoder.
  #
  # `withStatic` adds the .a archives; we leave `withShared` at its default
  # (on, except under pkgsStatic) so the same package also serves plain dynamic
  # dev builds (`cargo build --features capture`, without `moq-video/static`)
  # and the `ffmpeg` CLI the demo recipes use.
  moqFfmpegFor =
    {
      pkgs,
      gpl ? false,
    }:
    (pkgs.ffmpeg.override (
      {
        withHeadlessDeps = false;
        withSmallDeps = false;
        withFullDeps = false;
        buildAvcodec = true;
        buildAvformat = true;
        buildAvutil = true;
        buildAvdevice = true;
        buildAvfilter = true;
        buildSwscale = true;
        buildSwresample = true;
        buildFfmpeg = true;
        buildFfprobe = true;
        withV4l2 = pkgs.stdenv.hostPlatform.isLinux;
        withZlib = true;
        withStatic = true;
        withGPL = gpl;
        withX264 = gpl;
      }
      // final.lib.optionalAttrs gpl { x264 = moqX264For pkgs; }
    )).overrideAttrs
      (_: {
        # ffmpeg's bundled tests (e.g. libavutil/tests/pixelutils.c) miss a
        # <string.h> include and fail under darwin's strict clang, which only
        # treats the implicit-declaration as a warning for GNU cc. We only need
        # the libraries, so skip the self-test build.
        doCheck = false;
      });

  # nixpkgs' x264 hardcodes the `install-lib-shared` make target and declares a
  # separate `lib` output; with `enableShared = false` it builds only libx264.a
  # and the `lib` output comes up empty, which nix rejects. Install the static
  # lib instead and collapse to one output so the .a / .pc / header land where
  # pkg-config and the static ffmpeg link can find them. Only the dev-shell
  # (GPL) ffmpeg references it.
  moqX264For =
    pkgs:
    (pkgs.x264.override { enableShared = false; }).overrideAttrs (_: {
      outputs = [ "out" ];
      makeFlags = [ "install-lib-static" ];
    });

  # The dev shell links this (with libx264) so `just rs ci`'s `--all-features`
  # test and the demo recipes have a software H.264 encoder; it is never
  # distributed. The shipped builds use the LGPL variant via captureBuildArgs.
  moqFfmpeg = moqFfmpegFor {
    pkgs = final;
    gpl = true;
  };
  moqX264 = moqX264For final;

  # Build inputs that let ffmpeg-sys-next find and statically link the libav*
  # archives (pkg-config + bindgen's clang). LGPL (no libx264): the shipped
  # binary encodes with the platform hardware encoder only.
  captureBuildArgs =
    pkgs:
    {
      nativeBuildInputs = with final; [
        pkg-config
        clang
      ];
      buildInputs = [ (moqFfmpegFor { inherit pkgs; }) ];
      LIBCLANG_PATH = "${final.libclang.lib}/lib";
    }
    // final.lib.optionalAttrs pkgs.stdenv.hostPlatform.isDarwin {
      # ffmpeg-sys-next's static-macos path hardcodes -framework QTKit (and other
      # legacy frameworks) into the link line. QTKit was removed from macOS, so
      # the binary links against the SDK stub but dyld aborts at runtime ("no
      # such file"). ffmpeg references no QTKit symbols, so -dead_strip_dylibs
      # drops the load commands for frameworks nothing actually uses.
      RUSTFLAGS = "-C link-arg=-Wl,-dead_strip_dylibs";
    };

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

  # Capture is on so the released CLI can publish a webcam out of the box.
  # `moq-video/static` statically links ffmpeg (see moqFfmpeg) so there's no
  # runtime libav* dependency. On macOS the cross x86_64 output below swaps in
  # an x86_64 ffmpeg; the Linux release uses the fully-static moq-cli-static.
  moqCliArgs =
    crateInfo ../rs/moq-cli/Cargo.toml
    // captureBuildArgs final
    // {
      src = craneLib.cleanCargoSource ../.;
      cargoExtraArgs = "-p moq-cli --features capture,moq-video/static";
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
      # build puts the staticlib, header and pkgconfig under target/<triple>/.
      # Keep the prefix target-aware so the native and cross outputs share
      # one installPhase.
      tdir="target''${CARGO_BUILD_TARGET:+/$CARGO_BUILD_TARGET}"
      cp "$tdir/release/libmoq.a" $out/lib/
      cp "$tdir/include/moq.h" $out/include/
      cp "$tdir/pkgconfig/moq.pc" $out/lib/pkgconfig/

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
in
{
  moq-relay = craneLib.buildPackage moqRelayArgs;
  moq-relay-x86_64-apple-darwin = craneLib.buildPackage (crossX86Darwin moqRelayArgs);

  # Exposed so flake.nix's dev shell links the same static-capable ffmpeg the
  # package builds use (so `just rs ci`'s `--all-features` test can statically
  # link without a second ffmpeg on the pkg-config path).
  inherit moqFfmpeg moqX264;

  moq-cli = craneLib.buildPackage moqCliArgs;
  # The cross build links an x86_64 ffmpeg (built natively by pkgsX86Darwin,
  # like the moq-gst cross plugin) so capture's static link resolves x86_64
  # archives. crossX86Darwin's arg merge keeps captureBuildArgs' nativeBuildInputs.
  moq-cli-x86_64-apple-darwin = craneLib.buildPackage (
    crossX86Darwin (moqCliArgs // { buildInputs = (captureBuildArgs pkgsX86Darwin).buildInputs; })
  );

  # Fully static musl build for the portable Linux release artifacts. Under
  # pkgsStatic everything (ffmpeg, x264, libc -> musl) links static by default,
  # so the binary has zero dynamic dependencies and runs on any Linux
  # regardless of glibc version. This replaces the old cargo-zigbuild glibc-2.34
  # pin: a static binary is strictly more portable. Linux-only (see flake.nix);
  # darwin can't fully static link.
  moq-cli-static =
    let
      staticPkgs = final.pkgsStatic;
      muslTarget = staticPkgs.stdenv.hostPlatform.rust.rustcTarget;
      craneLibStatic = (crane.mkLib staticPkgs).overrideToolchain (
        final.rust-bin.stable.latest.default.override { targets = [ muslTarget ]; }
      );
    in
    craneLibStatic.buildPackage (
      crateInfo ../rs/moq-cli/Cargo.toml
      // captureBuildArgs staticPkgs
      // {
        src = craneLib.cleanCargoSource ../.;
        cargoExtraArgs = "-p moq-cli --features capture,moq-video/static";
        CARGO_BUILD_TARGET = muslTarget;
        # Cross-target test binaries can't run in the build sandbox.
        doCheck = false;
      }
    );

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
