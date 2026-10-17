# [M] Same-epoch importers derive one coordinate from the epoch

## Goal

Every importer publishing one path under one explicit `--epoch`, fed one
stream, derives its catalog root clock and each timestamp's era from the
epoch and the source alone. They all stay live (active-active) and publish
identical timestamps and catalogs whenever each started, including on
opposite sides of a 33-bit TS wrap or a flagged forward break, and after a
restart under the same epoch. No importer reads another's broadcast, waits
on one, or exits because one exists.

Today each importer places its clock at its own first frame's arrival, or
shifts onto a clock already in use by `clock.now()` minus its first
timestamp (#5082), and each TS importer unwraps from its own first PCR or
PTS. Two importers agree only if they started at the same packet.

## Plan

Decided 2026-10-13 by the maintainer: importers sharing an explicit epoch
derive the same coordinate without coordination, and none is first. This
supersedes the same day's #5412 plan, where the epoch's first importer fixed
the clock and era and every later one read the live catalog and adopted
them: adoption needs a first importer, and a co-start (routine, since both
importers of a pair restart together) has none that either can name.
Rejected with it: a co-started importer that watches its peer's announcement
and exits, an external leader or anchor flag, and co-start as an unsupported
precondition.

- The epoch is a UUIDv7, so it carries a millisecond wall time that every
  importer shares exactly. Each importer reads only its own input, the epoch,
  and its own wall clock, and never the relay or the cluster.
- The era comes from the epoch (maintainer, superseding #5412's era from the
  incumbent's published timeline and the join-time verification against its
  objects): for each timestamp base (in TS the PCR, and each stream's PTS and
  DTS), the whole number of periods (2^33 at 90 kHz for TS) that lands the
  value nearest its arrival on the epoch's clock, as the importer's wall clock
  reads it. The source's offset from the wall clock is the same for every
  importer, so they agree unless their delivery and host-clock skew straddles
  a half-period boundary of that offset. Add the periods in
  source ticks before converting to microseconds, since a period is not a
  whole microsecond count.
- Recommended: derive every value's era this way, not only each base's first,
  so no importer state enters the coordinate. A restart, a base a PMT
  revision adds after a wrap, and a sparse base (verbatim subtitles or
  teletext) then need no special case, and `PtsUnwrap`'s history goes away.
  The cost is that media time stays within half a period of the epoch
  clock's reading. A flagged forward break that would carry it further reads
  as a step back, which stays fatal on every importer alike, as does a file
  read faster than real time for half a period. The raw importer's clamp after a small flagged jump
  (`(last + step).max(self.written)` in `passthrough.rs`) depends on history;
  remove it.
- Two known failure cases, documented rather than guarded (maintainer,
  2026-10-13, over refusing to start inside a guard band of the halfway
  point): importers that start together split onto adjacent eras when their
  skew straddles the halfway point, with odds of about skew over the period
  (near 1e-7 at 10 ms against TS's 26.5 h); and the gap between the source
  clock and wall time drifts by seconds a day, so over weeks it can drift
  onto the halfway point mid-run, where even a single importer reads a
  rewind and fails.
- The catalog's wall mapping derives from the epoch too (maintainer): media
  time maps to wall through the epoch's wall time plus the deterministic era,
  so every importer writes the same root `clock`. Rejected: no wall mapping
  under a shared epoch, and quantizing each importer's measured offset, whose
  rounding boundary splits importers far more often than a period's. The
  accuracy is bounded by how well the epoch's wall time matches the source:
  the mapping is off by the source clock's reading at the epoch's wall time,
  modulo the period, which for an arbitrary TS program clock is anywhere
  within half a period (13.25 h). Document that with `--epoch`. The exact rule
  is open; recommended: PTS zero sits one period before the epoch's wall time
  (`E - period`, deterministically rounded to a whole microsecond), and the
  era is chosen against that clock's reading. Then timestamps never land
  below zero for an importer whose clock reads after the epoch's wall time
  minus half a period, and a source whose clock reads zero at the epoch's
  wall time (a stream started when the epoch was minted) maps exactly and
  sits half a period from the boundary. A format whose timestamps never wrap
  has no era: PTS zero is the epoch's wall time and its timestamps stay
  verbatim. Install it through `Timebase::place`, which keeps whole
  microseconds; nothing reads a catalog back, so the millisecond flooring in
  `Clock::wall_clock` never enters it.
- Every catalog field an importer derives from its history rather than the
  content at hand derives from the epoch and the content instead (maintainer,
  superseding #5412's adoption from the live catalog). In TS passthrough that
  is the M2ts section's `random_access` and recorded `mux_rate`, owned by
  [raw TS cuts](/quest/m2/ts-passthrough-epoch-cuts.md); audit the other
  importers for more.
- A restart under the same epoch re-derives the same coordinate, including
  after a total outage, so the epoch keeps naming one timeline and resuming
  across the outage is a gap, not a stitch (maintainer, reversing #5412's
  "an explicit epoch names one continuous run; mint a successor after a total
  outage"). Nothing persists beyond the live route.
- A rewind stays fatal (#4543), so both importers of a pair exit and restart
  under a successor epoch. An importer never rolls to a successor in-process
  (maintainer: "Never"). Open: how a supervisor obtains the successor so both
  importers of a pair land on the same one without coordination. Proposed:
  derive it from the old epoch and the last frame's catalog time. Its wall
  time then fixes the successor's coordinate like any explicit epoch's.
- Same stream stays the operator's precondition, unchecked. Known gap, not
  planned now (maintainer): nothing refuses two routes with the same epoch
  but different content, and failover resumes mid-group across them
  (`rs/moq-net/src/model/resume.rs`), so a misfed pair splices two streams.
- The mapping drifts from UTC at the source's clock error for the epoch's
  life. Nothing re-measures it.

Tests, on mocked time where possible: for each importer that takes
`--epoch`, two instances started at different source offsets publish
identical catalogs and timestamps; the same with the second started after a
33-bit wrap the first crossed, at a wrap with its streams' first values on
opposite sides of it, after flagged forward breaks, and with different
delivery skew into each; a stream a PMT revision adds after a wrap; forward
breaks carrying media time past half a period from the epoch clock fail on
both alike; a co-start across two clustered relays leaves both live and
byte-identical; a restart under the same epoch, alone and after both
stopped, rejoins identically; the derived root `clock` matches the
recommended rule, including a source that reads zero at the epoch's wall
time.

Public API: no new items. Under a shared epoch the root `clock` and the
timestamps move from arrival to the epoch's derivation; the `--epoch` docs
state the accuracy bound. `--epoch` needs no subscribe right (#5412's
breaking grant for publish-only tokens is dropped). Wire: none.

## Related

- [Same-epoch importers](/quest/m2/hop-aligned-import.md) - group sequences and the end-to-end pair on this coordinate
- [Broadcast epochs](/quest/m0/broadcast-epoch/README.md) - same-epoch routes resume each other's subscriptions
