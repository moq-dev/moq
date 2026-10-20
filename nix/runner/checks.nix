{ pkgs }:
let
  guestModule = import ./guest.nix {
    runner = {
      url = "https://github.com/example/queue";
      name = "test-queue";
      labels = [ "queue" ];
    };
    network = {
      hostAddress = "10.231.0.1";
      guestAddress = "10.231.0.2";
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
  github-runner-vm-config = import ./config-test.nix { inherit pkgs; };
}
