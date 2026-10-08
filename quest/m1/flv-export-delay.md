# [S] FLV and MKV exports release on a fixed delay

## Goal

`moq export flv --delay <dur>` and `moq export mkv --delay <dur>` interleave
tracks through the shared jitter buffer, so their tag and block order is a
function of the media, not of arrival, as the TS export's is.

## Plan

Today each exporter's `pick_next_track`
(`rs/moq-mux/src/container/flv/export.rs`,
`rs/moq-mux/src/container/mkv/export.rs`) takes the smallest *pending*
timestamp, so which track goes first depends on which frame has arrived.
Replace both with the jitter buffer the TS export uses
(`rs/moq-mux/src/jitter.rs`), with the same `--delay` flag and the same
late-frame drop. Update `doc/bin/cli.md`.

Decided in the 2026-10-06 audit: `--delay` replaces the staleness flag, as it
replaced `--max-age` on TS, so one knob sets both the release delay and the
sources' staleness budget. [#4917](https://github.com/moq-dev/moq/pull/4917)
already renamed that flag to `--max-delay` (`with_max_delay` on both
exporters); this quest replaces `--max-delay` with `--delay` for flv and mkv.

Decided 2026-10-08: FLV and MKV land together in one quest, so the CLI and the
exporters' builder API break once.

Public API: breaking, each exporter's staleness setting becomes the delay
setting. Wire: none.
