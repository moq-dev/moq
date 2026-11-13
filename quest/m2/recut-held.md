# [XS] Finish the milestone re-cut

## Goal

The epics that open claims held in place during the 2026-11-05 milestone
re-cut sit where the re-cut's rules put them: the probe epic moves from m2 to
m3. The "held" notes in the m2 and cut gate READMEs are removed, along with
each held entry's extra link from the gate's `Required`. Stream-credit stays
in m1 (maintainer, 2026-11-13): with takeover moved to m3 it breaks no wire,
and m2 no longer requires it.

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
it in the same PR. The browser padding probes already moved to m3 on 2026-11-06.

## Required

- [Browser send estimate](/quest/m2/probe/browser-send-estimate.md) - claim held probe
