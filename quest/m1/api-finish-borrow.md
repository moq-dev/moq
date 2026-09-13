# [S] Borrow publisher finish so abort can still run

## Goal

A public publisher `finish` borrows the handle, matching net track, group, and
broadcast, so a later `abort(self)` is still representable.

## Plan

Net already documents this split at `origin/dev` `b5a289a05`:
`track::Producer::finish(&mut self)` is not terminal (lower-numbered groups may
still arrive; abort still consumes), `group::Producer::finish` borrows so a
later failure can still abort, and `broadcast::Producer::finish` borrows so
declaring the end does not depend on surrendering the handle. Abort consumes.

An earlier plan to make `finish` consume the handle was abandoned for this
reason: that makes abort-after-finish unrepresentable.

Public publisher `finish(self)` still on `dev`:

- `moq_json::window::Producer::finish` (`rs/moq-json/src/window/producer.rs`),
  while snapshot and stream already borrow. Window has no `abort`; this is
  signature consistency only.
- `moq_audio::encode::Producer::finish` (`rs/moq-audio/src/encode/producer.rs`)
- `moq_video::encode::Producer::finish` (`rs/moq-video/src/encode/producer.rs`)

Change those to `finish(&mut self)`. Internal helpers that exist only to
forward (audio `Reserved`, capture `Track`) follow. Encoder drain that
returns packets (`Encoder::finish(self) -> Vec<Encoded>` and friends) stays
consuming: that is a builder, not a publisher close.

FFI already exposes `finish(&self)` but `take()`s the inner `Option`. Only
`MoqTrackProducer` and `MoqGroupProducer` also expose `abort`. Finish those
two in place so a later `abort` can still take the handle. Do not add `abort`
to broadcast, audio, video, or JSON FFI wrappers, and do not change their
`take()` on finish.

UniFFI signatures stay `&self`, so generated Dart/Kotlin/Python/Swift bindings
do not regenerate. If a binding guide says finish releases the handle, fix
that sentence (`doc/lib/{py,swift,kt,go,dart,c}` as needed). libmoq's
id-keyed `Session::finish` is a different shape and stays out.

Out of scope: Drop-disarm guards (`SourceGuard`, `recv::Group` / `recv::Frame`)
whose consuming finish is what stops `Drop` from aborting; `frame::Producer`
commit; JS, which has no abort and already cannot consume; adding abort to
types that lack it.

Do not add a consuming alias. A failed finish leaves the handle, as `&mut self`
already does. Keep last-drop cleanup. Clones are independent, same as today.

Prove abort after a successful finish on audio and video producers and on FFI
track/group. For JSON window, prove the handle remains usable after finish
(no `abort` to call). Existing call sites that only finish do not need
rewrites; drop any `Option::take` that existed solely to call a consuming
finish.

Public API: breaking for callers who bind a non-`mut` producer and call
`finish()` (Rust); FFI track/group finish no longer closes the handle.
Wire: none. Run the affected Rust and `moq-ffi` check/test recipes, including
`finish_closes_producer` and the Python local tests that encode take-on-finish,
then `just test smoke-full` because `moq-ffi` behavior changes.

## Related

- [External API proof](/quest/m1/api-release-proof.md) - records this disposition
