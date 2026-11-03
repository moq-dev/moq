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

    directory = tempfile.TemporaryDirectory()
    os.environ["LIBGUESTFS_BACKEND"] = "direct"
    legacy = Path(directory.name) / "legacy.raw"
    source = Path(directory.name) / "legacy.qcow2"
    migrated = Path(directory.name) / "root.raw"
    guestfish = "${pkgs.libguestfs-with-appliance}/bin/guestfish"
    migrate = "${nodes.machine.system.build.runner-migrate}/bin/runner-migrate"
    subprocess.run([guestfish, "-N", f"{legacy}=fs:ext4:512M"], input="""
    mount /dev/sda1 /
    mkdir-p /var/cache/github-runner-home/.cache/mbx
    write /var/cache/github-runner-home/.cache/mbx/fixture cached
    ln /var/cache/github-runner-home/.cache/mbx/fixture /var/cache/github-runner-home/.cache/mbx/linked
    mkdir-p /var/lib/github-runner/runner
    write /var/lib/github-runner/runner/.runner registered
    chmod 0600 /var/lib/github-runner/runner/.runner
    write /var/lib/runner-owner-marker owned
    chown 12345 12346 /var/lib/runner-owner-marker
    write /nix-path-registration stale
    """, text=True, check=True)
    subprocess.run([
        "${pkgs.qemu_kvm}/bin/qemu-img", "convert", "-f", "raw", "-O", "qcow2",
        str(legacy), str(source),
    ], check=True)
    with subtest("migration refuses an active source disk"):
        with source.open("rb"):
            assert subprocess.run([migrate, source, migrated]).returncode != 0
        assert not migrated.exists()
    subprocess.run([migrate, source, migrated], check=True)
    with subtest("migration preserves its source and refuses to overwrite a destination"):
        subprocess.run([
            "${pkgs.qemu_kvm}/bin/qemu-img", "compare", "-f", "raw", "-F", "qcow2",
            str(legacy), str(source),
        ], check=True)
        assert subprocess.run([migrate, source, migrated]).returncode != 0

    disk = Path(directory.name) / "boot.qcow2"
    subprocess.run([
        "${pkgs.qemu_kvm}/bin/qemu-img", "create", "-f", "qcow2",
        "-b", str(migrated), "-F", "raw", str(disk), "20G",
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
        machine.succeed("test $(cat /var/lib/github-runner/runner/.runner) = registered")
        machine.succeed("test $(stat -c %a /var/lib/github-runner/runner/.runner) = 600")
        machine.succeed("test $(stat -c %u:%g /var/lib/runner-owner-marker) = 12345:12346")
        machine.succeed("test $(cat /var/cache/github-runner-home/.cache/mbx/fixture) = cached")
        machine.succeed("test /var/cache/github-runner-home/.cache/mbx/fixture -ef /var/cache/github-runner-home/.cache/mbx/linked")

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
