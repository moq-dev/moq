# Persistent GitHub Actions runner VM

The `github-runner-vm` NixOS module runs each repository's runner in its own
persistent KVM guest. The guest keeps its Nix store, home caches, and runner state
across jobs and reboots. Restarting the runner clears its checkout and build directory. It has no host filesystem mounts or tailnet access.
The firewall permits public IPv4 egress and blocks the host and private networks.
The guest boots with UEFI from an XFS root, so the compiler cache and build
outputs can share blocks with reflinks. The host stores it in a sparse raw image;
it needs neither QCOW2 metadata nor a second copy-on-write layer.

Import `moq.nixosModules.github-runner-vm` into an x86\_64 NixOS host:

```nix
services.github-runner-vms.queue-ci = {
  enable = true;
  runner = {
    url = "https://github.com/example/project";
    name = "build-host-queue";
    labels = [ "merge-queue" ];
  };
  network = { id = 0; uplink = "eth0"; };
  tokenFile = "/var/lib/secrets/queue-runner.token";
  resources = { cores = 8; memory = 16384; disk = 192; };
  guestConfig = {
    users.users.root.openssh.authorizedKeys.keyFiles = [ ./deploy.pub ];
    nix.settings = {
      extra-substituters = [ "https://nix.moq.dev" ];
      extra-trusted-public-keys = [
        "nix.moq.dev-1:BZ0DtCypDFv1ESX3hxuB42yc0vitSA1YHojSmVwB5bk="
      ];
    };
  };
};
```

Add another named instance for another repository, with its own `network.id`,
registration token, and guest configuration. Every instance gets a separate user,
disk, runtime credentials, and TAP interface; no cache data is shared between
instances. For example:

```nix
services.github-runner-vms.other-ci = {
  enable = true;
  runner.url = "https://github.com/example/another-project";
  network = { id = 1; uplink = "eth0"; };
  guestConfig.users.users.root.openssh.authorizedKeys.keyFiles = [ ./deploy.pub ];
};
```

The default token path is `/var/lib/secrets/github-runner-NAME.token`.
Renaming an instance selects a new disk; it does not migrate its existing state.

Enable virtualization in firmware and load the CPU's KVM module before activation.
`/dev/kvm` must exist. Each instance needs a unique `network.id` from 0 to 255.
It uses `10.231.ID.0/30`: the host is `.1` and the guest is `.2`. Reserve these
subnets on the host. Duplicate IDs are rejected during evaluation. NetworkManager owns the host uplink when
enabled; otherwise configure that uplink in networkd. The TAP receives its address
before QEMU attaches, so guest startup does not depend on TAP carrier.
The module uses the host's NixOS NAT service. Leave `networking.nat.externalInterface`
unset, or set it to the same uplink as every runner. Fixed `networking.nat.externalIP`
is unsupported. Conflicting settings are rejected during evaluation.

For a public repository, require approval for **all external contributors' workflows**
before registering a persistent runner. A contributor can change `runs-on` in a
pull request, so a workflow condition is not an assignment boundary. Review the
workflow and its code before approving it: approval authorizes execution against
the persistent guest and its caches. Restricting approval only to first-time
contributors does not provide this boundary. See [GitHub's approval policy](https://docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/enabling-features-for-your-repository/managing-github-actions-settings-for-a-repository).

Mint a repository registration token with `gh api -X POST
repos/OWNER/REPO/actions/runners/registration-token --jq .token`, and install it
at `tokenFile` with mode `0600` in a root-only directory. Runtime credentials reach
the guest through QEMU firmware configuration; secret contents never enter the
Nix store. Keep the expired token file after registration. The guest retains its
own long-lived credential; wiping its state requires a fresh registration token.
Restart the host VM service after replacing the token.

Set `runner.count` to run several jobs at once in one guest. The runners share
the home, so concurrent jobs share mbx's store and compiler budget, and a
compilation one job is running is restored by the other instead of repeated.
Each runner has its own work directory. The first keeps `runner.name`; the others
append `-2`, `-3`, and so on. Raising the count registers new runners, so install
a fresh registration token and restart the host VM service; the existing runner
re-registers under its own name. Size `resources` for the concurrent jobs.

The host service and disk are named by the instance attribute: the example uses `queue-ci.service`
and `/var/lib/queue-ci/root.raw`. Its disk is created once. Changing the configured
image does not replace existing state, and changing `resources.disk` does not
resize an existing disk. For guest updates, expose the evaluated guest in the
host flake's outputs (`{ self, ... }:`):

```nix
nixosConfigurations.queue-ci =
  self.nixosConfigurations.build-host.config.services.github-runner-vms.queue-ci.guest;
```

Then deploy with `nixos-rebuild switch --flake .#queue-ci --target-host
root@10.231.0.2`, using the dedicated deployment key. Host rebuilds update the VM
launcher. Back up the deployment private key separately.

## Migrating an existing ext4 guest

An existing `root.qcow2` must be migrated before activating the new host
configuration. The launcher refuses to replace it with an empty guest.
Build the migration tool and the host configuration first, while the old guest
continues running:

```bash
nix build .#nixosConfigurations.queue-ci.config.system.build.runner-migrate -o runner-migrate
nixos-rebuild build --flake .#build-host
```

Once the runner is idle, stop its VM and copy its filesystem into the new image:

```bash
sudo systemctl stop queue-ci
sudo ./runner-migrate/bin/runner-migrate /var/lib/queue-ci/root.qcow2 /var/lib/queue-ci/root.raw
```

The tool keeps the source read-only, preserves registration, caches, ownership,
permissions and hard links, and grows the new XFS filesystem to `resources.disk`.
It refuses an open source disk or an existing destination. Guest filesystems stay
inside libguestfs appliances rather than being mounted in the host kernel.

Activate the built host configuration and start the VM:

```bash
nixos-rebuild switch --sudo --flake .#build-host
sudo systemctl start queue-ci
```

Verify the runner is online, its root filesystem is XFS, and a representative
job passes before removing the old disk. To roll back, stop the VM, reactivate
the previous host configuration, and start it with its retained `root.qcow2`.

The runner's home is `/var/cache/github-runner-home`; mbx uses
`/var/cache/github-runner-home/.cache/mbx`. Workflows must select mbx's local backend
on this runner. There is no cache-warming service or cache server. Persistent job
state is shared with subsequent jobs, so route only trusted work to the runner.
The guest hands freed memory back to the host and drops its page cache while no
job runs, so an idle VM holds little more than its running services; the next job
reads its caches from disk.
The guest has no swap, so earlyoom kills the largest process once available memory
drops below 10%, rather than letting a job that fills memory thrash the disk.
For MoQ, set `MOQ_QUEUE_RUNNER` to the runner label after a representative CI run
passes; release pull requests use hosted runners and main pull requests skip cloud CI.

The hosted workflow uses `just gh runner-vm-ci` to load a temporary AppArmor
profile for the exact pinned `unshare` while building the image. The recipe
refuses local and self-hosted runners, and unloads the profile on exit,
including build failure. `just gh runner-vm` does not change host policy;
its build host must already support rootless user namespaces.

`just gh runner-vm` builds a guest image and runs NixOS integration tests for
startup without carrier, public egress for multiple guests, private-network and
sibling-guest isolation including spoofed established replies, compatible host NAT
settings, the runner's locked-memory budget, symlink-free home and work directories
with state migrated from the former DynamicUser layout, a second runner sharing the
home but not the work directory, free page reporting and
idle cache drops, killing a job that fills memory before it thrashes the disk, and
Nix/mbx persistence across runner and guest restarts.
It also migrates a disposable ext4 disk, boots the resulting XFS image through
UEFI, checks filesystem growth and a second boot, and measures ordinary copies
against reflinks at 1, 16, and 64 MiB. Timings are reported without a flaky
performance threshold; reflink support and copy-on-write isolation are required.
