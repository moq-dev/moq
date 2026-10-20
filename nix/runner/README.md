# Persistent GitHub Actions runner VM

The `github-runner-vm` NixOS module runs each repository's runner in its own
persistent KVM guest. The guest keeps its Nix store, mbx cache, checkout, and runner state
across jobs and reboots. It has no host filesystem mounts or tailnet access.
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
      extra-substituters = [ "https://kixelated.cachix.org" ];
      extra-trusted-public-keys = [
        "kixelated.cachix.org-1:CmFcV0lyM6KuVM2m9mih0q4SrAa0XyCsiM7GHrz3KKk="
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
passes; pull requests stay on hosted runners.

`just gh runner-vm` builds a guest image and runs NixOS integration tests for
startup without carrier, public egress for multiple guests, private-network and
sibling-guest isolation, the runner's locked-memory budget, and Nix/mbx persistence across runner and guest restarts.
