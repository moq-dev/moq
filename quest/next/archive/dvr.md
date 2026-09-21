# [M] DVR rewind

## Goal

A viewer seeks through a bounded archive and returns to live playback using the
same timeline and group-range objects as an unbounded archive.

## Plan

The recording writer prerequisite owns retention, checkpoint recovery, deletion
grace, and restart cleanup. This quest consumes that contract and owns viewer
seek and return-to-live behavior, not a second writer implementation.

The player reads the archive timeline, FETCHes old groups through the normal
miss chain, and splices back to SUBSCRIBE at the live edge without opening a
second media format. Missing groups remain ordinary gaps.

An unbounded archive can continue the same segment numbering without rewriting
objects retained from an earlier DVR window.

Test seeks within the retained window, expiry during a seek, missing groups,
restart recovery, and return to live without duplicated or rewound playback.
Use the writer/reader fixtures; a retention defect is fixed in its owning layer.

## Required

- [Recording writer](/quest/next/archive/writer.md)
- [Recording reader](/quest/next/archive/reader.md)

## Closes

- [#2275](https://github.com/moq-dev/moq/issues/2275) - close this issue when the quest finishes
