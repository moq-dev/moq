# [S] lite-07 carries an untimed track

## Goal

On moq-lite-07, an untimed track's frames and datagrams cross the wire as
untimed in both Rust and JS, so absence survives a lite relay hop. lite-05 and lite-06
keep writing the encoder's send time, documented as a downgrade for old
peers. Covers `rs/moq-net`, `js/net` and `drafts/draft-lcurley-moq-lite.md`.

## Plan

Decided (2026-10-05, maintainer): timedness is per track ([untimed
model](/quest/m1/untimed-model.md)), and an untimed track sends no
TIMESCALE and no Timestamp. On lite-07 that likely means an optional
Timescale in TRACK_INFO and no Timestamp fields on an untimed track. The
per-frame shift below was planned for an absent timestamp inside a timed
track, which no longer exists (settled 2026-10-06: a mismatched frame is
refused), so re-plan the encoding on that shape and drop the shift if
nothing needs it.

Decided (2026-10-01, maintainer): shift the FRAME Timestamp Delta and the
DATAGRAM Timestamp by one, so 0 means absent. An absent frame doesn't move
the delta baseline. Rejected: a bare 0 as a sentinel, which collides with a
real pts of 0.

Decided (2026-10-02): one PR for both languages, after both
model quests. Shipping one language first would break Rust-JS interop on
lite-07-wip in between.

Draft work:

- The draft describes only the current wire. Add the lite-07 encoding, the
  lite-05/06 downgrade, and a lite-07 changelog entry.
- Fix the text that assumes every frame is timed: "each frame carries a
  presentation timestamp", the datagram's "any varint value (including 0) is
  valid", and expiration's "reach" (the first frame timestamp of the next
  group) for an untimed group.
- Check how a lite track that has no timescale says so on lite-07.
- lite-07 varints carry the full 64 bits, so the shift can't represent the
  top value: a DATAGRAM Timestamp of 2^64-1, or a delta whose zigzag
  encoding is 2^64-1. Wrapping would turn either into the absence marker.
  Recommended: the encoder refuses those values and the draft says so,
  rather than widening the encoding for timestamps no real track reaches.

Test: an untimed frame and an untimed datagram round-trip Rust-to-JS and
JS-to-Rust on lite-07. lite-06 still receives a timestamp. Run
`just drafts check` and `just test interop --all`.

Public API: none. Wire: lite-07-wip only, which is unpublished.

## Required

- [moq-net carries untimed frames faithfully](/quest/m1/untimed-model.md) - the Rust model must hold an absent timestamp before the wire can carry one
- [@moq/net carries untimed frames faithfully](/quest/m1/js-untimed-model.md) - the same for JS
