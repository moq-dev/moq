# [M] Rust and JS deliver the same groups from one range table

## Goal

One shared table of range cases pins which groups a subscription delivers, and both Rust and
`@moq/net` pass it. Each case gives a track's groups at the origin, what a copy holds
(complete, sparse, or partly evicted), a lite-07 range (a time start, a Position, or no anchor
with a Min Age, and a time end or none), and the exact group IDs delivered, plus whether the copy
must ask upstream. Every case runs direct from the origin, through a relay copy cold and warm, as
a late join, and across a route switch mid-read. The copy's answer always equals the origin's,
so the cut publishes lite-07 knowing the two implementations agree.

## Plan

Decided 2026-11-20 (maintainer, /quest-plan of the main → release audit): ✅ m2, so the cut
waits on it (rejected: m4 after the cut, since a divergence found then is a lite-07 behavior fix
after publication; dropping it for the per-language tests no relative anchors adds, which never
compare Rust with JS).

The audit reproduced why per-language tests weren't enough: with latest group 9, the same
`relative: 2` through `time: 8` subscription returned `[7, 8]` from a complete JS copy and `[1, 2]`
from one holding `[1, 2, 9]`. [No relative anchors](/quest/m2/time-addressing/no-relative-anchors.md)
removes that anchor, but a live join by Min Age and a time start resolve against holdings the
same way, and nothing compares the two languages' answers.

Recommended:

- One JSON fixture read by a Rust test and a `bun:test` file, as
  `rs/moq-json/tests/window-vectors.json` and `js/json/src/window/vectors.test.ts` already
  share one. Expected outputs are group IDs, never timings.
- Seed it with the audit's sparse-copy case restated without the relative anchor, and the cases
  no relative anchors names: a Min Age reaching past what a copy holds, an oldest reader and a
  live reader on one relay, a time start between held groups linked by Previous Start (which a
  copy may answer from cache) and one that isn't (which must ask).
- The relayed run uses the real copy path in each language, so a JS copy under a Rust origin
  is covered the way `just test relay-fill` already covers fills.
- Drive time with the manual clocks both test suites already use.
- A copy holding only a group's later frames is a case too: the relay fetches the head and
  delivers the group whole, or names it in SUBSCRIBE_DROP when upstream can't supply it.

Public API: none. Wire: none; a divergence this finds is fixed under lite-07-wip before the cut.

## Required

- [No relative anchors](/quest/m2/time-addressing/no-relative-anchors.md) - the range shapes this table pins
