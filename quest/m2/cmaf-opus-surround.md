# [M] Carry surround Opus through CMAF

## Goal

An fMP4 Opus track whose `dOps` declares channel mapping family 1 imports with
its mapping table in the OpusHead description, and a family 1 description
exports to a `dOps` that keeps the table. Today both directions refuse it.

This needs a released `mp4-atom` whose `Dops` decodes and encodes any channel
mapping family as `channel_mapping: Option<OpusChannelMapping>` (`None` is
family 0), the shape drafted in kixelated/mp4-atom#246. That is a breaking
`mp4-atom` 0.17.

## Plan

Decided 2026-10-17: the `mp4-atom` change is not worth landing on its own.
Alone it only swaps `mp4-atom`'s generic decode error for
`UnsupportedMappingFamily`; the payoff is surround Opus. So the former
`mp4-atom-dops-mapping` quest (draft moq-dev/moq#5819) folded in here.

Decided 2026-10-28 (maintainer): kixelated/mp4-atom#246 ships in 0.17.0
beside kixelated/mp4-atom#254, so downstream takes one breaking bump.
0.17.0 is released (2026-10-30), and moq-mux takes it for the counted
trun (moq-dev/moq#7043, carried by moq-dev/moq#7308), still refusing a
nonzero family explicitly, since `mp4-atom` no longer does.
This quest then only does the import and export.
A git patch to the branch cannot stand in for the release, since `cargo deny`
refuses unknown git sources.

Settled in #246 and #5819:

- `Dops` decode and encode both check the table against the output channel
  count and the RFC 7845 §5.1.1 stream rules, failing with
  `Error::InvalidCombination`. Family 0 encodes byte-identically.
- `family` stays a raw `u8`; family-specific limits (family 1 allows at most 8
  channels) are the consumer's, which `opus::Mapping` already enforces.
- `mp4-atom` does not check that family 0 declares one or two channels; moq's
  Opus config checks its own head, so that is left alone.

Open for the implementing PR (maintainer's call, in #246):

1. `Dops` shape. Recommended: `Option<OpusChannelMapping>` beside
   `output_channel_count`. Alternatives: a flat `channel_mapping_family: u8`
   plus an optional table, which mirrors the spec but allows family 0 with a
   table; or an enum replacing `output_channel_count`, which drops the
   redundant count but breaks every caller harder.
2. Table field name. Recommended: `channel_mapping`, mirroring the spec.
   Alternative: `table`, which reads better at call sites.

In moq-mux, build the head from the `dOps` family and table with
`opus::Mapping` on import, where the importer today relies on `mp4-atom`
refusing a nonzero family. On export, write the table from the parsed head in
place of the `UnsupportedMappingFamily` refusal in `synthesize_audio_trak`.
Keep refusing families the head cannot describe.

Regression: a family 1 5.1 `dOps` built with `mp4-atom` round-trips through
import and export with its table intact.
