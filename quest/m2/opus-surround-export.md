# [S] Opus export never writes a channel mapping it can't stand behind

## Goal

Exporting a catalog Opus rendition with 3 to 8 channels and no description
writes a `dOps` with the Vorbis family 1 mapping table, as the TS importer
assumes, instead of an invalid family 0 `dOps`. A family 3 OpusHead
(RFC 8486 projection with a demixing matrix) is refused by
`opus::Mapping::parse` with a typed error, or carried as `ambisonic`,
instead of being read as a channel table.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run), from [moq-dev/moq#7336](https://github.com/moq-dev/moq/pull/7336),
which finishes [surround Opus through CMAF](/quest/m2/cmaf-opus-surround.md),
and [moq-dev/moq#7308](https://github.com/moq-dev/moq/pull/7308).

- No description: on main the `None` arm in
  `rs/moq-mux/src/container/fmp4/mod.rs` (around line 1024) builds
  `opus::Config::new` and writes `channel_mapping: None` (family 0) for any
  channel count, which RFC 7845 allows only for 1 or 2 channels. Use
  `opus::Mapping::vorbis` (`rs/moq-mux/src/codec/opus/mapping.rs`) for 3
  to 8 channels and refuse more.
- Family 3: `Mapping::parse` (`mapping.rs:109`) reads a family 3 head as
  a plain table, so it imports as plain channels or fails as an invalid
  table. [moq-dev/moq#7336](https://github.com/moq-dev/moq/pull/7336) refuses family 3 in fMP4 only. Refuse it in `parse` itself
  (recommended, since no consumer can render a demixing matrix today), or
  carry it as `ambisonic` if [VR and ambisonic fMP4](/quest/m2/mux-vr-ambisonic-fmp4.md)
  wants it.

Cover both with unit tests beside [moq-dev/moq#7336](https://github.com/moq-dev/moq/pull/7336)'s
`opus_dops_mapping_the_head_cannot_describe_is_refused`.

Public API: none, or a new error variant. Wire: none.

## Required

- [Carry surround Opus through CMAF](/quest/m2/cmaf-opus-surround.md) - its PR, [moq-dev/moq#7336](https://github.com/moq-dev/moq/pull/7336), writes the family 1 table on export, which this reuses
