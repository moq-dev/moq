# [S] Fewer allocations in announce fan-out

## Goal

Announcing a broadcast to its subscribers costs about as many allocations per
subscriber with a deep path as with a shallow one. The Path spike's counting
allocator measured 16.5 allocations per subscriber at one depth and 26.7 at a deeper
one, the same with every `Path` representation, so the extra 10 come from fan-out
itself. Record the counts before and after in an allocation-count test.

## Plan

Decided with the maintainer on 2026-10-25, as a follow-up of the Path spike: find
where depth adds allocations per subscriber (per-segment joins, `strip_prefix`
copies, or per-subscriber presentation), and cut them. Allocation counts are the
signal; timing on a loaded host was within noise.

Public API: none.
