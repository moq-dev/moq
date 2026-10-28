# [S] An HLS viewer starts capture

## Goal

`moq publish` starts capturing a rendition when its only subscriber is an HLS
export, so an HLS-only viewer gets media.

## Plan

Decided 2026-10-28 (maintainer), from
[moq-dev/moq#6937](https://github.com/moq-dev/moq/pull/6937), which made HLS
records on by default: a subscriber to a rendition's record track counts as
demand for that rendition's media.

- `moq publish` encodes on demand from media-track demand
  (`rs/moq-cli/src/publish.rs`), but the HLS export reads a rendition's
  records before subscribing to its media, so the record subscriber alone
  never starts capture. This was already true with `--hls` before #6937.
- Make record-track demand reach the media's demand where records are
  produced (`rs/moq-mux/src/hls.rs`), so every on-demand publisher benefits,
  not only the CLI.
- Test an HLS-only viewer against `moq publish`: capture starts and segments
  appear.

Public API: none expected. Wire: none.
