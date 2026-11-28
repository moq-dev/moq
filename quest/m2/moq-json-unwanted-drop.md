# [XS] moq-json stream and window producers drop cleanly when unwanted

## Goal

Dropping an unfinished `moq-json` stream or window `Track` producer when nobody wants the track closes it cleanly via `close_unwanted` instead of aborting it with `Error::Dropped`, while dropping an unfinished producer that still has active subscribers continues to abort with `Error::Dropped`.

## Plan

Decided 2026-11-28 (maintainer, `/quest-plan` of the pre-0.22 design-gate audit): ✅ restore clean idle on-demand teardown in `moq-json` before `0.22.0` (rejected: leaving idle unwanted drops aborting with `Dropped`).

Caused by [moq-dev/moq#9550](https://github.com/moq-dev/moq/pull/9550), which deleted `abort_unwanted` across Rust and the bindings while `Drop for Track` in `rs/moq-json/src/stream/producer.rs:289-294` (and `rs/moq-json/src/window/producer.rs`) kept calling `self.abort(moq_net::Error::Dropped)` unconditionally whenever `self.group.is_some() && self.inner.finished().is_none()`. Without `abort_unwanted`, an on-demand JSON publisher whose `wanted_changed()` goes idle can neither drop cleanly (`Demand::closed()` returns `Err(Dropped)`) nor call `finish()` (which falsely declares the stream/window complete).

- In `Drop for Track` in `rs/moq-json/src/stream/producer.rs` and `rs/moq-json/src/window/producer.rs`, check whether the track is currently wanted (`self.inner.demand().wanted()` or `write_unwanted` semantics): abort with `Error::Dropped` only while wanted, and let an unwanted drop close cleanly so on-demand publishers can tear down when idle while never silently handing an unfinished stream/window over to an active reader.
- Tests: unit tests for both `stream` and `window` covering (1) dropping an unfinished producer while a subscriber is attached still aborts with `Dropped`, and (2) dropping an unfinished producer after all subscribers leave closes cleanly without `Err(Dropped)` and allows a later on-demand demand to start fresh.

Public API: none (behavior fix restoring clean on-demand teardown). Wire: none.
