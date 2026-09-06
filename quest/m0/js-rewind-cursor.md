# [S] Detect JavaScript container rewinds at the unread playback cursor

## Goal

The first frame of a newer, rewound group carries a discontinuity even when
that group is already the consumer's active sequence.

## Plan

The Rust regression in #3375 exposed a distinction between the playback cursor
and the group that supplied the last delivered live edge. The JavaScript
consumer has the same risky shape: completion advances `#active` to the next
sequence, frame ingestion skips `#checkReset` for that active sequence, and
`#checkReset` itself rejects sequences at or below `#active`.

- Reproduce an old group at a high timestamp, drain it, then deliver exactly one
  sequential group starting at zero. Assert the first new frame's counter.
- Base eligibility on the delivered live-edge group rather than an unread cursor.
  Preserve stale-group classification before checking for a new rewind.
- Cover an already-buffered successor, a later arrival, and B-frame reordering
  within one group. Run the existing consumer and playback coverage.

## Related

- [Rust rewind recovery](https://github.com/moq-dev/moq/pull/3375)
- [Gap discontinuity](/quest/m1/gap-discontinuity.md) - future boundary signalling rules
