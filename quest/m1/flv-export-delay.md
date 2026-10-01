# [S] FLV export releases frames on a fixed delay

## Goal

`moq export flv --delay <dur>` interleaves tracks through the shared
jitter buffer, so its tag order is a function of the media, not
of arrival, as the TS export's is.

## Plan

Today `pick_next_track` (`rs/moq-mux/src/container/flv/export.rs`) takes the
smallest *pending* timestamp, so which track goes first depends on which frame
has arrived. Replace it with the jitter buffer the TS export uses
(`rs/moq-mux/src/jitter.rs`), with the same `--delay` flag and the same
late-frame drop. Update `doc/bin/cli.md`.

Public API: the exporter gains a delay setting; breaking only if it replaces
an existing one. Wire: none.
