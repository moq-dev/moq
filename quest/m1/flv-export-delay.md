# [S] FLV export releases frames on a fixed delay

## Goal

`moq export flv --delay <dur>` interleaves tracks through the shared
fixed-delay release stage, so its tag order is a function of the media, not
of arrival, as the TS export's is.

## Plan

Today `pick_next_track` (`rs/moq-mux/src/container/flv/export.rs`) takes the
smallest *pending* timestamp, so which track goes first depends on which frame
has arrived. Replace it with the release stage from
[fixed-delay release](/quest/m1/tstd/delay.md), with the same `--delay` flag
and the same late-frame drop. Update `doc/bin/cli.md`.

Decided in the 2026-10-06 audit: `--delay` replaces the staleness flag, as it
replaced `--max-age` on TS, so one knob sets both the release delay and the
sources' staleness budget. [Subscriber max-delay](/quest/m1/subscriber-max-delay.md)
(#4917) renames that flag to `--max-delay` first; this quest then replaces
`--max-delay` with `--delay`. Rejected: excluding flv from the #4917 rename.

Public API: breaking, the exporter's staleness setting becomes the delay
setting. Wire: none.

## Required

- [Subscriber max-delay](/quest/m1/subscriber-max-delay.md) - renames the staleness flag first, so this replaces `--max-delay`
- [Fixed-delay release](/quest/m1/tstd/delay.md) - builds the shared release stage
