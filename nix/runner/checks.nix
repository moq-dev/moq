{ pkgs }:
let
  guestModule = import ./guest.nix {
    runner = {
      url = "https://github.com/example/queue";
      name = "test-queue";
      labels = [ "queue" ];
    };
    resources = {
      cores = 2;
      memory = 1024;
      disk = 16;
    };
  };
  guest = import (pkgs.path + "/nixos/lib/eval-config.nix") {
    system = pkgs.stdenv.hostPlatform.system;
    modules = [ guestModule ];
  };
in
{
  github-runner-vm = import ./test.nix { inherit pkgs guestModule; };
  github-runner-vm-image = guest.config.system.build.image;
}
