# [XS] Finish the milestone re-cut

## Goal

The epics that open claims held in place during the 2026-11-05 milestone
re-cut sit where the re-cut's rules put them. What remains of api-audit,
datagram-tracks, drop-set, rind,
session-policy, and stream-credit moves from m1 to m2; what remains of release-perf moves under
the [cut gate](/quest/m2/cut-gate/README.md); and the
probe epic moves from m2 to m3. The "held" notes in the m1, m2, and cut gate
READMEs are removed, along with each held entry's extra link from m2's or
the gate's `Required`.

## Plan

`quest mv` refuses to move a claimed quest, since its branch is its path, and
a child must stay listed by its own epic, so each epic waited for its claimed
children. Move each epic with `quest mv` once nothing in it is claimed; an
epic whose claim finished all its work is already gone. Before moving,
re-check what remains against the root [questline](/quest/README.md)'s rules:
an epic with no breaking work left stays in m1, and one with no gating work
left leaves the cut gate for m1. If a child is claimed again before this runs,
leave its epic in place and keep this quest for it.
Drop-set is a single quest, not an epic, so it has no `Required` entry here:
when this runs, move it if it still exists and nothing claims it.

Prose that names a held epic's milestone becomes stale when it moves; update
it in the same PR: datagram tracks in [net-wrap](/quest/m5/net-wrap/README.md), and the browser
padding probes in [lite07-finalize](/quest/m2/lite07-finalize.md).

## Required

- [Datagram answer race](/quest/m1/datagram-tracks/answer-race.md) - claim held datagram-tracks
- [IETF datagram tracks](/quest/m1/datagram-tracks/ietf.md) - claim held datagram-tracks
- [rind CPU error](/quest/m1/rind/cpu-error.md) - claim held rind
- [Connect policy](/quest/m1/session-policy/connect.md) - claim held session-policy
- [Release on leave](/quest/m1/stream-credit/release-on-leave.md) - claim held stream-credit
- [Probe padding](/quest/m2/probe/padding.md) - claim held probe
