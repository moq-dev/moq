# [S] Raw TS groups and their catalog section follow the epoch timeline

## Goal

Two `moq import ts --passthrough` instances sharing one `--epoch` and fed
one stream cut identical groups wherever each started, including on a
source that never marks random access and on a multi-program stream.
Objects already run from PCR to PCR, so only group starts differ: the
fallback cut counts `FALLBACK` from the current group's start, and before
the first group from when the importer began waiting, so on a source with no
random access points each importer keeps its own group phase for good.
Their catalogs' M2ts sections match too, though both fields depend on
history today: one non-random-access cut clears `random_access` for good,
and the recorded `mux_rate` is judged over windows that start at the
importer's first PCR.

## Plan

Decided 2026-10-13 by the maintainer: every group start is a function of
the content and the [epoch's timeline](/quest/m2/epoch-anchor.md), never of
when the importer started.

- Fallback cuts fall on a grid of the epoch timeline, so an importer started
  later first cuts where its partner did. A grid on the raw 27 MHz count will
  not do: a 33-bit wrap is not a whole number of seconds, so such a grid
  shifts by about 0.72 s at each wrap.
- An importer's first group starts only where a partner running since
  before it would also cut: a random access point, a flagged forward break,
  or the grid.
- Settle the grid rule so that a source marking random access more often
  than `FALLBACK` is never cut away from its random access points, since any
  other start publishes `random_access` false.
- Size cuts count from the group's start, so they follow once group starts
  do.
- A cut is a position in the content, so under a shared epoch it follows
  media time alone. Decided 2026-10-13 by the maintainer as an explicit
  exception to timers advancing on max(wall, pts), because a wall-clock cut
  would split a stalled instance from its partner. Rejected: keeping a wall
  path.
- The M2ts section derives from the epoch and the content, never from the
  live catalog or what came before the importer started (maintainer,
  superseding #5412's adoption by a joiner). Today both fields are sticky
  history. Settle the rule; recommended: each is a function of a window on
  the epoch's grid (the rate measured over it, `random_access` true while its
  cuts all fell on random access points), and an importer publishes its
  catalog only once it has a whole window, so it never publishes a section
  its partner did not. Rejected in #5412 and still out: conservative
  constants under a shared epoch, which lose `random_access` and the rate
  `--sync` uses.

Tests: two instances started at different offsets into one capture publish
identical groups and objects from the later one's first group, and a
restarted instance rejoins identically. Cover a
source with no random access indicator, a multi-program stream, groups that
hit the size cap, the later instance started after a 33-bit wrap the
earlier crossed, and one started after the earlier made a fallback cut and
recorded a rate; their catalogs match.

Public API: none.

## Required

- [Epoch anchor](/quest/m2/epoch-anchor.md) - the timeline the grid sits on

## Related

- [Same-epoch importers](/quest/m2/hop-aligned-import.md) - group sequences and the end-to-end pair
- [TS passthrough export](/quest/m2/ts-passthrough-export.md) - identical objects export as identical packets
