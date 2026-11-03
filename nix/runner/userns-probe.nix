{ nonce }:
let
  # Other lanes rewrite ignored build output while this probe evaluates.
  flake = builtins.getFlake "git+file://${toString ../..}";
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
