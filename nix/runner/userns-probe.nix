{ nonce }:
let
  flake = builtins.getFlake (toString ../..);
  pkgs = import flake.inputs.nixpkgs { system = builtins.currentSystem; };
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
