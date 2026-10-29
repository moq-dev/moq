# [M] moq-c JSON window producer and consumer

## Goal

moq-c publishes and reads moq-json windows, as moq-ffi does, with tests and
`doc/lib/c`.

## Plan

Found by [moq-dev/moq#6946](https://github.com/moq-dev/moq/pull/6946). The
maintainer decided on 2026-10-25 that the hand-written moq-c ships at this
cut with parity with moq-ffi for every m1 binding feature, and windows are
the gap left. Mirror moq-ffi's window API and its timestamp handling
(nullable `const moq_timestamp *`).

Public API: new moq-c functions. Wire: none.
