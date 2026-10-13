# [M] Same-epoch importers share one anchor and era

## Goal

Every importer publishing one path under one explicit `--epoch` places its
timestamps on one coordinate: the catalog root clock that the epoch's first
importer fixed, and the timestamp era that clock implies. A later importer
adopts that clock instead of placing its own, and shifts each timestamp
base by the whole number of the format's timestamp periods that lands it on
the incumbent's live timeline. Two
importers of one stream then publish identical timestamps and catalogs
whenever each started, including on opposite sides of a 33-bit TS wrap or of
a flagged forward clock break.

Today each importer places its clock at its own first frame's arrival, or
shifts onto a clock already in use by `clock.now()` minus its first
timestamp (#5082), and each TS importer unwraps from its own first PCR or
PTS. Two importers agree only if they started at the same packet.

## Plan

Decided 2026-10-13 by the maintainer, over one importer per epoch and over a
contract narrowed to what one input proves: a shared epoch carries no
source-to-wall mapping or era, and only one observation of the source can
supply them, so the epoch fixes them once and nothing re-derives them. The
maintainer accepted the follow-on rules below the same day: eras from the
incumbent's live timeline, joiners adopting history-derived catalog fields,
and the subscribe grant a joiner needs.

- The epoch's first importer places the clock at its first frame, as today.
  A later importer reads the epoch's live catalog before it announces and
  adopts its root clock exactly before publishing a frame, through the path
  that [catalog clock adoption](/quest/m1/catalog-clock-adopt.md) adds.
  `catalog::Timebase::place` will not do: it takes a `SystemTime`, which
  `Clock::wall_clock` floors to milliseconds, so the re-advertised clock
  would differ by up to 999 µs. It reads what its relay has now and never
  waits on the cluster for completeness; an incumbent it cannot see yet is
  the co-start case below.
- The era comes from the incumbent's published timeline, not from the
  clock's wall reading. A flagged forward break advances media time without
  wall time, so after two accepted +10 h breaks the clock reads about 10 h
  while the incumbent publishes 30 h, and a joiner choosing against the
  clock lands a full wrap off. Each independent timestamp base (in TS the
  PCR, and each stream's PTS and DTS, which unwrap separately) takes the
  whole number of periods (2^33 at 90 kHz for TS) that lands its first value
  nearest the incumbent's latest published timestamp. One offset taken from
  the first frame would misplace a stream whose first value sits across a
  wrap from it. Delivery skew alone is far below half a period, so it
  changes nothing. Add the periods in source ticks before converting to
  microseconds, since a period is not a whole microsecond count.
- The first importer has no incumbent, so it applies the same rule with its
  own first value as the reference. Today each stream's `PtsUnwrap` starts
  at era 0, so a first importer started at a wrap, with one stream's first
  PTS still just before it, places that stream 26.5 h from the others, and
  every joiner would adopt the broken coordinate. Timestamps are unsigned,
  so the pre-wrap stream's nearest candidate can be negative: resolve the
  initial bases together before placing the clock, and lift the whole
  coordinate by a period when one would land below zero. A base created
  later, such as a PID a PMT revision adds after a wrap (today
  `ensure_stream` starts it at `PtsUnwrap::default()`), resolves against
  the program's current verified timeline, on every importer alike.
- The joiner verifies its era against the incumbent's objects before
  announcing, one per independently mapped timestamp base (each track): the
  incumbent's object at the joiner's first group's time must match it in
  timestamp and payload. One object would let a matching video track
  approve an audio track a period off. A sparse base with no cadence
  (verbatim subtitles or teletext, each with its own `PtsUnwrap`) must not
  hold the announcement until its next cue: resolve it against its
  program's verified PCR base instead. A mismatch exits with an error, and
  so does an incumbent that leaves before the check. Decided
  2026-10-13 by the maintainer, over documenting a bound: the latest
  timestamp alone is wrong when flagged breaks totalling half a period fall
  within the delivery skew (a joiner one second behind, while the incumbent
  takes two +10 h breaks, lands a wrap ahead).
- The era choice alone cannot detect a different stream, since a nearest
  whole period always exists; the verification above refuses one at join.
  Past that, same stream stays the operator's precondition, unchecked like
  the rest of the same-epoch contract (maintainer, 2026-10-13).
- Every catalog field an importer derives from its history rather than the
  content at hand is the epoch's too. The joiner adopts the live value as of
  its first group, and later changes depend only on content on the epoch
  timeline. In TS passthrough that is the M2ts section's `random_access` and
  recorded `mux_rate`, owned by
  [raw TS cuts](/quest/m1/ts-passthrough-epoch-cuts.md).
- Two importers that both start before either is live would place two
  clocks under one epoch. One must exit, so that its supervisor's restart
  adopts; settle how. Detection by each importer through its own relay
  fails in a cluster: a replica on each relay reads before its peer's route
  arrives, and `route_order` keeps preferring each relay's own route
  afterwards, so neither sees the other. A co-start is also routine, since
  both importers of a pair restart together under a successor epoch.
  Candidates: an external leader or shared anchor; detection through state
  both can see, such as the announcement of the peer's route; or co-start
  as an unsupported precondition, which the successor-epoch flow would have
  to avoid.
- A flagged forward break must be carried as the same function of the
  source's clock values on every importer, so media time stays congruent to
  the raw clock modulo the period and the era choice above holds after it.
  The raw importer's clamp after a small flagged jump
  (`(last + step).max(self.written)` in `passthrough.rs`) breaks that and
  depends on history; remove or bound it.
- A rewind stays fatal (#4543), so both importers of a pair exit together
  and restart under a successor epoch. Settle how the successor gets its
  anchor without the co-start race above. The successor epoch is proposed to
  derive from the old epoch and the last frame's catalog time; the same
  inputs and the first source timestamp after the step can fix its anchor.
- Nothing outlives the live route, so a total outage loses the anchor: the
  next importer under the same explicit epoch is a first importer, and its
  era and clock can differ from the old run's. Decided 2026-10-13 by the
  maintainer: an explicit epoch names one continuous run, and a supervisor
  restarting after every importer stopped mints a successor epoch, as for a
  rewind; the `--epoch` docs say so. Rejected: persisting the anchor beyond
  the live route, and a flag marking the first importer.
- The mapping drifts from UTC at the source's clock error for the epoch's
  life, as one importer's does today. Nothing re-measures it.

Tests, on mocked time where possible: for each importer that takes
`--epoch`, two instances started at different source offsets publish
identical catalogs and timestamps; the same with the second started after a
33-bit wrap the first crossed, at a wrap with its streams' first values on
opposite sides of it, after flagged forward breaks that together exceed half
a period, with observations straddling such breaks (the verification
refuses a wrong era, on every track), and with different delivery skew into
each; a first importer started at a wrap with its streams on opposite sides
of it; a stream a PMT revision adds after a wrap; a different stream under
the same epoch fails verification; a cold
co-start, including one across two clustered relays, leaves one publisher.

Public API: no new items, but `--epoch` on an importer now reads the live
broadcast before announcing, so its token needs subscribe rights on its own
path. A publish-only token that works with `--epoch` today fails there: a
breaking auth requirement, documented with `--epoch` in `doc/bin/cli.md` and
`doc/bin/relay/auth.md`. Wire: none.

## Required

- [The catalog root clock is the producer clock](/quest/m1/catalog-clock-adopt.md) - the exact adoption path a joining importer uses

## Related

- [Same-epoch importers](/quest/m1/hop-aligned-import.md) - group sequences and the end-to-end pair on this coordinate
- [Broadcast epochs](/quest/m0/broadcast-epoch/README.md) - same-epoch routes resume each other's subscriptions
