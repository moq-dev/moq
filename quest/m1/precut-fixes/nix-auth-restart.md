# [XS] Upgrading the NixOS moq-auth unit restarts the relay through its handoff

## Goal

With `services.moq-relay.handoff` on, a `nixos-rebuild switch` that changes
both units restarts the relay through `SIGUSR2` and keeps its sessions,
instead of stopping it cold because its required `moq-auth` unit stopped.

## Plan

Found by the 2026-11-17 pre-cut correctness audit; confirmed from source and
systemd's semantics. In `nix/modules/moq-relay.nix`, `moq-auth` is
`requiredBy` the relay and keeps the default `stopIfChanged = true`. Its
`ExecStart` names the `moq-cli` store path, so nearly every upgrade changes
it: switching stops `moq-auth`, `Requires=` propagates the stop to the relay,
and the relay drains every session and starts with no handoff.

Recommended: `moq-auth` sets `stopIfChanged = false`, so it restarts in place
and nothing propagates (rejected: `wants` instead of `requiredBy`, which lets
the relay run with no auth server and refuse every session). Check it with
the module's NixOS VM test if one covers handoff, else by a switch on a
test host.

Public API: none. Wire: none.

Caused by, for the fix PR's `## Caused by`:

- [moq-dev/moq#8596](https://github.com/moq-dev/moq/pull/8596) - made moq-auth a required unit of the relay
