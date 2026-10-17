# [XS] TS validation runs whenever a crate it builds changes

## Goal

`.github/workflows/ts.yml` triggers on every workspace crate the TS harness
builds, and a lint fails Check when its path filter drifts from that set.

## Plan

`test/ts/run.sh` builds `moq-relay` and `moq-cli` (plus `moq-shaper`). With
default features those pull in workspace crates the filter misses, found
2026-10-17: `moq-clock`, `moq-auth`, `moq-stats`, `moq-archive`, `moq-hls`,
`moq-rtc`, `moq-rtmp`, `moq-srt`, `moq-transcode`, `moq-video` and
`moq-nvenc`. crates.io dependencies such as `web-transport-*` are already
covered by the `Cargo.lock` filter.

Decided 2026-10-17 (maintainer): derive the set from `cargo metadata` in a
lint that runs in Check, rather than hand-listing paths that drift with the
next dependency.

Public API: none. Wire: none.
