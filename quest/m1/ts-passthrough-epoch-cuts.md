# [S] Raw TS groups cut on the epoch timeline

## Goal

Two `moq import ts --passthrough` instances sharing one `--epoch` and fed
one stream cut identical groups wherever each started, including on a
source that never marks random access and on a multi-program stream.
Objects already run from PCR to PCR, so only group starts differ: the
fallback cut counts `FALLBACK` from the current group's start, and before
the first group from when the importer began waiting, so on a source with no
random access points each importer keeps its own group phase for good.

## Plan

Decided 2026-10-13 with the redundant TS alignment plan: every group start
is a function of the content and the
[epoch's timeline](/quest/m1/epoch-anchor.md), never of when the importer
started.

- Fallback cuts fall on a grid of the epoch timeline, so a joiner's first
  group starts where the incumbent cut. A grid on the raw 27 MHz count will
  not do: a 33-bit wrap is not a whole number of seconds, so such a grid
  shifts by about 0.72 s at each wrap.
- An importer's first group starts only where the incumbent's rules also
  cut: a random access point, a flagged forward break, or the grid.
- Settle the grid rule so that a source marking random access more often
  than `FALLBACK` is never cut away from its random access points, since any
  other start publishes `random_access` false.
- Size cuts count from the group's start, so they follow once group starts
  do.

Tests: two instances started at different offsets into one capture publish
identical groups and objects from the later one's first group. Cover a
source with no random access indicator, a multi-program stream, groups that
hit the size cap, and the later instance started after a 33-bit wrap the
earlier crossed.

Public API: none.

## Required

- [Epoch anchor](/quest/m1/epoch-anchor.md) - the timeline the grid sits on

## Related

- [Same-epoch importers](/quest/m1/hop-aligned-import.md) - group sequences and the end-to-end pair
- [TS passthrough export](/quest/m1/ts-passthrough-export.md) - identical objects export as identical packets
