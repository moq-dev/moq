# [S] moq-c's media consume applies the catalog warmup, and can disable rungs

## Goal

`moq_consume_audio` and `moq_consume_video` start where the moq-ffi and Rust
media consumers do: the catalog's `warmup` widens the start, and the
subscription budget grows by the same warmup, so a warmup rendition's decoder
gets the history it needs. A C publisher of pre-encoded media can also
disable and resume a simulcast rung, as moq-ffi's callers can since [moq-dev/moq#7029](https://github.com/moq-dev/moq/pull/7029).

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7311](https://github.com/moq-dev/moq/pull/7311) and
[moq-dev/moq#7029](https://github.com/moq-dev/moq/pull/7029).

Milestone: ✅ m1. Not chosen: m2.

- Warmup: `rs/moq-c/src/consume.rs:495` (`media`) builds the container
  consumer with only `with_max_delay(max_delay)` (`:531`), on main as well.
  Add `with_warmup` from the rendition's catalog `warmup`, and add the same
  warmup to the subscription's budget, as `moq_audio::decode::Consumer` and
  `moq_video::decode::Consumer` do. [moq-dev/moq#7311](https://github.com/moq-dev/moq/pull/7311) (merged) replaced `with_max_delay` with a
  `Delay { min, max }`; use its shape (C mirrors `MoqDelay`).
- Rung disable ([moq-dev/moq#7029](https://github.com/moq-dev/moq/pull/7029)'s decision 4): `moq_publish_media_set_enabled`, the
  pre-encoded media track's lower rendition and priority, and a C rate
  control over `moq_reservation`, mirroring moq-ffi's `MoqRateControl`,
  `MoqVideoInit.lower`, and `MoqMediaTrackProducer::set_enabled`. Needs [moq-dev/moq#7029](https://github.com/moq-dev/moq/pull/7029).
- Update `moq.h`, `doc/lib/c`, and the C tests for both.

Public API: moq-c gains the warmup on media consume (behavior), and the rung
disable functions. Wire: none.

## Required

- [OBS disables simulcast rungs](/quest/m1/simulcast-rung-disable.md) - [moq-dev/moq#7029](https://github.com/moq-dev/moq/pull/7029)'s FFI rung disable that C mirrors
