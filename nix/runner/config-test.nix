{ pkgs }:
let
  evaluate =
    ids: nat:
    import (pkgs.path + "/nixos/lib/eval-config.nix") {
      system = pkgs.stdenv.hostPlatform.system;
      modules = [
        ../modules/github-runner-vm.nix
        {
          boot.isContainer = true;
          networking.useHostResolvConf = false;
          networking.nat = nat;
          system.stateVersion = "26.05";
          fileSystems."/" = {
            device = "none";
            fsType = "tmpfs";
          };
          services.github-runner-vms = pkgs.lib.mapAttrs (_: id: {
            enable = true;
            runner.url = "https://github.com/example/project";
            githubApp = {
              appId = 1;
              installationId = 2;
            };
            network = {
              inherit id;
              uplink = "eth0";
            };
          }) ids;
        }
      ];
    };
  ids = {
    first = 0;
    second = 1;
  };
  valid = evaluate ids { };
  guest = valid.config.services.github-runner-vms.first.guest.config;
  mixedNetworking = valid.extendModules {
    modules = [
      {
        networking.networkmanager.enable = true;
        systemd.network.networks.secondary = {
          matchConfig.Name = "eth1";
          linkConfig.RequiredForOnline = "routable";
        };
      }
    ];
  };
  matchingNat = evaluate ids { externalInterface = "eth0"; };
  smallDisk = valid.extendModules {
    modules = [ { services.github-runner-vms.first.resources.disk = 7; } ];
  };
  minimumDisk = valid.extendModules {
    modules = [ { services.github-runner-vms.first.resources.disk = 8; } ];
  };
  wrongUplink = evaluate ids { externalInterface = "eth1"; };
  fixedAddress = evaluate ids { externalIP = "203.0.113.1"; };
  duplicate = evaluate {
    first = 0;
    second = 0;
  } { };
in
assert pkgs.lib.assertMsg (pkgs.lib.all (item: item.assertion) valid.config.assertions) (
  pkgs.lib.concatMapStringsSep "\n" (item: item.message) (
    builtins.filter (item: !item.assertion) valid.config.assertions
  )
);
assert pkgs.lib.all (item: item.assertion) matchingNat.config.assertions;
assert pkgs.lib.any (item: !item.assertion) smallDisk.config.assertions;
assert pkgs.lib.all (item: item.assertion) minimumDisk.config.assertions;
assert pkgs.lib.any (
  item: !item.assertion
) smallDisk.config.services.github-runner-vms.first.guest.config.assertions;
assert pkgs.lib.all (
  item: item.assertion
) minimumDisk.config.services.github-runner-vms.first.guest.config.assertions;
assert !valid.config.networking.firewall.filterForward;
assert guest.systemd.services.github-runner-runner.serviceConfig.StateDirectory == [ ];
assert guest.systemd.services.github-runner-runner.serviceConfig.Restart == "no";
assert guest.systemd.services.github-runner-runner.unitConfig.SuccessAction == "poweroff";
assert guest.systemd.services.github-runner-runner.unitConfig.FailureAction == "poweroff";
assert
  guest.services.github-runners.runner.extraEnvironment.RUNNER_ROOT == "/run/github-runner/runner";
assert !(valid.config.systemd.services.first.serviceConfig ? LoadCredential);
assert
  valid.config.systemd.services.github-runner-jit-first.serviceConfig.LoadCredential == [
    "app-key:/var/lib/secrets/github-runner-first.pem"
  ];
assert pkgs.lib.hasInfix "credentials/runner-jit" valid.config.systemd.services.first.script;
assert !(pkgs.lib.hasInfix "app-key" valid.config.systemd.services.first.script);
assert mixedNetworking.config.systemd.network.wait-online.enable;
assert !valid.config.systemd.network.wait-online.enable;
assert pkgs.lib.all (
  name: valid.config.systemd.services.${name}.serviceConfig.Restart == "always"
) (builtins.attrNames ids);
assert pkgs.lib.all (
  name:
  pkgs.lib.hasInfix "virtio-balloon-pci,free-page-reporting=on"
    valid.config.systemd.services.${name}.script
) (builtins.attrNames ids);
assert pkgs.lib.any (item: !item.assertion) duplicate.config.assertions;
assert pkgs.lib.any (item: !item.assertion) wrongUplink.config.assertions;
assert pkgs.lib.any (item: !item.assertion) fixedAddress.config.assertions;
pkgs.runCommand "github-runner-vm-config-test" { } ''
  # ExecStop also runs after QEMU has already exited on guest poweroff.
  env -u MAINPID ${valid.config.systemd.services.first.serviceConfig.ExecStop}
  touch "$out"
''
