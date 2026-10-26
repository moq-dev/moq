# [M] One fMP4 sample resolver for import and decode

## Goal

The fMP4 importer and the CMAF subscriber decode resolve samples through
one resolver that enforces non-empty, in-bounds, disjoint, and correctly
based samples. A moof of field-less truns can no longer cost gigabytes and
hundreds of thousands of groups, and ffmpeg output that uses
`base_data_offset` (the common `frag_keyframe+empty_moov` pipe recipe)
imports or is refused with a typed error instead of failing on a bogus
range.

## Plan

Found by the 2026-10-24 pre-cut audit at 151a529670.

- **Zero-size samples** (reproduced; per-sample fan-out new on main from
  [moq-dev/moq#6250](https://github.com/moq-dev/moq/pull/6250)): a moof of
  1000 field-less truns of 4096 entries with tfhd default size 0 is 4.1M
  samples in 16 KB, since mp4-atom 0.16.2 materializes 4096 entries for a
  trun without per-entry fields. Through import it took 6.3 GB RSS, 66-81 s
  of CPU, and about 720k groups. Paths:
  `rs/moq-mux/src/container/fmp4/import.rs:1052` (`with_capacity(total_samples)`),
  `:1095-1128` (a 0-byte sample passes), `fmp4/sample.rs:87`
  (`Claimed::take` returns early on empty), `import.rs:1380` (each sync
  sample opens a group), and the subscriber-side `Wire::decode`
  (`fmp4/mod.rs:533`), reached by any Rust subscriber of a cmaf track.
- **`base_data_offset`** (reproduced with ffmpeg 9.0.2, inherited):
  `import.rs:1049-1069` and `fmp4/text.rs:72` treat the absolute file offset
  as an mdat-relative one, failing the first fragment with
  `SampleRangeOutOfBounds`. Resolve it against the moof's absolute position
  with a per-source byte counter, or refuse it with a typed error as
  `decode()` does at `mod.rs:523`.

Fix: refuse zero-size media samples in the shared resolver, so each sample
costs at least one input byte, and make field-less truns bounded by design
in mp4-atom (refuse, or store a count) instead of its 4096 constant. The two
sample loops have drifted through repeated fixes
([moq-dev/moq#5078](https://github.com/moq-dev/moq/pull/5078),
[moq-dev/moq#6379](https://github.com/moq-dev/moq/pull/6379),
[moq-dev/moq#6686](https://github.com/moq-dev/moq/pull/6686)); one resolver
is a net deletion. If the mp4-atom change needs a release, land the
resolver here and track the bump separately.

Public API: possibly a new `fmp4::Error` variant. Wire: none.
