# @moq/libav: libav.js's opus-af variant plus ffmpeg's native AAC decoder.
#
# libav.js publishes no variant that decodes AAC (its MPEG configs are
# source-only), so this builds one from the tagged source. Only the
# single-threaded WebAssembly target is built: the frontend picks it whenever
# WebAssembly exists and the caller doesn't ask for threads, which the polyfill
# never does, so the asm.js and pthread targets would double the build and the
# package without ever loading.
#
# The output is the npm package itself. The root `postinstall` runs build.ts,
# which copies it into js/libav/dist, the directory that serves the workspace
# and that release.ts publishes.
{
  lib,
  stdenv,
  buildPackages,
  stdenvNoCC,
  fetchurl,
  fetchFromGitHub,
  importNpmLock,
  emscripten,
  nodejs,
  python3,
  pkg-config,
  jq,
}:
let
  version = "6.10.9.0";
  variant = "opus-aac-af";

  # opus-af's fragments plus the AAC parser and decoder.
  fragments = [
    "avformat"
    "avcodec"
    "format-ogg"
    "format-webm"
    "parser-opus"
    "codec-libopus"
    "parser-aac"
    "decoder-aac"
    "avfilter"
    "swresample"
    "audio-filters"
  ];

  # The tarballs libav.js's Makefile would curl, at the versions this tag pins.
  sources = {
    "ffmpeg-9.0.tar.xz" = fetchurl {
      url = "https://ffmpeg.org/releases/ffmpeg-9.0.tar.xz";
      hash = "sha256-f2B6AN0NKKcp1aSBEgWBLu8Bz272FVAl/rtvNqkGLVI=";
    };
    "opus-1.6.1.tar.gz" = fetchurl {
      url = "https://downloads.xiph.org/releases/opus/opus-1.6.1.tar.gz";
      hash = "sha256-b/y1kyB76SWE3xWzJGbtZLvsmRCfAHyCIF8BlFckEaE=";
    };
    "emfiberthreads-1.3.tar.gz" = fetchurl {
      url = "https://github.com/Yahweasel/emfiberthreads/archive/refs/tags/v1.3.tar.gz";
      hash = "sha256-D+kz13Qt47MhNgjg+wh6GuDQYGBNWDX3yc387g/Iy8s=";
    };
  };

  # The expensive part, kept apart from the manifest so a version bump in
  # package.json doesn't recompile ffmpeg.
  wasm = stdenv.mkDerivation (finalAttrs: {
    pname = "libav.js-${variant}";
    inherit version;

    src = fetchFromGitHub {
      owner = "Yahweasel";
      repo = "libav.js";
      rev = "v${version}";
      hash = "sha256-9DPlsGIeoAVSgbMqXJQf+iBH5HMSOcEsbeaiFNFIb5s=";
    };

    # terser minifies the frontend.
    npmDeps = importNpmLock { npmRoot = finalAttrs.src; };

    nativeBuildInputs = [
      emscripten
      nodejs
      importNpmLock.npmConfigHook
      python3
      pkg-config
    ];

    postPatch = ''
      patchShebangs tools configs
      mkdir -p build
      ${lib.concatStrings (
        lib.mapAttrsToList (name: src: ''
          cp ${src} build/${name}
        '') sources
      )}
      (cd configs && node ./mkconfig.js ${variant} '${builtins.toJSON fragments}')
      # As a cross-compile, ffmpeg's configure defaults its host compiler to a
      # literal `gcc`, which the darwin stdenv doesn't provide.
      echo "--host-cc=${buildPackages.stdenv.cc}/bin/cc" >> configs/configs/${variant}/ffmpeg-config.txt
    '';

    # emcc compiles its system libraries into a cache on first use, and the
    # one in the store is read-only.
    preBuild = ''
      export EM_CACHE=$TMPDIR/emscripten-cache
      cp -r ${emscripten}/share/emscripten/cache $EM_CACHE
      chmod -R u+w $EM_CACHE
    '';

    makeFlags = [
      "dist/libav-${variant}.mjs"
      "dist/libav-${version}-${variant}.wasm.mjs"
    ];

    enableParallelBuilding = true;

    installPhase = ''
      runHook preInstall
      install -Dm444 -t $out \
        dist/libav-${variant}.mjs \
        dist/libav-${version}-${variant}.wasm.mjs \
        dist/libav-${version}-${variant}.wasm.wasm
      runHook postInstall
    '';
  });
in
stdenvNoCC.mkDerivation {
  pname = "moq-libav";
  version = (lib.importJSON ./package.json).version;

  dontUnpack = true;
  nativeBuildInputs = [ jq ];

  # The workspace manifest points into dist/, which is this output, so drop that
  # prefix and the dev-only fields, the way js/common/package.ts does for src/.
  installPhase = ''
    runHook preInstall
    mkdir -p $out
    cp ${wasm}/* $out/
    install -Dm444 ${./README.md} $out/README.md
    install -Dm444 ${./index.d.ts} $out/index.d.ts
    jq 'del(.scripts, .devDependencies, .jsr)
      | .exports |= walk(if type == "string" then sub("^\\./dist/"; "./") else . end)' \
      ${./package.json} > $out/package.json
    runHook postInstall
  '';

  passthru = { inherit wasm; };

  meta = {
    description = "libav.js decoding Opus and AAC for the @moq/hang WebCodecs polyfill";
    homepage = "https://github.com/moq-dev/moq/tree/main/js/libav";
    license = lib.licenses.lgpl21Plus;
  };
}
