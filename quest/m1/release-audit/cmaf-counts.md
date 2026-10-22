# [M] Validate CMAF sample counts before materializing entries

## Goal

An untrusted CMAF fragment cannot allocate or loop in proportion to an
unvalidated sample count. Browser playback rejects impossible fragments
before disproportionate work while retaining valid defaulted sample fields.

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
The current consumer is watch CMAF ingestion. Land on main first, then
propose a release-compatible backport for this inherited resource bug.
