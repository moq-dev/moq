# Persistent GitHub Actions runner VM

The `github-runner-vm` NixOS module runs each repository's runner in its own
persistent KVM guest. The guest keeps its Nix store, home caches, and runner state
across jobs and reboots. Restarting the runner clears its checkout and build directory. It has no host filesystem mounts or tailnet access.
The firewall permits public IPv4 egress and blocks the host and private networks.

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

The host service and disk are named by the instance attribute: the example uses `queue-ci.service`
and `/var/lib/queue-ci/root.qcow2`. Its disk is created once. Changing the configured
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

The runner's home is `/var/cache/github-runner-home`; mbx uses
`/var/cache/github-runner-home/.cache/mbx`. Workflows must select mbx's local backend
on this runner. There is no cache-warming service or cache server. Persistent job
state is shared with subsequent jobs, so route only trusted work to the runner.
For MoQ, set `MOQ_QUEUE_RUNNER` to the runner label after a representative CI run
passes; release pull requests use hosted runners and main pull requests skip cloud CI.

`just gh runner-vm` builds a guest image and runs NixOS integration tests for
startup without carrier, public egress for multiple guests, private-network and
sibling-guest isolation including spoofed established replies, compatible host NAT
settings, the runner's locked-memory budget, and Nix/mbx persistence across runner
and guest restarts.
