# [XS] Backport the relay redial backoff reset to the next release

## Goal

A relay on the next `release` patch redials a cluster peer from the first
backoff ceiling once an attempt connected, so a flap after a long outage no
longer waits out 30 s.

## Plan

Port the `main` fix from the PR that finished `relay-config-loud` as a separate
PR onto `release`, per RELEASE.md, keeping the published API.
`rs/moq-relay/src/cluster.rs` `run_remote` on `release` has the same dead
`Ok(()) if elapsed >= stable_threshold` branch, because `run_remote_once` only
returns an error. Report whether the attempt connected (any observation with
`connects > 0`) and pass it to `Redial::next`.

The same PR on `main` also refuses `cluster.api.root` without `cluster.api.url`
and `cluster.token` with no peer to dial. Those are startup failures for
configs that ran before, so a patch release should not carry them.

## Waiting

- Maintainer: whether the backport is wanted at all, and whether it carries the
  two config refusals. Recommend backporting the backoff reset only: a flapping
  peer is a degraded mesh, while a patch that stops a working relay from
  booting is a break. Check by this quest's Plan naming the choice.
