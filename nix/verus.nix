# Verus, the verifier `just rs verify` runs over rs/kio/src/log.rs's proof.
#
# Verus is a rustc driver linked against one exact toolchain's librustc_driver, so this
# wraps its release build with that toolchain instead of the rustup install its launcher
# expects. Bump `version` and `toolchain` together: the release's rust-toolchain.toml
# names the toolchain, and the vstd version in rs/kio/verify/Cargo.toml must match
# the release's date.
{
  pkgs,
  rust-bin,
}:
let
  version = "0.2026.11.22.1ce6a59";
  toolchain = rust-bin.stable."1.100.0".minimal.override {
    extensions = [ "rustc-dev" ];
  };
  assets = {
    x86_64-linux = {
      name = "x86-linux";
      hash = "sha256-y3E6cSjVg2Yw34KjQWZ1lEPgIHDErPEthzgPerGRzwg=";
    };
    aarch64-linux = {
      name = "arm64-linux";
      hash = "sha256-//P7zs/N52q3KgidcdvGXo7rW71VXNufbL/4B2lOGTs=";
    };
  };
  asset =
    assets.${pkgs.stdenv.hostPlatform.system}
      or (throw "verus.nix packages Verus for Linux only; macOS needs its dylibs relinked");
in
pkgs.stdenv.mkDerivation {
  pname = "verus";
  inherit version;
  src = pkgs.fetchzip {
    url = "https://github.com/verus-lang/verus/releases/download/release%2F${version}/verus-${version}-${asset.name}.zip";
    inherit (asset) hash;
  };
  nativeBuildInputs = [
    pkgs.autoPatchelfHook
    pkgs.makeWrapper
  ];
  buildInputs = [
    toolchain
    pkgs.stdenv.cc.cc.lib
    pkgs.zlib
  ];
  installPhase = ''
    runHook preInstall
    mkdir -p $out/libexec/verus $out/bin
    cp -r . $out/libexec/verus
    for bin in verus cargo-verus; do
      makeWrapper $out/libexec/verus/$bin $out/bin/$bin \
        --set VERUS_USE_RUSTUP 0 \
        --prefix PATH : ${toolchain}/bin
    done
    runHook postInstall
  '';
  passthru = { inherit toolchain; };
}
