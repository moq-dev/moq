# A smaller, safer API than release

## Goal

When `main` is cut into `release`, its public API is net smaller and harder
to misuse than `release`'s. Every surface added since release has a consumer
or is removed or made private. Overlapping operations are collapsed into one.
Names mirror across Rust, JS, and the bindings. Every break has its
`MIGRATE.md` entry.

Non-goals: redesigning surfaces that already have their own quest
([subscribe ranges](/quest/m1/subscribe-ranges/README.md),
[archives](/quest/m1/archive/README.md),
[compressed tracks](/quest/m1/compress/README.md),
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
  reshapes it (the ordered track reader joins
  [foreign poll](https://github.com/kixelated/rust2/blob/main/quest/m1/ffi-poll-protocol.md); binding durations get
  [their own m2 quest](/quest/m2/ffi-durations.md)).
- Each child lists its public API and wire impact in its PR and writes its
  own `MIGRATE.md` entries.

Already gated elsewhere, so not children: the archive `Producer` and `Reader`
disagree on layout while both claim version 2
([archive consumer](/quest/m1/archive/consumer.md)), and the binary-to-flate
fold is followed by flate's removal ([compress migrate](/quest/m1/compress/migrate.md)).
Both are in m1, so the cut waits for them. hang's
`Archive::replay_epoch` goes with the root `archive` section in
[catalog HLS](/quest/m1/archive/catalog-hls.md).

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
  ([TS export follows by cause](/quest/m1/broadcast-lifecycle/ts-follow.md)
  renames the export wait `--follow`) fixes the confusion more cheaply.

## Required

- [MIGRATE matches release](/quest/m1/api-audit/migrate.md) - every break already on main has a correct entry measured against release, and entries for unreleased packages are gone
- [Two broadcast resolutions](/quest/m1/api-audit/resolve.md) - Rust, JS, and the bindings resolve a broadcast with `request(path, epoch)` or `follow(path, epoch)`, and every racy two-step caller is gone
- [The origin announces](/quest/m1/api-audit/announce-owner.md) - a broadcast producer cannot announce; the origin hands out the announcement, so `Prepared` and standalone broadcasts lose methods that always fail
- [Unpublish what nothing uses](/quest/m1/api-audit/unpublish.md) - packages, features, and listeners with no consumer stop shipping or stop being on by default
- [Container formats name inputs](/quest/m1/api-audit/container-format.md) - `ContainerFormat` names what is parsed, never how it is packaged; `ContainerStream` and TS passthrough internals are gone
- [Binding connection state](/quest/m1/api-audit/binding-state.md) - one `state()` snapshot and `state_changed()` replace `status`, `connects`, and both `failure` polls
- [moq-net exports with a consumer](/quest/m1/api-audit/net-exports.md) - Rust and JS model exports without a consumer go private, wrong ones are deleted, and JS names mirror Rust
- [One clean close](/quest/m1/api-audit/clean-close.md) - a clean session close has one spelling and failures are recorded inside moq-net
- [Ranges refuse what the wire drops](/quest/m1/api-audit/ranges-wire.md) - relative and time anchors fail loud where a wire cannot carry them, `floor()` honors the subscription, and bindings take an optional max delay
- [Derived origins borrow](/quest/m1/api-audit/derived-origin-close.md) - closing a JS `scope()` or `withPreference()` handle releases only that handle
- [Wrapper config names](/quest/m1/api-audit/wrapper-config.md) - Python, Go, and Dart client options use the FFI's names and nesting
- [Bindings read gaps and ends](/quest/m1/api-audit/decode-read-bindings.md) - moq-ffi and every wrapper surface `Gap` and `End` instead of discarding them
- [Media surface trim](/quest/m1/api-audit/media-trim.md) - media crate items with no consumer go private, and Rust/JS media names agree
- [One frame rate name](/quest/m1/api-audit/frame-rate.md) - core `moq_video::Rate` becomes `FrameRate`, as the bindings name it
- [CLI and gst flags](/quest/m1/api-audit/cli-flags.md) - one flag name per meaning across CLI subcommands and gst elements, and `linger` means one thing
- [Transport config](/quest/m1/api-audit/transport-config.md) - moq-tokio and moq-quic configs have one meaning for unset, enums for modes, no order-dependent setters, and no leftover features
- [Relay config](/quest/m1/api-audit/relay-config.md) - moq-relay and its embedders load config through one path that refuses old spellings, under names that do not collide
- [Session-only items stay private](/quest/m1/api-audit/model-private.md) - model items only the built-in sessions use are private
- [Only sessions accept a request](/quest/m1/api-audit/claim-request-type.md) - accepting an application claim's request fails to compile, and the peer-route hatches are crate-private
- [One range setter](/quest/m1/api-audit/serve.md) - `with_range` replaces seven cursor setters and nothing else of the serving cursor goes public
- [A request's source enforces its validity](/quest/m1/api-audit/answer.md) - a stale source refuses new track requests by itself, with no public additions
- [moq-transport stays private](/quest/m1/api-audit/ietf-private.md) - no public moq-net item exists only for moq-transport, and a client names its request target once as a URL
- [One lifecycle vocabulary](/quest/m1/api-audit/lifecycle.md) - finish, abort, closed, and close mean one thing on every handle, and Rust and JS observers fire alike
- [moq-net names say one thing](/quest/m1/api-audit/net-names.md) - demand says `wanted`, colliding stats and transport names are renamed, and Rust and JS info lookups mirror
- [Request handlers are named requests](/quest/m1/api-audit/requests.md) - `dynamic()` and `Dynamic` become `requests()` and `Requests` at every layer

## Related

- [Claim request type](/quest/m1/api-audit/claim-request-type.md) - removes `accept` from application requests; [the origin announces](/quest/m1/api-audit/announce-owner.md) narrows `Prepared` beside it
- [Readers follow restarts](/quest/m1/sink-restart-follow.md) - moves readers onto the follow operation that [two broadcast resolutions](/quest/m1/api-audit/resolve.md) names
- [API-diff check on release PRs](/quest/m1/api-diff.md) - the backstop that catches breaks this epic misses
- [Main is no slower than release](/quest/m1/release-perf/README.md) - the other release-readiness check
