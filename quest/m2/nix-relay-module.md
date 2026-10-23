# [XS] The relay's NixOS module is tested and takes several cluster peers

## Goal

A change to `nix/modules/moq-relay.nix` runs moq-relay's tests, and
`cluster.rootUrl` accepts several peers.

## Plan

moq-relay's `the_nixos_module_sets_only_known_variables` test reads the
module, but a module-only change selects only the flake scope. Map
`nix/modules/moq-relay.nix` to moq-relay in `sh/dispatch.sh` and
`sh/rs/select.sh`.

`cluster.rootUrl` is a single string mapped to `MOQ_CLUSTER_CONNECT`, which
accepts a comma-separated list (`rs/moq-relay/src/cluster.rs`). Make it a
non-empty list joined with commas. That breaks the module option; say so in the PR.

Public API: NixOS module option type changes. Wire: none.
