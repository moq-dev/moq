# [M] moq-transport carries group starts and ends

## Goal

A moq-transport peer that understands the MOQ Properties sees each group's
predecessor link, each object's PTS, and each group's end, and moq-net reads
them back on ingest. A peer without them still plays live; only end
precision and gap detection degrade.

## Plan

Decided 2026-10-25 (maintainer), from the spike:

- The Group ID is the start ticks ([the model](/quest/m1/time-addressing/model.md)
  owns that mapping). The previous-start link rides in the Prior Group ID Gap
  property.
- Object IDs stay +1 per frame, so no Prior Object ID Gap is sent. Each
  object's PTS rides in TIMESTAMP (0x10), with TIMESCALE (0x08) on the track,
  the MOQ Properties shared with draft-ietf-moq-loc, so it stays LOC
  compatible.
- GROUP_END maps to END_OF_GROUP. The supported drafts refuse properties on
  a status object (`check_status` in `rs/moq-net/src/ietf/object.rs`), so
  END_OF_GROUP stays bare and the end needs a negotiated extension or is
  lost, as the extension rule below allows.
- Rejected: Object ID as a PTS delta, because B-frames run backwards in
  decode order and open-GOP leading frames present before the group's start.
- The properties are optional on both sides: moq-transport may carry them as
  properties or a negotiated extension, and a peer sending none still works
  for live playback.

Test both directions over drafts 18 and 22, with and without the properties.

Public API: none. Wire: moq-transport Prior Group ID Gap and TIMESTAMP are
sent and read, END_OF_GROUP marks a whole group, and the end time rides a
negotiated extension if one is defined.

## Required

- [Rust groups by start](/quest/m1/time-addressing/model.md) - the model whose links and ends these carry
