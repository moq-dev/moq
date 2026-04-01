# Pre-built moq-relay binary package.
# Fetches a release binary from GitHub instead of compiling from source.
#
# By default, reads version and hashes from ./hashes.json (updated by CI).
# Override version/hashes to pin a specific release.
{
  lib,
  stdenvNoCC,
  fetchurl,
  autoPatchelfHook,
  version ? null,
  hashes ? null,
}:
let
  data = lib.importJSON ./hashes.json;
  info = data."moq-relay";
  effectiveVersion = if version != null then version else info.version;
  effectiveHashes = if hashes != null then hashes else info.hashes;

  targets = {
    x86_64-linux = "x86_64-unknown-linux-gnu";
    aarch64-linux = "aarch64-unknown-linux-gnu";
    x86_64-darwin = "x86_64-apple-darwin";
    aarch64-darwin = "aarch64-apple-darwin";
  };

  system = stdenvNoCC.hostPlatform.system;
  target = targets.${system} or (throw "Unsupported system: ${system}");
  hash = effectiveHashes.${system} or (throw "No hash for system: ${system}");
in
assert effectiveVersion != "" || throw "No moq-relay version in hashes.json. Run the release workflow first or pass version/hashes manually.";
stdenvNoCC.mkDerivation {
  pname = "moq-relay";
  version = effectiveVersion;

  src = fetchurl {
    url = "https://github.com/moq-dev/moq/releases/download/moq-relay-v${effectiveVersion}/moq-relay-v${effectiveVersion}-${target}";
    inherit hash;
  };

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
