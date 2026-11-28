{ nonce }:
let
  # The capability probe needs only the pinned nixpkgs, independent of build output.
  lock = builtins.fromJSON (builtins.readFile ../../flake.lock);
  # The root's nixpkgs follows quest's, so it is quest's node's input.
  nixpkgs =
    builtins.fetchTree
      lock.nodes.${lock.nodes.${lock.nodes.root.inputs.quest}.inputs.nixpkgs}.locked;
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
