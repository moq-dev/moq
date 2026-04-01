# Pre-built moq-relay binary package.
# This fetches a release binary from GitHub instead of compiling from source.
#
# Usage in a flake:
#   packages.moq-relay-bin = pkgs.callPackage ./nix/moq-relay-bin.nix {
#     version = "0.10.13";
#     hashes = {
#       x86_64-linux = "sha256-AAAA...";
#       aarch64-linux = "sha256-BBBB...";
#       x86_64-darwin = "sha256-CCCC...";
#       aarch64-darwin = "sha256-DDDD...";
#     };
#   };
{
  lib,
  stdenvNoCC,
  fetchurl,
  autoPatchelfHook,
  version,
  hashes,
}:
let
  targets = {
    x86_64-linux = "x86_64-unknown-linux-gnu";
    aarch64-linux = "aarch64-unknown-linux-gnu";
    x86_64-darwin = "x86_64-apple-darwin";
    aarch64-darwin = "aarch64-apple-darwin";
  };

  target = targets.${stdenvNoCC.hostPlatform.system} or (throw "Unsupported system: ${stdenvNoCC.hostPlatform.system}");
  hash = hashes.${stdenvNoCC.hostPlatform.system} or (throw "No hash for system: ${stdenvNoCC.hostPlatform.system}");
in
stdenvNoCC.mkDerivation {
  pname = "moq-relay";
  inherit version;

  src = fetchurl {
    url = "https://github.com/moq-dev/moq/releases/download/moq-relay-v${version}/moq-relay-v${version}-${target}";
    inherit hash;
  };

  # Patch ELF binaries on Linux to work with Nix's linker/libraries
  nativeBuildInputs = lib.optionals stdenvNoCC.isLinux [ autoPatchelfHook ];

  dontUnpack = true;

  installPhase = ''
    install -Dm755 $src $out/bin/moq-relay
  '';

  meta = {
    description = "Media over QUIC relay server (pre-built binary)";
    homepage = "https://github.com/moq-dev/moq";
    license = with lib.licenses; [
      mit
      asl20
    ];
    mainProgram = "moq-relay";
    platforms = builtins.attrNames targets;
  };
}
