# [S] A created first group reads differently locally and through a relay

## Goal

A late oldest-group, key-order reader of a finished track whose first group came from
`track::Producer::create_group` gets the same result locally as through a relay. Today the
local reader gets a `NotFound` gap over the keys below that group, while through a relay whose
copy a latest-group reader filled first it gets the group.

## Plan

Decided 2026-11-24 (maintainer, kixel.ai /quest-run): planned from kixel-ai/kixel.ai#269's follow-ups.
m3, since it is a bug fix in shipped code.

Found by [kixel-ai/kixel.ai#269](https://github.com/kixel-ai/kixel.ai/pull/269).
[moq-dev/moq#9534](https://github.com/moq-dev/moq/pull/9534) makes a track's first appended group
skip every key below it, but a created group says nothing about the keys around it.

Reproduce it first. A scratch integration test (not committed) on `main` at `2d3bc05f09`
(lite-07-wip, unbounded pools, proxy origins) wrote one group at a wall-clock key K with
`create_group`, finished the group and the track, then read with `Start::Oldest`, in
ascending order:

- 0 links (local): `gap 0..K not found`
- 1 or 2 relays, with no earlier reader: `gap 0..K not found`
- 1 or 2 relays, after a `Start::LATEST` reader read group K from the same copy: `group K`

With the first group appended instead, every one of those cases reads `group K`. The relayed
answer therefore depends on what the copy already holds, which the root `AGENTS.md` cache rule
forbids. Find where the warmed copy places the oldest group at K without the proof the publisher
has, and settle which answer is right for keys a finished track never created. Both paths must
give that answer, and the reproduction becomes the regression test.

## Related

- [Test an oldest reader behind a relay copy a latest reader filled first](/quest/m4/time-keyed-warmed-relay-test.md) - the appended-group case, which already reads the same through a relay
- [moq-dev/moq#9529](https://github.com/moq-dev/moq/pull/9529) adds `quest/m1/relayed-refused-fill-gap.md` - the converse: a relayed reader reports a gap that a direct reader does not
