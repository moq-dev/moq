# [M] DVR rewind

## Goal

A viewer seeks through a bounded archive and returns to live playback using the
same timeline and `(track, segment)` objects as an unbounded archive.

## Plan

The writer owns retention. It pops expired records from the archive Window,
persists the new timeline group, waits a short grace period, then deletes each
expired track segment. The timeline never advertises an object after deletion,
and no manifest or head must be rewritten.

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
