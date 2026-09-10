# [M] DVR rewind

## Goal

A viewer seeks through a bounded archive and returns to live playback using the
same timeline and group-range objects as an unbounded archive.

## Plan

The writer owns retention. During the next segment commit, it pops expired
records from the archive Window (`Producer::pop`,
`rs/moq-mux/src/timeline.rs:983`), closes and stores the timeline groups under
that segment's key, waits the configured grace period, then deletes each
expired track segment. Retention stops
after the final segment is committed. Keep a durable checkpoint covering the
retained window and the latest timeline object, including all-gap segments.
The timeline never advertises an object after deletion, and no object is
rewritten.

On writer restart, hold exclusive prefix ownership, fully recover the retained
Window, and list all track `groups/` keys before accepting new groups. After
a fresh deletion grace period measured from recovery, remove keys absent from the retained records; this
finishes expiration interrupted after its timeline pop and removes uncommitted
uploads. Never delete on incomplete or failed recovery/listing. Preserve `.info`
and checkpoint-supporting timeline objects. This resumes prior cleanup without
popping further records after the final segment.

The player reads the archive timeline, FETCHes old groups through the normal
miss chain, and splices back to SUBSCRIBE at the live edge without opening a
second media format. Missing groups remain ordinary gaps.

An unbounded archive can continue the same segment numbering without rewriting
objects retained from an earlier DVR window.

## Required

- [Recording writer](/quest/m1/archive/writer.md)
- [Recording reader](/quest/m1/archive/reader.md)

## Closes

- [#2275](https://github.com/moq-dev/moq/issues/2275) - close this issue when the quest finishes
