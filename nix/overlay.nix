final: prev:
let
  # Pin the build toolchain to rust-toolchain.toml's and flake.nix's so `nix
  # build` uses what `nix develop` does and release artifacts build with the
  # version CI verifies. `minimal`: a build needs no docs or rustfmt.
  rustToolchain = final.rust-bin.stable."1.98.1".minimal;

  crates = import ./crates.nix {
    pkgs = final;
    inherit rustToolchain;
  };

  # Frame pointers, so profiles of the deployed binaries resolve complete call
  # stacks (negligible overhead on x86_64). nix.moq.dev caches these builds.
  framePointers = [ "-C force-frame-pointers=yes" ];

  # rustls-platform-verifier reads the system CA bundle. A scratch image has
  # no /etc/ssl, so the package output references cacert and the image links
  # the well-known path at this bundle. The symlink is what puts cacert in
  # the runtime closure.
  caBundle = ''
    mkdir -p "$out/etc/ssl/certs"
    ln -s ${final.cacert}/etc/ssl/certs/ca-bundle.crt "$out/etc/ssl/certs/ca-certificates.crt"
  '';

  # The native libraries an external linker must pass alongside libmoq.a.
  # rs/moq-c/native-libs/ is the single source: rs/moq-c/CMakeLists.txt reads
  # it for in-tree consumers, and moq-c below substitutes it into the moq-c.pc
  # and find_package templates directly rather than running CMake, so format
  # it the way each expects: `Libs.private` flags, and CMakeLists.txt's
  # MOQ_NATIVE_LIBS_QUOTED.
  moqCNativeLibs =
    let
      platform =
        if final.stdenv.hostPlatform.isDarwin then
          "apple"
        else if final.stdenv.hostPlatform.isWindows then
          "windows"
        else
          "linux";
      lines = final.lib.splitString "\n" (builtins.readFile ../rs/moq-c/native-libs/${platform}.txt);
      entries = builtins.filter (line: line != "" && !(final.lib.hasPrefix "#" line)) lines;
      framework = entry: "-framework ${final.lib.removePrefix "framework:" entry}";
      isFramework = final.lib.hasPrefix "framework:";
    in
    {
      pc = final.lib.concatMapStringsSep " " (
        entry: if isFramework entry then framework entry else "-l${entry}"
      ) entries;
      cmake = final.lib.concatMapStringsSep " " (
        entry: if isFramework entry then ''"${framework entry}"'' else ''"${entry}"''
      ) entries;
    };

  # Releases carry the OpenH264 software fallback, which is opt-in.
  moqCCrate = crates.build {
    root = "moq-c";
    features = [ "openh264" ];
    # Cargo.toml's `[lib] name`, which crate2nix drops for a staticlib-only
    # crate, so the archive would be libmoq_c.
    rootAttrs.libName = "moq";
  };

  moqGstCrate = crates.build { root = "moq-gst"; };
in
{
  moq-relay = crates.build {
    root = "moq-relay";
    features = [ "jemalloc" ];
    rustflags = framePointers;
    rootAttrs.postInstall = caBundle;
  };

  moq-cli = crates.build {
    root = "moq-cli";
    features = [ "jemalloc" ];
    rustflags = framePointers;
    rootAttrs = {
      postInstall = caBundle;
      # The crate is `moq-cli`, but its `[[bin]]` ships as `moq`.
      meta.mainProgram = "moq";
    };
  };

  moq-bench = crates.build { root = "moq-bench"; };

  moq-boy = crates.build {
    root = "moq-boy";
    # moq-boy encodes 160x144, below every hardware encoder's minimum.
    features = [
      "jemalloc"
      "openh264"
    ];
    rustflags = framePointers;
  };

  # Lay out the artifact tree the way release tarballs and downstream
  # `find_package(moq-c)` consumers already expect.
  moq-c = final.stdenv.mkDerivation {
    pname = "moq-c";
    inherit (moqCCrate) version;
    dontUnpack = true;
    nativeBuildInputs = [ final.pkg-config ];
    passthru = { inherit (moqCCrate) deps; };

    installPhase = ''
      runHook preInstall

      mkdir -p $out/lib/pkgconfig $out/include $out/lib/cmake/moq-c

      # buildRustCrate suffixes the archive with the crate's metadata hash, and
      # keeps build.rs's OUT_DIR, where it writes the header.
      cp ${moqCCrate.lib}/lib/libmoq-*.a $out/lib/libmoq.a
      cp ${moqCCrate.lib}/lib/moq-c.out/include/moq.h $out/include/

      # Rendered here rather than by build.rs: the template's paths are relative
      # to the .pc, which only holds once libmoq.a sits beside pkgconfig/.
      pc=$out/lib/pkgconfig/moq-c.pc
      substitute ${../rs/moq-c/moq-c.pc.in} "$pc" \
        --subst-var-by VERSION "$version" \
        --subst-var-by LIBS_PRIVATE ${final.lib.escapeShellArg moqCNativeLibs.pc}
      if grep -nE '@[A-Z_]+@' "$pc"; then
        echo "unsubstituted placeholder in moq-c.pc (see above)" >&2
        exit 1
      fi
      # Resolve the paths the way a consumer's pkg-config will, so a template
      # whose libdir or includedir misses what this prefix ships fails here.
      for check in libdir:libmoq.a includedir:moq.h; do
        dir=$(pkg-config --variable="''${check%%:*}" "$pc")
        if [ ! -f "$dir/''${check#*:}" ]; then
          echo "moq-c.pc ''${check%%:*} $dir has no ''${check#*:}" >&2
          exit 1
        fi
      done

      substitute ${../rs/moq-c/cmake/moq-c-config.cmake.in} \
        $out/lib/cmake/moq-c/moq-c-config.cmake \
        --subst-var-by LIB_FILE libmoq.a \
        --subst-var-by VERSION "$version" \
        --subst-var-by MOQ_NATIVE_LIBS_QUOTED ${final.lib.escapeShellArg moqCNativeLibs.cmake}
      substitute ${../rs/moq-c/cmake/moq-c-config-version.cmake.in} \
        $out/lib/cmake/moq-c/moq-c-config-version.cmake \
        --subst-var-by VERSION "$version" \
        --subst-var-by MAJOR_VERSION "''${version%%.*}"

      runHook postInstall
    '';
  };

  # moq-gst is a cdylib GStreamer plugin. Install into lib/gstreamer-1.0
  # so gst_all_1.gstreamer's nixpkgs setup-hook (which scans every input
  # for that subdir) appends us to GST_PLUGIN_SYSTEM_PATH_1_0. Then
  #   nix shell .#moq-gst .#gst_all_1.gstreamer --command gst-inspect-1.0 moq
  # discovers moqsink/moqsrc without any env-var fiddling.
  moq-gst-plugin = final.stdenv.mkDerivation {
    pname = "moq-gst-plugin";
    inherit (moqGstCrate) version;
    dontUnpack = true;
    passthru = { inherit (moqGstCrate) deps; };

    installPhase = ''
      runHook preInstall
      mkdir -p $out/lib/gstreamer-1.0
      cp ${moqGstCrate.lib}/lib/libgstmoq-*${final.stdenv.hostPlatform.extensions.sharedLibrary} \
        $out/lib/gstreamer-1.0/libgstmoq${final.stdenv.hostPlatform.extensions.sharedLibrary}
      runHook postInstall
    '';

    # The flake output is meant to load against nix's GStreamer (in a
    # `nix shell .#moq-gst` / binary-cache-pulled context). `/nix/store` refs
    # are correct there. The only thing we fix is the rustc-emitted
    # self-reference to the build directory's libgstmoq dylib (gone
    # post-build), which would break loading even inside nix.
    # rs/moq-gst/scrub.sh handles tarball / homebrew portability separately.
    postFixup = final.lib.optionalString final.stdenv.hostPlatform.isDarwin ''
      dylib="$out/lib/gstreamer-1.0/libgstmoq.dylib"
      install_name_tool -id "@rpath/libgstmoq.dylib" "$dylib"

      # The rustc self-ref is the only LC_LOAD_DYLIB whose basename
      # matches our own and isn't already @rpath-prefixed. Rewriting
      # it to @rpath/libgstmoq.dylib matches LC_ID_DYLIB, so dyld
      # dedupes the load against the already-mapped image.
      otool -L "$dylib" \
        | tail -n +2 \
        | awk '{print $1}' \
        | { grep -E '/libgstmoq(-[0-9a-f]+)?\.dylib$' || true; } \
        | { grep -v '^@' || true; } \
        | while read -r self_ref; do
            install_name_tool -change "$self_ref" "@rpath/libgstmoq.dylib" "$dylib"
          done

      # Assert no build-sandbox paths leaked. /nix/store refs are
      # fine here, see top comment.
      bad="$(otool -L "$dylib" \
        | tail -n +2 \
        | awk '{print $1}' \
        | { grep -E '^/(nix/var|private)/' || true; })"
      if [ -n "$bad" ]; then
        echo "ERROR: $dylib has build-sandbox LC_LOAD_DYLIB entries:" >&2
        echo "$bad" >&2
        exit 1
      fi
    '';
  };

  # User-facing flake output. Bundles the plugin with wrapped gstreamer
  # tools so a single `nix shell .#moq-gst` gives you gst-inspect-1.0 /
  # gst-launch-1.0 that already know about the moq plugin plus the usual
  # base/good/bad/ugly/libav plugin set, matching the "install a plugin and the
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
        "${final.gst_all_1.gst-plugins-ugly}/lib/gstreamer-1.0"
        "${final.gst_all_1.gst-libav}/lib/gstreamer-1.0"
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
        export GST_REGISTRY="$TMPDIR/gst-registry.bin"
        for element in moq x264enc avenc_aac avdec_h264; do
          $out/bin/gst-inspect-1.0 "$element" > /dev/null
        done
      '';
      meta.mainProgram = "gst-inspect-1.0";
    };
}
