# [S] A moq-transport finish settles a declared end's tail

## Goal

When a moq-transport copy finishes its track (`Producer::finish_shared`, reached from `ietf/subscriber.rs` and `ietf/publisher.rs`), an end declared earlier with a pending tail settles at once, on a timed track and on an errored end too. No reader waits out a grace for groups the finish already ruled out.

## Plan

Audit finding `S12` from the 2026-11-24 subscribe/fetch audit, moved out of [Replace lite-07 SUBSCRIBE_SKIP and stall timers with Previous Delta](/quest/m2/lite07-subscribe/previous-delta.md): lite-07 copies never reach `finish_shared`, and the lite-07 epic leaves `moq-transport` out of scope.

`finish_shared` (`rs/moq-net/src/model/track.rs`) clears the declared end's tail only when that end has no error and sits exactly at `Position::group(next_key())`:

- A timed track keys groups by start time, so an end declared past the last group's start never matches `next_key()`, and its tail keeps the grace running.
- An errored end (`declared.error.is_some()`) keeps its tail even when its position matches.

Reproduce each case with a unit test before changing it, then settle the tail whenever the finish rules out every key the tail still owes, keeping the declared error.
