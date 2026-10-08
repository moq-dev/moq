# [S] lite-07 carries an untimed track

## Goal

On moq-lite-07, an untimed track's frames and datagrams cross the wire as
untimed in both Rust and JS, so absence survives a lite relay hop. lite-05 and lite-06
keep writing the encoder's send time, documented as a downgrade for old
peers. Covers `rs/moq-net`, `js/net` and `drafts/draft-lcurley-moq-lite.md`.

## Plan

Decided (2026-10-05, maintainer): timedness is per track (the untimed
model, [#4822](https://github.com/moq-dev/moq/pull/4822)), and an untimed track sends no
TIMESCALE and no Timestamp. On lite-07 that likely means an optional
Timescale in TRACK_INFO and no Timestamp fields on an untimed track. The
2026-10-01 shift-by-one (0 means an absent timestamp) is dropped: it served an
absent timestamp inside a timed track, which no longer exists (settled
2026-10-06: a mismatched frame is refused).

Decided (2026-10-02): one PR for both languages. Both model quests have
landed (#4822). Shipping one language first would break Rust-JS interop on
lite-07-wip in between.

Decided 2026-10-08: [Rust untimed default](/quest/m1/rust-untimed-default.md)
lands first, since both edit the same Timescale defaults.

Draft work:

- The draft describes only the current wire. Add the lite-07 encoding, the
  lite-05/06 downgrade, and a lite-07 changelog entry.
- Fix the text that assumes every frame is timed: "each frame carries a
  presentation timestamp", the datagram's "any varint value (including 0) is
  valid", and expiration's "reach" (the first frame timestamp of the next
  group) for an untimed group.
- Check how a lite track that has no timescale says so on lite-07.

Test: an untimed frame and an untimed datagram round-trip Rust-to-JS and
JS-to-Rust on lite-07. lite-06 still receives a timestamp. Run
`just drafts check` and `just test interop --all`.

Public API: none. Wire: lite-07-wip only, which is unpublished.

## Required

- [Rust untimed default](/quest/m1/rust-untimed-default.md) - touches the same Timescale defaults in `rs/moq-net` and lands first; this rebases onto it
