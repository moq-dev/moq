# [XS] moq-json stream and window producers drop cleanly when unwanted

## Goal

Dropping an unfinished `moq-json` stream or window `Track` producer when nobody wants the track closes it cleanly instead of aborting it with `Error::Dropped`, while dropping an unfinished producer that still has active subscribers aborts with `Error::Dropped` and never hands a subscriber to a replacement renewal producer that cannot continue the unfinished log or window.

## Plan

Decided 2026-11-28 (maintainer, `/quest-plan` of the pre-0.22 design-gate audit): ✅ restore clean idle on-demand teardown in `moq-json` before `0.22.0` (rejected: leaving idle unwanted drops aborting with `Dropped`).

Caused by [moq-dev/moq#9550](https://github.com/moq-dev/moq/pull/9550), which deleted `abort_unwanted` across Rust and the bindings while `Drop for Track` in `rs/moq-json/src/stream/producer.rs:289-294` and `rs/moq-json/src/window/producer.rs:432-442` kept calling `self.abort(moq_net::Error::Dropped)` unconditionally whenever `self.group.is_some() && self.inner.finished().is_none()`. Without a race-free idle teardown, an on-demand JSON publisher whose `wanted_changed()` goes idle can neither drop cleanly (`Demand::closed()` returns `Err(Dropped)`) nor call `finish()` (which falsely declares the stream/window complete).

- Do **not** check `demand().wanted()` outside the subscriber-join lock before dropping: a subscriber can join right after `wanted()` returns `false`, and dropping the producer would then take the renewal path and hand that subscriber to a replacement producer that cannot continue the unfinished JSON log or window.
- Instead, decide between clean idle close and `Error::Dropped` abort atomically under `moq-net`'s subscriber-join lock in the underlying drop/teardown path (with renewal disabled for an unfinished `moq-json` stream/window track), so either no subscriber joined (clean idle close) or any subscriber that joined sees `Error::Dropped` (never silent renewal onto an unfinished stream/window).
- Tests: unit tests for both `stream` and `window` covering (1) dropping an unfinished producer while a subscriber is attached aborts with `Dropped`, (2) dropping an unfinished producer after all subscribers leave closes cleanly without `Err(Dropped)` and allows a later on-demand request to start fresh, and (3) racing a late subscriber join against idle teardown never renews onto an unfinished track.

Public API: none (behavior fix restoring clean on-demand teardown, or minimal crate/public teardown helper if needed). Wire: none.
