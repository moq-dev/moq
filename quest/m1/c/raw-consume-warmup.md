# [XS] moq-c's media consume applies the catalog warmup

## Goal

`moq_consume_audio` and `moq_consume_video` start where the moq-ffi and Rust
media consumers do: the catalog's `warmup` widens the start, and the
subscription budget grows by the same warmup, so a warmup rendition's decoder
gets the history it needs.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7311](https://github.com/moq-dev/moq/pull/7311) and
[moq-dev/moq#7029](https://github.com/moq-dev/moq/pull/7029).

Milestone: ✅ m1. Not chosen: m2.

- Warmup: `rs/moq-c/src/consume.rs:493` (`media`, at be39188000) subscribes
  with `with_delay(delay)` (`:531`) and builds the container consumer with
  only `with_delay(delay)` (`:534`), so the catalog's `warmup` never
  reaches it. Add `moq_mux::container::Consumer::with_warmup` from the
  rendition's catalog `warmup`, and add the same warmup to the
  subscription's max delay, as `moq_audio::decode::Consumer` does
  (`rs/moq-audio/src/decode/consumer.rs:193-206` and `:243-246`) and
  `moq_video::decode::Consumer` likewise.
  [moq-dev/moq#7311](https://github.com/moq-dev/moq/pull/7311) (merged)
  replaced `with_max_delay` with a `Delay { min, max }`, which moq-c
  already mirrors as `moq_delay` (`rs/moq-c/src/api.rs:717`).
- Update `moq.h`, `doc/lib/c`, and the C tests.

The pre-encoded rung disable this quest also carried
([moq-dev/moq#7029](https://github.com/moq-dev/moq/pull/7029)'s decision 4)
is planned in [moq-c disables pre-encoded simulcast rungs](/quest/m1/c/api-gaps.md).

Public API: moq-c gains the warmup on media consume (behavior). Wire: none.
