# [S] The Annex B import formats are named h264 and h265

## Goal

The import format for a raw Annex B elementary stream is `h264` / `h265`
(`hevc` accepted) everywhere it is named: `moq_mux::import::VideoFormat`,
the CLI's `--format`, moq-ffi, moq-c, and the Go wrapper. `avc1` / `hvc1`
stay as the length-prefixed formats with a config record, and `avc3` /
`hev1` are no longer import format names.

## Plan

Decided 2026-11-17 (maintainer, `/quest-import` of
[moq-dev/moq#9011](https://github.com/moq-dev/moq/issues/9011)):
✅ rename (rejected: keep the names and fix only the doc comments).
`avc3`/`hev1` say where parameter sets live, not how NAL units are framed:
inside a frame, NAL units are still split by start codes or length
prefixes. Naming the Annex B input after them taught the conflation that
became the native decoder bug fixed in
[moq-dev/moq#9066](https://github.com/moq-dev/moq/pull/9066). The names follow ffmpeg's raw
`h264` / `hevc` formats; `h264` already aliases `avc3`.

Where the names live: `rs/moq-mux/src/import/format.rs` (the enum, its doc
comment, and `FromStr`), `rs/moq-ffi/src/media.rs`, `rs/moq-c/src/api.rs`,
`go/wrapper/media/media.go`, `rs/moq-cli/src/args.rs`, and
`doc/bin/cli.md`. Mirror the same names in every binding, and put the
migration (`avc3` → `h264`, `hev1` → `h265`) in the PR's Impact section.
No alias for the old names.

The catalog's codec strings are not import formats and keep `avc3`/`hev1`.

Public API: breaking, as above. Wire: none.
