{ nonce }:
let
  # The capability probe needs only the pinned nixpkgs, independent of build output.
  lock = builtins.fromJSON (builtins.readFile ../../flake.lock);
  nixpkgs = builtins.fetchTree lock.nodes.${lock.nodes.root.inputs.nixpkgs}.locked;
  pkgs = import nixpkgs.outPath { system = builtins.currentSystem; };
in
pkgs.runCommand "moq-userns-probe-${nonce}"
  {
    nativeBuildInputs = [ pkgs.util-linux ];
    allowSubstitutes = false;
  }
  ''
    if ! unshare --map-root-user true; then
      echo "MOQ_USERNS_UNAVAILABLE" >&2
      exit 77
    fi
    touch "$out"
  ''
