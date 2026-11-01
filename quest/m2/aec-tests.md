# [S] aec::Processor's edge cases are tested

## Goal

moq-audio tests cover `aec::Processor` at the extremes of its accepted
input: 8 kHz and 384 kHz sample rates, render and capture streams with
mismatched formats (refused, or converted as documented), and a `reset`
while a partial frame is buffered, which neither panics nor leaks the
partial frame into the next output.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7369](https://github.com/moq-dev/moq/pull/7369): m2, as
recommended. [moq-dev/moq#7369](https://github.com/moq-dev/moq/pull/7369) (merged) exposes `aec::Processor`,
`aec::Stream`, and `aec::Stats` for caller-supplied PCM.

Add the tests beside [moq-dev/moq#7369](https://github.com/moq-dev/moq/pull/7369)'s unit tests in `rs/moq-audio/src/aec/`. Fix any
bug they find in the same PR; if a rate is outside what Sonora accepts,
assert the refusal rather than widening support.

Public API: none. Wire: none.
