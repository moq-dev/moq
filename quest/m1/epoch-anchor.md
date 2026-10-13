# [M] Same-epoch importers share one anchor and era

## Goal

Every importer publishing one path under one explicit `--epoch` places its
timestamps on one coordinate: the catalog root clock that the epoch's first
importer fixed, and the timestamp era that clock implies. A later importer
adopts that clock instead of placing its own, and shifts its timestamps by a
whole number of the format's timestamp periods chosen against it. Two
importers of one stream then publish identical timestamps and catalogs
whenever each started, including on opposite sides of a 33-bit TS wrap or of
a flagged forward clock break.

Today each importer places its clock at its own first frame's arrival, or
shifts onto a clock already in use by `clock.now()` minus its first
timestamp (#5082), and each TS importer unwraps from its own first PCR or
PTS. Two importers agree only if they started at the same packet.

## Plan

Decided 2026-10-13 with the redundant TS alignment plan, over one importer
per epoch and over a contract narrowed to what one input proves: a shared
epoch carries no source-to-wall mapping or era, and only one observation of
the source can supply them, so the epoch fixes them once and nothing
re-derives them.

- The epoch's first importer places the clock at its first frame, as today.
  A later importer reads the epoch's live catalog before it announces and
  adopts its root clock before publishing a frame, through
  `catalog::Timebase::place` or the adoption path that
  [catalog clock adoption](/quest/m1/catalog-clock-adopt.md) adds. Its token
  needs subscribe rights on its own path for that.
- The later importer's offset is the whole number of periods (2^33 at
  90 kHz for TS) that lands its first frame nearest the adopted clock's
  reading. Wall time only selects among candidates a period apart, so any
  error short of half a period changes nothing, and it never sets the media
  clock. No candidate within that bound means a different stream: refuse to
  announce.
- Two importers that both start before either is live would place two
  clocks under one epoch. The later must detect it and exit, so that its
  supervisor's restart adopts; settle how it detects it.
- A flagged forward break must be carried as the same function of the
  source's clock values on every importer, so a standby started after it
  lands on the incumbent's timeline. The raw importer's clamp after a small
  flagged jump (`(last + step).max(self.written)` in `passthrough.rs`)
  depends on history; remove or bound it.
- A rewind stays fatal (#4543), so both importers of a pair exit together
  and restart under a successor epoch. Settle how the successor gets its
  anchor without the co-start race above. The successor epoch is proposed to
  derive from the old epoch and the last frame's catalog time; the same
  inputs and the first source timestamp after the step can fix its anchor.
- The mapping drifts from UTC at the source's clock error for the epoch's
  life, as one importer's does today. Nothing re-measures it.

Tests, on mocked time where possible: for each importer that takes
`--epoch`, two instances started at different source offsets publish
identical catalogs and timestamps; the same with the second started after a
33-bit wrap the first crossed, after a flagged forward break, and with
different delivery skew into each; a cold co-start leaves one publisher; a
different stream under the same epoch is refused.

Public API: none new. `--epoch` on an importer reads the live broadcast
before announcing. Wire: none.

## Related

- [The catalog root clock is the producer clock](/quest/m1/catalog-clock-adopt.md) - the adoption path a joining importer can use
- [Same-epoch importers](/quest/m1/hop-aligned-import.md) - group sequences and the end-to-end pair on this coordinate
- [Broadcast epochs](/quest/m0/broadcast-epoch/README.md) - same-epoch routes resume each other's subscriptions
