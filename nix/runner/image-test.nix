{ pkgs, guestModule }:

pkgs.testers.runNixOSTest {
  name = "github-runner-vm-image";
  nodes.machine = { lib, ... }: {
    imports = [ guestModule ];
    environment.systemPackages = [ pkgs.python3 ];
    system.switch.enable = true;
    services.github-runners.runner.enable = lib.mkForce false;
    systemd.services.runner-token.enable = false;
    virtualisation = {
      directBoot.enable = false;
      mountHostNixStore = false;
      useEFIBoot = true;
      useDefaultFilesystems = false;
      memorySize = 1024;
      fileSystems = lib.mkForce { };
    };
  };
  testScript = { nodes, ... }: ''
    import os
    from pathlib import Path
    import subprocess
    import tempfile

    # Grow the virtual disk past the image, as the host launcher does, so the
    # root filesystem must expand on first boot.
    directory = tempfile.TemporaryDirectory()
    disk = Path(directory.name) / "boot.qcow2"
    subprocess.run([
        "${pkgs.qemu_kvm}/bin/qemu-img", "create", "-f", "qcow2",
        "-b", "${nodes.machine.system.build.image}/${nodes.machine.image.fileName}",
        "-F", "raw", str(disk), "20G",
    ], check=True)
    os.environ["NIX_DISK_IMAGE"] = str(disk)

    machine.start()
    machine.wait_for_unit("multi-user.target")
    machine.succeed("test $(systemctl show --value -p Result runner-initialize.service) = success")
    with subtest("the actual image boots on a growable XFS root"):
        machine.succeed("test $(findmnt -n -o FSTYPE /) = xfs")
        machine.succeed("test $(df --output=size -B1 / | tail -1) -gt 8589934592")
        machine.succeed("test -L /nix/var/nix/profiles/system")
        machine.succeed("nix-store --query --references /run/current-system")
        machine.succeed("test ! -e /nix-path-registration")
        machine.succeed("grep -F 'init=' /boot/loader/entries/nixos-*.conf")
        machine.succeed("test ! -e /boot/EFI/Linux/${nodes.machine.system.boot.loader.ukiFile}")

    with subtest("the build and cache directories share copy-on-write storage"):
        machine.succeed("install -d /var/cache/github-runner-work")
        machine.copy_from_host_via_shell("${./storage.py}", "/tmp/storage.py")
        print(machine.succeed("${pkgs.python3}/bin/python3 /tmp/storage.py /var/cache/github-runner-work"))
        machine.succeed("echo registered > /var/lib/runner-registration-marker")

    with subtest("the installed NixOS generation boots again with its state"):
        machine.shutdown()
        machine.start()
        machine.wait_for_unit("multi-user.target")
        machine.succeed("test $(findmnt -n -o FSTYPE /) = xfs")
        machine.succeed("test $(cat /var/lib/runner-registration-marker) = registered")
  '';
}
