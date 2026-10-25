{
  config,
  lib,
  pkgs,
  modulesPath,
  ...
}:

{
  imports = [ (modulesPath + "/image/repart.nix") ];

  image.repart = {
    enable = true;
    name = "github-runner";
    imageSize = "8G";
    partitions = {
      "10-esp" = {
        contents = {
          "/EFI/BOOT/BOOTX64.EFI".source =
            "${config.systemd.package}/lib/systemd/boot/efi/systemd-bootx64.efi";
          "/EFI/Linux/${config.system.boot.loader.ukiFile}".source =
            "${config.system.build.uki}/${config.system.boot.loader.ukiFile}";
        };
        repartConfig = {
          Type = "esp";
          Format = "vfat";
          Label = "ESP";
          SizeMinBytes = "512M";
          SizeMaxBytes = "512M";
        };
      };
      "20-root" = {
        storePaths = [ config.system.build.toplevel ];
        contents = {
          "/nix-path-registration".source = "${
            pkgs.closureInfo {
              rootPaths = [ config.system.build.toplevel ];
            }
          }/registration";
          "/etc/NIXOS".source = pkgs.emptyFile;
        };
        repartConfig = {
          Type = "root";
          Format = "xfs";
          Label = "nixos";
        };
      };
    };
  };

  # Repart copies the closure without a store database or system profile. Seed
  # both once so this remains an ordinary rebuildable NixOS guest.
  systemd.services.runner-initialize = {
    wantedBy = [ "multi-user.target" ];
    before = [
      "nix-daemon.service"
    ]
    ++ map (runner: "github-runner-${runner}.service") (lib.attrNames config.services.github-runners);
    unitConfig.ConditionPathExists = "/nix-path-registration";
    serviceConfig.Type = "oneshot";
    environment.NIX_REMOTE = "local";
    path = [ config.nix.package ];
    script = ''
      nix-store --load-db < /nix-path-registration
      nix-env --profile /nix/var/nix/profiles/system --set /run/current-system
      /run/current-system/bin/switch-to-configuration boot
      rm -f /boot/EFI/Linux/${config.system.boot.loader.ukiFile}
      rm /nix-path-registration
    '';
  };
}
