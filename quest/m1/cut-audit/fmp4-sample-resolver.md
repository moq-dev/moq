# [M] Counted field-less trun entries

## Goal

Parsing a moof allocates in proportion to its bytes: a `trun` whose samples
inherit every field from `tfhd`/`trex` decodes as a count, not one
materialized entry per sample. Land that counted representation in
kixelated/mp4-atom, release it, and adopt it in moq-mux so field-less runs
cannot amplify before the sample resolver sees the mdat.

## Plan

[moq-dev/moq#6864](https://github.com/moq-dev/moq/pull/6864) put fMP4
import, subtitle import, and CMAF decode on one lazy `sample::Resolver`. It
refuses empty samples, out-of-bounds or overlapping ranges, and absolute
`base_data_offset`, so output is bounded by the mdat. The parser is not:
mp4-atom 0.16.2 pushes one `TrunEntry` per sample even when the run carries
no per-sample field, capped only at 4096 per run. A 16 KB moof of 1000 such
runs materializes 4.1 million entries before the resolver refuses anything.

Settled (2026-10-26, maintainer): a counted representation, not a refusal.
[kixelated/mp4-atom#248](https://github.com/kixelated/mp4-atom/pull/248)
refuses nonempty field-less runs and breaks the AV1, BBB, and ESDS fixtures,
which are valid files; close or rework it rather than weaken those tests. Do
not add another sample-count cap or duplicate the atom parser in moq-mux.

- mp4-atom: replace `Trun.entries: Vec<TrunEntry>` with a shape that stores a
  field-less run (including a first-sample-flags-only run) as a count and
  yields entries through an iterator, without allocating from
  `sample_count`. Encode must round-trip both forms. The API shape is the
  maintainer's call; propose it in the mp4-atom PR. This is breaking, so
  aim for the 0.17.0 release that
  [VR fields](/quest/m2/mux-vr-fields.md) is waiting on.
- moq-mux re-exports `mp4_atom`, so the bump is a breaking change to its
  public API. Migrate `sample::Resolver::resolve`, the segment-start scan,
  and `kept`/`fragment` in `import.rs`, plus the tests that build `Trun`
  literals.
- Verify with the fMP4 tests and the interleaved container benchmark,
  including its 1-byte samples, and a test that a moof of many field-less
  runs decodes without a per-sample allocation.

Public API: breaking `mp4_atom::Trun` through moq-mux's re-export. MoQ wire:
none.
