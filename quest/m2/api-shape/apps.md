# [M] Apps' small surface fixes

## Goal

- moq-relay:
  - `Internal::with_uring` becomes crate-private; it takes a type nobody
    outside can build since `uring` went private.
  - The hidden refusals for config keys and flags `release` never shipped
    (`[session]`, `[peer]`, `--peer-*`, `--limits-*-requests`,
    `auth.verifications`) are deleted.
  - `cluster::Options.session` becomes `client`, matching its type and the
    `[limits.client]` key.
  - `Relay::{rtmp, webrtc}` become take-once `&mut self -> Option<_>`, like
    `admissions()`.
  - `session::View` and `shutdown::{Tally, Drained}` get
    `#[non_exhaustive]`.
- moq-webrtc drops the dead `Error::{SessionNotFound, SessionClosed}` and
  hides `p2p` (`#[doc(hidden)]`, as on release) until the
  [m4 p2p work](/quest/m4/p2p/README.md) has a caller.
- moq-srt's `dial` module becomes private; `Client` keeps one path.
- moq-hls gains `Error::is_denied()`, so embedders stop matching nested
  errors.
- `moq export hls`'s listener flags become `--hls-listen` and
  `--hls-tls-{cert,key,generate,root}`, matching the `MOQ_HLS_*`
  environment, with named refusals for the old flags, which shipped.
- moq-boy's Rust crate stops exporting its internals (the JS half landed in
  [moq-dev/moq#8296](https://github.com/moq-dev/moq/pull/8296)).
- moq-relay's `cluster::Options.network`, a `moq_tokio::sim::Network` test
  hook added by [moq-dev/moq#8139](https://github.com/moq-dev/moq/pull/8139),
  is `#[doc(hidden)]` test support, so the published config names no
  simulator type. If an embedder needs injected dials, the shape is a
  take-once `Cluster::dials()` handle, not a config field (added
  2026-11-07, maintainer, /quest-plan from the 2026-11-07 API sweep of open PRs and remaining quests).

## Plan

Found by the 2026-11-07 API shape audit (apps); see the
[epic](/quest/m2/api-shape/README.md). Decided 2026-11-07 (maintainer):
✅ hide `p2p` until m4 (rejected: keep it public; delete it). Gateway ingest
is [gateway-ingest](/quest/m2/gateway-ingest.md). Downstream code that
matches the deleted webrtc variants loses only dead arms.
