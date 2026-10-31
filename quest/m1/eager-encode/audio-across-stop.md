# [S] Buffered audio survives a capture stop and a disable

## Goal

Audio retained by default (buffered) capture is still served, or expires on
its own `max_age`, after capture stops: in Rust, across `Control::stop` and
parking after a terminal input failure; in JS, across disabling the input. A
subscriber who arrives while capture is stopped gets the retained history it
would have got a moment earlier. Stopping never encodes the retained history
eagerly.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7238](https://github.com/moq-dev/moq/pull/7238).

Milestone: ✅ m1. Not chosen: m2.

- [moq-dev/moq#7238](https://github.com/moq-dev/moq/pull/7238)'s overnight decision (2026-10-31, on the maintainer's standing
  authorization): "retained audio when capture stops: accept for now, follow
  up in a new quest" (option 1). Rejected there: flushing retained history on
  stop (encode CPU on every mute), and keeping demand and expiry running
  while stopped inside [moq-dev/moq#7238](https://github.com/moq-dev/moq/pull/7238) (the largest change, especially in JS).
- Today, in Rust, `Control::stop` or parking stops demand and expiry, so the
  retained PCM is unreachable; in JS, disabling discards the buffer. Keep the
  demand watch and expiry alive while stopped, and encode on demand from the
  retained buffer as when capturing.
- JS overlaps [push-to-talk](/quest/m1/push-to-talk/README.md)'s plan to keep
  the codec setup alive across a disable; share that mechanism rather than
  adding a second one.
- The CLI's `buffered` audio default is owned by
  [cli-modes](/quest/m1/eager-encode/cli-modes.md), not this quest.

Public API: none expected. Wire: none.

## Required

- [Audio encodes on demand from a raw backbuffer](/quest/m1/eager-encode/raw-audio.md) - [moq-dev/moq#7238](https://github.com/moq-dev/moq/pull/7238) lands buffered audio by default
