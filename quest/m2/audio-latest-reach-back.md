# [S] Audio's Latest start reaches back by min plus warmup

## Goal

`moq_audio`'s decoder under `Start::Latest` starts at the group covering live edge − (`delay.min` + warmup), as the other consumers do since [moq-dev/moq#7311](https://github.com/moq-dev/moq/pull/7311). Today it floors at the live-edge group and primes its warmup by trimming.

## Plan

Decided 2026-10-30 (maintainer, follow-up planning): m2. Use the same start resolution the container consumer uses rather than a second one. With `min` and warmup at 0, the start is unchanged. Add a test of a cold join with a 2-group-old start. [Audio join budget](https://github.com/moq-dev/moq/pull/7342) changes the same `joining_latest` path in `rs/moq-audio/src/decode/consumer.rs`; whichever lands second builds its start on the other's.

Public API: none. Wire: none.

## Related

- [A max-delay decoder joins an earlier group](https://github.com/moq-dev/moq/pull/7311) - its PR, #7311, adds the start rule
- [Audio join budget](https://github.com/moq-dev/moq/pull/7342) - the other `Start::Latest` join fix in `rs/moq-audio/src/decode/consumer.rs`
