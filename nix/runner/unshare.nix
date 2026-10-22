{ pkgs }:

pkgs.writeText "runner-userns.apparmor" ''
  # Repart uses the pinned Nix-store unshare, which Ubuntu's profiles omit.
  abi <abi/4.0>,
  include <tunables/global>

  profile nix-repart-unshare ${pkgs.lib.getExe' pkgs.util-linux "unshare"} flags=(unconfined) {
    userns,
  }
''
