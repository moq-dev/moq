# [XS] TS validation runs whenever a crate it builds changes

## Goal

`.github/workflows/ts.yml` triggers on every workspace crate the TS harness
builds, and a lint fails Check when its path filter drifts from that set.

## Plan

`test/ts/run.sh` builds `moq-relay`, `moq-cli` and `moq-shaper` with their
default features. That build compiles workspace crates the filter misses,
found 2026-10-17 with `cargo tree` over those roots: `moq-clock`,
`moq-auth`, `moq-stats`, `moq-archive`, `moq-hls`, `moq-rtc`, `moq-rtmp` and
`moq-srt`. Optional crates such as `moq-transcode`, `moq-video` and
`moq-nvenc` are not compiled and must not trigger the workflow.
crates.io dependencies such as `web-transport-*` are already covered by the
`Cargo.lock` filter.

Decided 2026-10-17 (maintainer): derive the set from Cargo in a lint that
runs in Check, rather than hand-listing paths that drift with the next
dependency. Resolve it for the harness's exact packages and features (the
activated closure), not the whole workspace graph, or unrelated changes will
trigger this expensive workflow.

Public API: none. Wire: none.
