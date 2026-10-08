# [S] Rank audio renditions in JS and HLS

## Goal

`@moq/hang` orders audio renditions the way Rust `hang::catalog::Audio::ranked`
(#4993) does: highest bitrate, then sample rate, then channels, unknown
bitrate last, ties in name order. The HLS exporter orders audio renditions by
that rank instead of by name.

## Plan

Mirror `Catalog.ranked` for video (#4988). Name it to match Rust per AGENTS.md.
