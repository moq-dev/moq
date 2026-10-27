# [XS] graviola's own CPU check

## Goal

A graviola release ships a public CPU feature check that names the missing
feature ([ctz/graviola#227](https://github.com/ctz/graviola/issues/227)), so
[moq-relay on graviola](/quest/m3/pure-crypto/relay-graviola.md) calls it at
startup instead of mirroring graviola's mandatory feature list. Check the issue
and graviola's changelog. This quest only waits; delete it once that holds.

## Plan

A check that answers only yes or no does not satisfy this: the relay's startup
error names the missing feature (maintainer, 2026-10-25). Decided by the
maintainer on 2026-10-26: moq waits passively and contributes no code upstream.

Public API: none. Wire: none.
