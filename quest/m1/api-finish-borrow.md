# [S] Borrow publisher finish so abort can still run

## Goal

A public publisher `finish` borrows the handle, matching net track, group, and
broadcast, so `abort(self)` can still run after a clean finish.

## Plan

Net already documents this split at `origin/dev` `b5a289a05`:
`track::Producer::finish(&mut self)` is not terminal (lower-numbered groups may
still arrive; abort still consumes), `group::Producer::finish` borrows so a
later failure can still abort, and `broadcast::Producer::finish` borrows so
declaring the end does not depend on surrendering the handle. Abort consumes.

An earlier plan to make `finish` consume the handle was abandoned for this
reason: that makes abort-after-finish unrepresentable.

Public publisher `finish(self)` still in the tree:

- `moq_json::window::Producer::finish` (`rs/moq-json/src/window/producer.rs`),
  while snapshot and stream already borrow
- `moq_room` chat `finish` (`rs/moq-room/src/chat.rs`), which only forwards to
  the window producer
- `moq_audio::encode::Producer::finish` (`rs/moq-audio/src/encode/producer.rs`)
- `moq_video::encode::Producer::finish` (`rs/moq-video/src/encode/producer.rs`)

Change those to `finish(&mut self)`. Internal helpers that exist only to
forward (audio `Reserved`, capture `Track`) follow. Encoder drain that
returns packets (`Encoder::finish(self) -> Vec<Encoded>` and friends) stays
consuming: that is a builder, not a publisher close.

FFI already exposes `finish(&self)` but `take()`s the inner `Option` first
(`MoqTrackProducer`, `MoqGroupProducer`, `MoqBroadcastProducer`, audio, video,
JSON snapshot/stream). Finish in place so a later `abort` can still take the
handle. UniFFI signatures stay `&self`.

Out of scope: Drop-disarm guards (`SourceGuard`, `recv::Group` / `recv::Frame`)
whose consuming finish is what stops `Drop` from aborting; `frame::Producer`
commit; JS, which has no abort and already cannot consume.

Do not add a consuming alias. A failed finish leaves the handle, as `&mut self`
already does. Keep last-drop cleanup. Clones are independent, same as today.

Prove abort after a successful finish on each migrated publisher, including
the FFI take-in-place path. Existing call sites that only finish do not need
rewrites; drop any `Option::take` that existed solely to call a consuming
finish.

Public API: breaking for callers who bind a non-`mut` producer and call
`finish()` (Rust); FFI behavior change (finish no longer closes the handle).
Wire: none. Run the affected Rust and FFI check/test recipes.

## Related

- [External API proof](/quest/m1/api-release-proof.md) - records this disposition
