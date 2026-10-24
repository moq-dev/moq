# Persistent GitHub Actions runner VM

The `github-runner-vm` NixOS module runs each repository's runner in its own
persistent KVM guest. The guest keeps its Nix store and home caches across jobs,
but each boot runs one ephemeral, just-in-time (JIT) runner and powers off when
that listener exits. The host then mints a fresh configuration and boots it again.
Runner registration lives only in guest runtime storage. The guest has no host
filesystem mounts or tailnet access.
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
  githubApp = {
    appId = 123;
    installationId = 456;
    privateKeyFile = "/var/lib/secrets/queue-runner.pem";
  };
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
App configuration, and guest configuration. Every instance gets a separate user,
disk, runtime credentials, and TAP interface; no cache data is shared between
instances. For example:

```nix
services.github-runner-vms.other-ci = {
  enable = true;
  runner.url = "https://github.com/example/another-project";
  githubApp = { appId = 123; installationId = 456; };
  network = { id = 1; uplink = "eth0"; };
  guestConfig.users.users.root.openssh.authorizedKeys.keyFiles = [ ./deploy.pub ];
};
```

The default App key path is `/var/lib/secrets/github-runner-NAME.pem`.
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

Install a GitHub App on the repository with **Administration: write** permission.
Store its private key at `githubApp.privateKeyFile` with mode `0600` in a root-only
host directory. `appId` and `installationId` are public IDs, not secrets. Each
host VM start invokes a separate host-only minting unit: it signs a short-lived
App JWT, obtains an installation token restricted to this repository, and calls
[`generate-jitconfig`](https://docs.github.com/en/rest/actions/self-hosted-runners#create-configuration-for-a-just-in-time-runner-for-a-repository).
It refuses a non-ephemeral configuration. The App key and installation token never
enter QEMU; only the encoded JIT config crosses the firmware interface. Secret
contents never enter the Nix store. Runner names gain a unique suffix, so a failed
boot cannot replace an existing registration. Select runners by their labels.
`runner.groupId` defaults to 1.

The runner's registration files and loaded JIT credential live in `/run` and are
removed when its service ends. A root-owned latch prevents a second listener
start during the same boot, including after startup failure. Every listener exit
powers off the guest, and the next host VM start mints a new config. GitHub's JIT
runners accept at most one job and are then removed, so copying that config does
not authorize later jobs. Server-side replay refusal still needs an authorized
live registration test; local fixtures do not substitute for GitHub's server.

Before migrating a persistent registration, remove the old runner in GitHub so
previously copied credentials cannot keep it alive. Do this only as an approved
runner replacement. The new guest removes legacy registration files and the old
registration-token file from its persistent disk, while retaining its separate
cache directories. Host activation and App/key provisioning are operational steps,
not part of building or testing this module.

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
sudo ./runner-migrate/bin/runner-migrate /var/lib/queue-ci/root.raw /var/lib/queue-ci/root.raw
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

`just gh runner-vm` checks host JWT/JIT minting with an in-memory fake API, builds
a guest image, and runs NixOS integration tests for runtime-only registration,
removal of legacy credentials, refusing a second listener in one boot, and
startup without carrier, public egress for multiple guests, private-network and
sibling-guest isolation including spoofed established replies, compatible host NAT
settings, the runner's locked-memory budget, symlink-free home and work directories
with caches migrated from the former DynamicUser layout, free page reporting and
idle cache drops, killing a job that fills memory before it thrashes the disk, and
Nix/mbx persistence across runner and guest restarts.
It also migrates a disposable ext4 disk, boots the resulting XFS image through
UEFI, checks filesystem growth and a second boot, and measures ordinary copies
against reflinks at 1, 16, and 64 MiB. Timings are reported without a flaky
performance threshold; reflink support and copy-on-write isolation are required.
