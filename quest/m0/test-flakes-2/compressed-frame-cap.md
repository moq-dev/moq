# [S] js/net's compressed-frame cap test holds up under load

## Goal

`js/net/src/group.test.ts`'s "a compressed frame is refused unless its
worst-case slice fits a peer's frame cap" passes on a loaded host, and its
`test.skip` is removed. It still proves the cap: a frame whose worst-case
slice exceeds the peer's cap is refused, and one that fits is accepted.

## Plan

Planned 2026-11-09 (maintainer, /quest-complete), under the repository's
flaky-test rule. Quarantined by
[moq-dev/moq#8333](https://github.com/moq-dev/moq/pull/8333) after the
whole `js/net` package failed it 2 of 30 times under 48 CPU burners on 24
cores, first seen in [moq-dev/moq#8326](https://github.com/moq-dev/moq/pull/8326)'s
loaded runs.

The failure is bun's per-test limit ("timed out after 5000ms", at 5840 ms and
10442 ms). Alone it takes about 0.85 s: it deflates and inflates a random
payload of almost 32 MiB, so the CPU work, not a race, runs past the limit
under load. This resembles the flate tail-shed test fixed in
[moq-dev/moq#8137](https://github.com/moq-dev/moq/pull/8137). Prove the cap
with a payload sized relative to a small cap (or a cap the test sets low)
rather than a full-size one; never raise the timeout.

Public API: none. Wire: none.
