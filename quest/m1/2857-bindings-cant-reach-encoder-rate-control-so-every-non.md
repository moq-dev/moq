# [M] Bindings can't reach encoder rate control, so every non-Rust publisher ignores congestion

## Goal

Implement and verify the behavior tracked in [#2857](https://github.com/moq-dev/moq/issues/2857)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Noticed while wiring [#2854](https://github.com/moq-dev/moq/pull/2854).

#### Problem

Every non-Rust publisher encodes at its configured bitrate regardless of congestion. There is no way to reach the encoder rate control from a binding:

- `moq-ffi` and `libmoq` publish video (`moq_publish_video_raw`) and audio (`moq_audio::encode::Options::default()`), but never set `Options::bandwidth`.
- The send estimate is exposed only as a **snapshot**, on `moq_connection_stats` / `Session::stats` (`send_rate_bps` + a `_valid` flag). A caller can read a number; it cannot hand a live handle to an encoder.

So an OBS plugin, or a Python/Swift/Kotlin/Go publisher, overshoots a closing uplink exactly as far as its configured bitrate is above what the link can carry, and keeps overshooting until the operator changes the setting by hand. The Rust path has followed the estimate since `moq_video::encode::rate` landed.

This predates the bandwidth allocator; [#2854](https://github.com/moq-dev/moq/pull/2854) didn't regress it, it just made the hole visible by giving the Rust side a second thing bindings can't reach.

#### What a fix needs

The awkward part is that both `bandwidth::Consumer` and `bandwidth::Allocator` are live handles with wakeups, which is not a shape UniFFI or a C ABI carries naturally. Options, roughly cheapest first:

- **A flag on the publish call**: "follow this connection's estimate", with the binding creating the allocator and registering the track on the caller's behalf. No new handle type crosses the boundary, and it's the behavior nearly every caller wants. Loses the ability to share one allocator across separately-created publishers.
- **An opaque allocator handle** (`moq_allocator_*` / a UniFFI object) minted from a session and passed into each publish call. Mirrors the Rust API exactly and composes across publishers, at the cost of a new object in five wrappers.
- **A polled getter** returning the current target for a track, leaving the caller to drive their own encoder. Fits an application that owns its encoder (OBS does), useless for the built-in encode path.

The first two are not exclusive: the flag can be the default and the handle an escape hatch.

Whichever way, this touches `rs/moq-ffi`, `rs/libmoq`, `{py,swift,kt}/`, `go/wrapper/moq/`, and `doc/lib/{py,swift,kt,go,c}` per the Cross-Package Sync table, plus `cpp/obs` if the OBS plugin should adopt it.

## Closes

- [#2857](https://github.com/moq-dev/moq/issues/2857) - close this issue when the quest finishes
