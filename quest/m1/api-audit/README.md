# [S] A smaller, safer API than release

## Goal

When `main` is cut into `release`, its public API is net smaller and harder
to misuse than `release`'s. Every surface added since release has a consumer
or is removed or made private. Overlapping operations are collapsed into one.
Names mirror across Rust, JS, and the bindings. Every break says how
to migrate.

This epic's own work, now that its children have landed: settle the surfaces
the final pass left for the maintainer (see Waiting), then make private or
remove what the decisions say.

Non-goals: redesigning independently scoped surfaces
([subscribe ranges](/quest/m2/subscribe-ranges/README.md),
[archives](/doc/concept/archives.md),
[compressed tracks](/doc/concept/compression.md),
and `Sync`'s per-track clock handles), and inherited debt that a later quest
already reshapes.

## Plan

Planned 2026-10-22 from an audit of `release` (36f392f492) against `main`
(d44813cb70), 3,725 files changed. Seven read-only audits covered Rust
moq-net, `@moq/net`, the media crates, the bindings, `@moq/watch` and
`@moq/publish`, the native, relay, and transport crates, and the gateway and
misc crates. Consumers were checked in this repository, moq.pro (excluding its
vendored checkout), and moq.dev, the npm consumer of `@moq/watch` and
`@moq/publish`. A prior external audit was folded in; its corrections are
recorded where they matter. The maintainer approved every recommendation
wholesale ("quest-plan anything that you recommend"), so the choices in each
child's Plan are settled.

Rules every child follows:

- Removal beats addition. A surface with no consumer in the repository,
  moq.pro, or moq.dev goes private or away. A consumer planned in a quest
  counts only when that quest is in m1 or m2.
- Inherited debt is fixed here only when it is cheap or sits on a surface
  this release breaks anyway. Otherwise it joins the later quest that
  reshapes it (the ordered track reader joins rust2's
  [exporter](https://github.com/moq-dev/rust2/blob/main/quest/m1/ffi-export.md),
  which absorbed its foreign poll quest on 2026-10-25).
- Each child lists its public API and wire impact in its PR, and its Impact
  bullet says how to migrate.

The binary-to-flate fold was followed by flate's removal in the
[compression migration](https://github.com/moq-dev/moq/pull/7046), so moq-flate, `@moq/flate`, and
`moq-mux::binary` have no API left to audit. hang's
`Archive::replay_epoch` went with the root `archive` section in catalog HLS.

Kept public though only moq-transport uses them (decided 2026-10-23), so an
audit skips them:

- moq-transport-only error variants (`Error::{InvalidJoiningRequestId,
  InvalidRange, Remote, UnsupportedExtension}`, the `DecodeError` auth-token
  variants) and the session error codes the lite draft registers with
  moq-transport's meaning. Errors are fine; only dead ones go.
- `setup::Extensions::{auth, solicit}` and the relay's
  `[listen|connect.extensions]` TOML, as interop knobs documented as
  affecting moq-transport only. `epoch` went with moq-transport's epochs in
  [moq-dev/moq#7239](https://github.com/moq-dev/moq/pull/7239).

Rejected in planning:

- Restoring `track::Demand::{used, unused}`: refresh
  decided one `wanted()` signal.
- Removing `Audio.Decoder.arrivals()`: moq.dev has a planned consumer
  waiting on this release. Only the video handle went.
- Pulling hang's `Projection`, `Stereo`, and `AudioLayout`: the maintainer
  decided 2026-10-20 to land the fields first.
- One terminal verb across bindings: Kotlin's `end` exists only because
  `close` collides with `AutoCloseable`, and no single verb fits every
  language's reserved names.
- A follow policy enum folding `linger` and `stitch`: one name per meaning
  (the export wait is `--follow`) fixes the confusion more cheaply.

The final pass (2026-11-06, [moq-dev/moq#8088](https://github.com/moq-dev/moq/pull/8088))
diffed rustdoc JSON and TypeScript declarations at `release`, the audit base,
and `main` (f2f9bc935e), and made private, removed, or renamed about 70
surfaces added since the audit. It kept these public on purpose, so a later
audit skips them:

- `moq_auth::mesh`'s `Client`, `Signer`, and coordinator handlers: moq keeps
  the mesh libraries for outside daemons (decided 2026-11-02).
- moq-audio's `aec` module: callers with their own audio stack use it
  (the maintainer's decision cited in
  [moq-dev/moq#7369](https://github.com/moq-dev/moq/pull/7369)).
- `@moq/json` `Snapshot.Config.maxGroupBytes`: chosen in
  [moq-dev/moq#7072](https://github.com/moq-dev/moq/pull/7072).
- `@moq/hang` `Health`: moq.pro's m2 `qos-connection-health` and
  `qos-preflight` quests plan the JS consumer; studio uses the Rust mirror.
- `@moq/watch` `Broadcast.include`: includes stay for moq.pro transcodes
  (decided 2026-11-04).
- `MoqTrackInfo.{max_age, max_age_unlimited}`: UniFFI 0.32 cannot give an
  optional `Duration` a zero default, so the pair stays until it can.
- The root re-exports `moq_net::{Reconnect, Server, Session}`, which follow
  one existing pattern: fix all of them together or none.

## Waiting

- Maintainer: keep the m3 realtime-uplink bake-off surfaces public
  (`moq_congestion::{gcc, scream}`, moq-tokio's `quic::Controller` hook with
  `Config.controller` and `Resolved::has_controller`, and moq-quic's
  `transport::Config.receive_timestamps`), or make them private now that
  [Realtime uplink congestion control](/quest/m3/quic-realtime/README.md) is
  m3, moving the nightly SCReAM socket test into moq-quic's sim.
  Recommendation: keep them, recorded as that quest's surface, since removing
  them deletes the nightly measurement it runs on. An agent cannot weigh the
  rule against the bake-off's schedule.
- Maintainer: keep `moq_crypto::sign` (broadcast, track, and group signing,
  about 1.3k lines) or remove it. Only `SecretKey` and `PublicKey` have
  consumers; the rest has only the m3 [stream](/quest/m3/stream/README.md)
  plan and draft-lcurley-moq-sign. Recommendation: remove the signing
  producers and consumers and keep the keys, restoring them with the m3 work.
- Maintainer: one `web::Error` per crate for moq-tokio's and moq-uring's
  `quic::web`, typed (tokio's five nested types folded into one enum with
  `Connection`, `Closed`, `SessionGone`, `UnknownSession`, `Http3`,
  `Handshake`, `Read`, and `Write` variants) or flat (uring's string-based
  `quic::Error`). Recommendation: typed, splitting uring's `Web` and `Http3`
  variants out of its `quic::Error`.
- Maintainer: `moq_uring::Config.passes` is public only for the nightly
  `worker_turns` 1-versus-4 sweep. Keep the sweep and the knob, or drop the
  axis and make it private. Recommendation: keep it until the sweep has chosen
  a value.
- Maintainer: moq-stats' `aggregate::{Consumer::group, GroupConsumer}` and
  `group_track` are public for the release-perf aggregate bench and the
  "scope by group" advice in `doc/concept/stats.md`. Keep them as that
  documented path, or delete the group merge and move the bench to the prefix
  merge. Recommendation: keep them.
