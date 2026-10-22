# [M] Validate CMAF header fields before use

## Goal

An untrusted CMAF fragment cannot allocate or loop in proportion to an
unvalidated sample count, and an init segment with an impossible timescale is
refused instead of producing non-finite timestamps. Browser playback rejects
impossible input before disproportionate work while retaining valid defaulted
sample fields.

## Plan

P1, inherited from release and still present in main's changed decoder.
At main 6369eca, `js/hang/src/cmaf/decode.ts` delegates `trun` parsing to
`@svta/cml-iso-bmff` 1.0.7. Its `readTrun` calls `readEntries(sampleCount)`
even with flags zero, allocating an object per entry without consuming input.
The caller checks sample sizes against `mdat` only afterwards.

Reproduction: construct a 40-byte `moof/traf/trun` followed by empty `mdat`,
with `trun.flags=0`. Changing only the u32 count from 1,000 to 100,000 causes
that many objects to be allocated before `Sample 0 would overflow mdat`.
The larger case took about 7.4ms on the audit host; no dangerous billion-entry
allocation was attempted. The u32 permits much larger counts, making this a
publisher-controlled allocation/CPU concern even for tiny network input.

Validate the count against the media representation before allocation or
retain defaulted samples lazily. Inspect sibling count-based ISO readers at
the same parser boundary. Prefer a maintained parser fix where appropriate;
posting outside moq-dev/kixelated still needs authorization. Avoid an arbitrary
maximum count as a replacement for input-proportional work. Cover truncated
tables, defaulted fields, and impossible payload extents; benchmark fragment
bytes against declared sample count. No API or wire change is expected.
Added 2026-10-22 (maintainer) from the media audit at
[main 1f5caa7](https://github.com/moq-dev/moq/commit/1f5caa7026bcfd8a2684da3712b384b5f74a7496),
the same root cause in the same file: `decodeInitSegment` checks that `mdhd`
exists but returns its `timescale` unchecked, and the fragment path divides by
it. A checked-in fixture with only the timescale set to zero initialized and
decoded with Infinity timestamps and durations; another case gave NaN. Refuse
a zero timescale at initialization, and validate the other header fields the
decoder divides or indexes by at the same boundary.

The current consumer is watch CMAF ingestion. Land on main first, then
propose a release-compatible backport for this inherited resource bug.
