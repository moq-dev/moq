# [XS] Backport the relay redial backoff reset to the next release

## Goal

A relay on the next `release` patch redials a cluster peer from the first
backoff ceiling once an attempt connected, so a flap after a long outage no
longer waits out 30 s.

## Plan

Decided by the maintainer when the PR that finished `relay-config-loud` was
picked: ✅ backport only the redial backoff reset to `release`, and leave the
two startup refusals on `main` (rejected: backporting all of it, which would
let a patch stop a working relay from booting on a config that ran before; no
backport, which leaves a flapping peer waiting out 30 s on `release`).

Port the backoff reset from that PR as a separate PR onto `release`, per
RELEASE.md, keeping the published API.
`rs/moq-relay/src/cluster.rs` `run_remote` on `release` has the same dead
`Ok(()) if elapsed >= stable_threshold` branch, because `run_remote_once` only
returns an error. Report whether the attempt connected (any observation with
`connects > 0`) and pass it to `Redial::next`, which starts over at the first
ceiling when it did, including on the immediate retry after the cluster token
rotated.

Do not port `cluster.api.root` without `cluster.api.url` or `cluster.token`
with no peer to dial as startup errors: those stay on `main`.
