# [M] moq-mux imports a regular MP4

## Goal

moq-mux imports a regular, non-fragmented MP4, with `moov` before or after
`mdat`, as it imports fMP4.

## Plan

Decided 2026-11-05 (maintainer, on moq-dev/studio's /quest-plan): m3, since the studio is a named consumer that is not urgent.

`ContainerFormat` offers fMP4, MKV, TS, and FLV
(`rs/moq-mux/src/import/format.rs`), and fMP4 import refuses an `mdat` that
follows no `moof` (`Error::NoMoof` in
`rs/moq-mux/src/container/fmp4/import.rs`). `container/mp4.rs` handles regular
MP4 only for export and repair; reuse its parsing. A `moov`-last file needs
the whole file or a seekable source, so bound what it holds by design.

Consumer: the studio's media-file source ([moq-dev/studio#44](https://github.com/moq-dev/studio/pull/44)), which
refuses typical intro clips with a remux hint today. Fuzz the new path with
the existing media fuzz targets.
