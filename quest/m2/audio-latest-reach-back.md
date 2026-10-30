# [S] Audio's Latest start reaches back by min plus warmup

## Goal

`moq_audio`'s decoder under `Start::Latest` starts at the group covering live edge − (`delay.min` + warmup), as the other consumers do since [moq-dev/moq#7311](https://github.com/moq-dev/moq/pull/7311). Today it floors at the live-edge group and primes its warmup by trimming.

## Plan

Decided 2026-10-30 (maintainer, follow-up planning): m2. Use the same start resolution the container consumer uses rather than a second one. With `min` and warmup at 0, the start is unchanged. Add a test of a cold join with a 2-group-old start.

Public API: none. Wire: none.

## Required

- [A max-delay decoder joins an earlier group](/quest/m2/intra-refresh/join-earlier-group.md) - its PR, #7311, adds the start rule
