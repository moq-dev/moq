# [M] fMP4 import applies edit lists to timestamps

## Goal

An fMP4 source with an edit list (AAC priming, a non-zero `media_time`, or an
empty edit) publishes frames at their presentation time, so both players and
every export see the timeline the source meant.

## Plan

Today `edts` is forwarded in the catalog init but read by no Rust or JS code,
and `rs/moq-mux/src/container/fmp4/export.rs` strips it on the way out. Both
players present `tfdt` plus the composition offset. ffmpeg's fragmented
output avoids `elst` by default, so this mostly affects GPAC, Bento4, and
Shaka sources (2026-10-07 audit).

Decided (2026-10-07): the importer applies the edit list to each frame's
timestamp and drops `edts` from the catalog init, so no consumer needs edit
list code. This builds on decoders taking time from the frame timestamp
rather than `tfdt`. Decide in the PR whether priming samples before the edit
are dropped or published with negative-offset handling, and refuse edit
lists with more than one non-empty entry rather than guess.

Public API: none. Wire: none, but a catalog behavior change: the init
loses `edts`, so a third-party consumer that applied `elst` itself must stop,
or it shifts twice. Note it in the changelog.

## Required

- [CMAF frame timestamp](/quest/m1/cmaf-frame-timestamp.md) - decoders time samples from the frame timestamp, which is where the edit lands

## Related

- [MP4 export](/quest/m2/mp4-export.md) - its open "edit lists" item is the output side
