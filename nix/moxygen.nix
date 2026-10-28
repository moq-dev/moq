{
  lib,
  stdenv,
  fetchFromGitHub,
  cmake,
  ninja,
  pkg-config,
  gperf,
  python3,
  perl,
  openssl,
  glog,
  gflags,
  fmt,
  double-conversion,
  boost,
  libevent,
  libsodium,
  zstd,
  zlib,
  c-ares,
  libunwind,
}:
let
  pins = import ./moxygen-sources.nix;
  sources = lib.mapAttrs (_: pin: fetchFromGitHub pin) pins;
in
stdenv.mkDerivation {
  pname = "moxygen-conformance";
  version = "unstable-2026-10-23";
  src = sources.moxygen;
  cmakeDir = "../standalone";
  patches = [
    # The relay conformance profile: the client accepts track-level priority and
    # an END_OF_TRACK directly after the last object. Every other check stays.
    ./moxygen-profile.patch
    # The test origin answers TRACK_STATUS, which a relay cluster's edge sends
    # before subscribing, without the GROUP_ORDER parameter draft-18 forbids in
    # TRACK_STATUS_OK. Drop at the next pin bump once
    # https://github.com/facebookexperimental/moxygen/pull/264 lands.
    ./moxygen-track-status.patch
  ];
  nativeBuildInputs = [
    cmake
    ninja
    pkg-config
    gperf
    python3
    perl
  ];
  buildInputs = [
    openssl
    glog
    gflags
    fmt
    double-conversion
    boost
    libevent
    libsodium
    zstd
    zlib
    c-ares
    libunwind
  ];
  # The standalone client uses blockingWait, which needs Folly's concrete executor.
  postPatch =
    lib.concatStringsSep "\n" (
      lib.mapAttrsToList (name: source: ''
        cp -R ${source} .vendor-${name}
        chmod -R u+w .vendor-${name}
        patchShebangs .vendor-${name}
      '') (builtins.removeAttrs sources [ "moxygen" ])
    )
    + ''
      substituteInPlace moxygen/moqtest/CMakeLists.txt \
        --replace-fail 'moqtest_client PUBLIC' \
          'moqtest_client PUBLIC Folly::folly_executors_manual_executor'
    '';
  preConfigure = lib.concatStringsSep "\n" (
    lib.mapAttrsToList (name: _: ''
      cmakeFlagsArray+=("-DFETCHCONTENT_SOURCE_DIR_${lib.toUpper name}=$PWD/.vendor-${name}")
    '') (builtins.removeAttrs sources [ "moxygen" ])
  );
  cmakeFlags = [
    "-DBUILD_TESTS=OFF"
    "-DBUILD_MOQTEST=ON"
    "-DBUILD_SAMPLES=OFF"
    "-DFETCHCONTENT_FULLY_DISCONNECTED=ON"
  ];
  ninjaFlags = [
    "moqtest_client"
    "moqtest_server"
  ];
  installPhase = ''
    runHook preInstall
    mkdir -p $out/bin $out/libexec/moxygen/moqtest $out/share/moxygen
    for executable in moqtest_client moqtest_server; do
      install -m755 moxygen/moqtest/$executable $out/libexec/moxygen/moqtest/$executable
      ln -s $out/libexec/moxygen/moqtest/$executable $out/bin/$executable
    done
    install -m755 $src/moxygen/moqtest/conformance_test.sh $out/share/moxygen/conformance_test.sh
    cp ${./moxygen-sources.nix} $out/share/moxygen/sources.nix
    runHook postInstall
  '';
  meta = {
    description = "Pinned Moxygen client and origin for relay conformance";
    homepage = "https://github.com/facebookexperimental/moxygen";
    license = lib.licenses.asl20;
    platforms = lib.platforms.linux;
  };
}
