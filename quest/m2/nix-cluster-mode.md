# [XS] The NixOS relay module drops its dead cluster.mode

## Goal

`services.moq-relay` has no `cluster.mode` option, and its missing-credential
warning fires for any relay that dials a cluster peer (`cluster.rootUrl` set)
without a certificate or token.

## Plan

Found 2026-11-14 by the [feature review](/quest/m3/release/feature-review.md).
Since [moq-dev/moq#8301](https://github.com/moq-dev/moq/pull/8301),
`cluster.mode` in `nix/modules/moq-relay.nix` is read only by an assertion and
a warning; dialing follows `rootUrl` alone. A root that dials other roots with
no `certFile` or `tokenFile` dials anonymously, is refused with
`unauthorized`, and retries forever with no warning, and `mode = "none"` with
`rootUrl` set still dials. Remove the option, key the warning on `rootUrl`,
drop the leaf assertion, update the plan in
[relay peers test](/quest/m5/nix-relay-peers-test.md), which checks that
assertion, and note it in `rs/moq-relay/MIGRATE.md`, since `release` ships the
option.

Public API: removes the NixOS option `services.moq-relay.cluster.mode`.
Wire: none.
