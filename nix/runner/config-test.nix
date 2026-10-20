{ pkgs }:
let
  evaluate =
    ids:
    import (pkgs.path + "/nixos/lib/eval-config.nix") {
      system = pkgs.stdenv.hostPlatform.system;
      modules = [
        ../modules/github-runner-vm.nix
        {
          boot.isContainer = true;
          networking.useHostResolvConf = false;
          system.stateVersion = "26.05";
          fileSystems."/" = {
            device = "none";
            fsType = "tmpfs";
          };
          services.github-runner-vms = pkgs.lib.mapAttrs (_: id: {
            enable = true;
            runner.url = "https://github.com/example/project";
            network = {
              inherit id;
              uplink = "eth0";
            };
          }) ids;
        }
      ];
    };
  valid = evaluate {
    first = 0;
    second = 1;
  };
  duplicate = evaluate {
    first = 0;
    second = 0;
  };
in
assert pkgs.lib.assertMsg (pkgs.lib.all (item: item.assertion) valid.config.assertions) (
  pkgs.lib.concatMapStringsSep "\n" (item: item.message) (
    builtins.filter (item: !item.assertion) valid.config.assertions
  )
);
assert pkgs.lib.any (item: !item.assertion) duplicate.config.assertions;
pkgs.runCommand "github-runner-vm-config-test" { } ''
  touch "$out"
''
