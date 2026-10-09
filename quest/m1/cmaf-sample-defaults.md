# [S] CMAF sample defaults and keyframes resolve one way everywhere

## Goal

Every CMAF reader resolves a sample's size, duration, and flags the same way,
and agrees on which samples are keyframes. A fragment that keeps its defaults
only in `trex` decodes in Rust, and the importer opens a new group on every
sync sample the JS decoder would also treat as one.

## Plan

Today there are three rules (2026-10-07 audit):

- `Wire::from_init` in `rs/moq-mux/src/container/fmp4/mod.rs` keeps only the
  `trak` and drops `mvex/trex`, so `decode` falls back from `trun` to `tfhd`
  only, and missing values become 0. A duration that only `trex` carries is
  `MissingSampleDuration`, and a size that only `trex` carries is an empty
  sample. The importer forwards `trex` in the catalog init and passes the
  fragments through, so its own output can hit this.
- Rust `decode` reads only the `trun` entry flags (`entry.flags.unwrap_or(0)`),
  ignoring `first_sample_flags`, the `tfhd` and `trex` default flags, and the
  non-sync bit, and needs `sample_depends_on == 2`. The importer's `is_sync_sample`
  (`import.rs`) needs `depends_on == 2` with non-sync clear. JS
  (`js/hang/src/container/cmaf/decode.ts`) already resolves size, duration,
  and flags through `trun` (including `firstSampleFlags`), `tfhd`, and `trex`,
  but treats flags 0 or a clear non-sync bit as a keyframe.

Decided (2026-10-07): one quest and one resolver. Resolve each of size,
duration, and flags as `trun` (with `first_sample_flags` for the first
sample), then `tfhd`, then `trex`, in one place shared by the importer and
`Wire::decode`, matching JS. Use one sync rule in all three; JS changes only
there. Add fixtures that Rust cannot decode today (defaults only in `trex`,
flags only in `tfhd` or `first_sample_flags`, a sync sample with `depends_on`
unset) and decode them in both languages.

Public API: none. Wire: none.

## Related

- [Export sync flags](/quest/m2/intra-refresh/export-sync-flags.md) - the export side of the same flags
