# [S] The relay NixOS module's peer list is tested

## Goal

`nix flake check` evaluates the moq-relay NixOS module with no, one, and
several `cluster.rootUrl` peers and refuses an empty list, asserting the
`MOQ_CLUSTER_CONNECT` value each produces. Optionally a NixOS VM test runs a
root and two leaves from the module and sees them connect.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7343](https://github.com/moq-dev/moq/pull/7343): m2, as
recommended.

[moq-dev/moq#7343](https://github.com/moq-dev/moq/pull/7343) made `services.moq-relay.cluster.rootUrl` a
`nullOr (nonEmptyListOf str)` (`nix/modules/moq-relay.nix:153`), joined into
`MOQ_CLUSTER_CONNECT` at `:380`, and verified it only by a manual `nix eval`.

- Add an eval check beside `nix/runner/config-test.nix`, wired into
  `flake.nix`'s `checks` (`:714`): `null` sets no `MOQ_CLUSTER_CONNECT`, one
  and two URLs join with commas, `[ ]` fails the type check, and a leaf
  without peers trips the assertion at `:219`.
- A VM test (root plus two leaves) only if it stays cheap in CI; otherwise
  leave it out and say so.

The string-to-list break needs a `MIGRATE.md` line; the
[migration audit](/quest/m2/release/migrate-audit.md) handles it, not this
quest.

Public API: none. Wire: none.
