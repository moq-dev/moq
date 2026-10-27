# [M] Sparse reads judge misses incrementally

## Goal

A reader of a sparse track pays O(log gaps) per group handed out, as it did
before [dynamic range requests](https://github.com/moq-dev/moq/pull/6171).
`track_sparse_read` and the setup of `track_gap_flush` stay flat across their
gap sweeps.

## Plan

Found by the 2026-10-25 main against release run. Every hand-out goes through
`Cursor::probe` (`model/track.rs`), and `miss::Misses::judge`
(`model/track/miss.rs`) rebuilds its clipped span from the whole missable
window and re-judges every deferred gap, so each read walks every gap below
it. Instructions per write and read pair of `track_sparse_read/timed`: 152k
at 16 gaps, 2.57M at 256, and 51.4M at 4096, which is 1079x release in wall
time (2.75 ms against 2.5 us). The untimed rows match. `track_gap_flush`
builds its gaps through the same path, so its untimed setup is quadratic
(50.8G instructions at 4096 gaps).

Judge only what changed since the last probe: the groups the read passed,
gaps the ledger resolved, and a window or budget change. A probe with
nothing new must not touch the deferred set. Keep the fills and drops
that range requests serve; the sweep, not a release ratio, is the acceptance signal.
[Drop set](/quest/m1/drop-set.md) reshapes the same state, so whichever lands
second rebases onto the other.

Public API: none. Wire: none.

## Related

- [Drop set](/quest/m1/drop-set.md) - per-cursor miss judgement over the same ledger
