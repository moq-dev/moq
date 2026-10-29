# A smaller, safer API than release

## Goal

When `main` is cut into `release`, its public API is net smaller and harder
to misuse than `release`'s. Every surface added since release has a consumer
or is removed or made private. Overlapping operations are collapsed into one.
Names mirror across Rust, JS, and the bindings. Every break says how
to migrate.

Non-goals: redesigning surfaces that already have their own quest
([subscribe ranges](/quest/m1/subscribe-ranges/README.md),
[archives](/quest/m1/archive/README.md),
[compressed tracks](/doc/concept/compression.md),
[A/V clock](/quest/m1/av-clock.md)), and inherited debt that a later quest
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
  counts only when that quest is in m1.
- Inherited debt is fixed here only when it is cheap or sits on a surface
  this release breaks anyway. Otherwise it joins the later quest that
  reshapes it (the ordered track reader joins rust2's
  [exporter](https://github.com/kixelated/rust2/blob/main/quest/m1/ffi-export.md),
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
- `setup::Extensions::{auth, solicit, epoch}` and the relay's
  `[listen|connect.extensions]` TOML, as interop knobs documented as
  affecting moq-transport only.

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

## Required

- [Two broadcast resolutions](/quest/m1/api-audit/resolve/README.md) - Rust, JS, and the bindings resolve a broadcast with `request(path, epoch)` or `follow(path, epoch)`, and every racy two-step caller is gone
- [Bindings default to zero max delay](/quest/m1/api-audit/binding-max-delay.md) - a no-argument subscription skips superseded groups in every binding, as Rust, JS, and Python do, and no subscriber limit is asked for explicitly
- [Demand is wanted, everywhere](/quest/m1/api-audit/wanted.md) - every `used`/`unused` pair becomes `wanted()` and `wanted_changed()`, and unused ones go
- [One strict path rule](/quest/m1/api-audit/strict-paths.md) - `Relative` refuses empty segments, `Path` refuses non-canonical escapes, the error is `path::Error`, and auth mints only strict paths

## Related

- [Main is no slower than release](/quest/m1/release-perf/README.md) - the other release-readiness check
