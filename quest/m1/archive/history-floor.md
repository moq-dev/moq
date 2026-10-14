# [M] History mode starts at a DVR recording's retained floor

## Goal

An HLS export in history mode lists a recording that `moq_archive::Writer`
retention has pruned, starting at its oldest retained segment, and the
Reader's timeline index stays within the retained window. Today
`moq_hls::export::Config::history` refuses it with `Error::HistoryPruned`
before listing anything, because the exporter cannot tell a pruned start from
missing history.

## Plan

Decided 2026-10-11 (#5236): refuse retained recordings now, and quest the
floor. A gap after the floor must still fail, never list as a complete
recording.

Prefer a subscribe range: once moq-lite-07 ranges land
([Subscribe ranges](/quest/m1/subscribe-ranges/README.md)), an `Absolute(0)..`
range answers from the oldest group that still exists and names the pruned
ones with SUBSCRIBE_DROP, so history starts at the first delivered group.
Otherwise the Reader answers a request below its retained floor with an error
distinct from a missing group.

The same floor lets `moq_archive::Reader` prune its timeline-group map
(`Shared::timelines`), which today grows with a long retained recording's
whole lifetime: its refresh lists only objects after its cursor, so it never
sees the writer's deletions. Drop mappings below the floor rather than
bounding the map by age or size, which could discard retained history
(#5236 review).

Surface retention in the catalog's archive section only if the exporter still
needs to know up front; that changes the hang format (`js/hang`,
`drafts/draft-lcurley-moq-hang.md`).

Test a retained recording whose first timeline objects are deleted: history
lists from the first retained segment, and a hole after it still fails.

## Related

- [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) - a range from group 0 answers from the oldest group that still exists
- [DVR rewind](/quest/m2/archive-dvr.md) - seeks through the same bounded archive
