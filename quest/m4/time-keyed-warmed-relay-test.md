# [XS] Test an oldest reader behind a relay copy a latest reader filled first

## Goal

`rs/moq-net/tests/integration/time_keyed_start.rs` covers an oldest-group, key-order reader
of a time-keyed track behind a relay whose copy of the track a latest-group reader filled
first. That reader still gets the track's first group, not a gap below it.

## Plan

Decided 2026-11-24 (maintainer, kixel.ai /quest-run): planned from kixel-ai/kixel.ai#269's follow-ups.
m4, since it is a test nobody waits on: it passes on `main`.

[kixel-ai/kixel.ai#269](https://github.com/kixel-ai/kixel.ai/pull/269) traced it: the relay
lowers its downstream START to 0 for the speculative fill below the copy's start, then sends the
skip of the keys below the first group, and the reader gets the first group. The skip from
[moq-dev/moq#9534](https://github.com/moq-dev/moq/pull/9534) is recorded under the same lock that
creates that group, so it always arrives ahead of the relay's lowered START 0. This test pins
that ordering. It is the only case that covers START below the first group, then the skip.

Use two relays, a live track whose first group is appended, and a latest-group reader that
reads that group before the oldest-group reader subscribes. Extend the file's `first_item`
helper or add a case beside it.

Public API: none. Wire: none.

## Related

- [A created first group reads differently locally and through a relay](/quest/m1/created-first-group-relay-parity.md) - the same reader on a finished track whose first group was created, where the local and relayed answers differ
