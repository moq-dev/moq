# Persistent GitHub Actions runner VM

The `github-runner-vm` NixOS module runs a repository's runner in a persistent
KVM guest. The guest keeps its Nix store, mbx cache, checkout, and runner state
across jobs and reboots. It has no host filesystem mounts or tailnet access.
The firewall permits public IPv4 egress and blocks the host and private networks.

Import `moq.nixosModules.github-runner-vm` into an x86\_64 NixOS host:

```nix
services.github-runner-vm = {
  enable = true;
  name = "queue-ci";
  runner = {
    url = "https://github.com/example/project";
    name = "build-host-queue";
    labels = [ "merge-queue" ];
  };
  network.uplink = "eth0";
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

Enable virtualization in firmware and load the CPU's KVM module before activation.
`/dev/kvm` must exist. Reserve `10.231.0.0/30` for the private VM link. The host is
`10.231.0.1`; the guest is `10.231.0.2`. NetworkManager owns the host uplink when
enabled; otherwise configure that uplink in networkd. The TAP receives its address
before QEMU attaches, so guest startup does not depend on TAP carrier.

Mint a repository registration token with `gh api -X POST
repos/OWNER/REPO/actions/runners/registration-token --jq .token`, and install it
at `tokenFile` with mode `0600` in a root-only directory. Runtime credentials reach
the guest through QEMU firmware configuration; secret contents never enter the
Nix store. Keep the expired token file after registration. The guest retains its
own long-lived credential; wiping its state requires a fresh registration token.
Restart the host VM service after replacing the token.

The host service and disk are named by `name`: the example uses `queue-ci.service`
and `/var/lib/queue-ci/root.qcow2`. Its disk is created once. Changing the configured
image does not replace existing state, and changing `resources.disk` does not
resize an existing disk. For guest updates, expose the evaluated guest through
the host flake:

```nix
nixosConfigurations.queue-ci =
  nixosConfigurations.build-host.config.services.github-runner-vm.guest;
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
startup without carrier, public egress, private-network isolation, and Nix/mbx
persistence across runner and guest restarts.
