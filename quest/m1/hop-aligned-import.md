# [L] Same-epoch importers publish identical tracks

## Goal

Two importers publishing one path, such as a redundant pair sharing an
explicit `--epoch`, and fed the same encoded stream publish identical tracks,
so a relay can fail over between them. Today two `moq import ts` fed one
forked stream break that contract twice: a standby refuses every track until
it has parsed its PMT, which aborts every subscriber the moment it connects
(#4352), and each numbers its groups from its own counter, so after a
failover `export ts` sees a timestamp rewind and exits (#4354).

## Plan

Decided: every publisher of one path and epoch MUST publish the same
broadcasts and tracks, since routes with the same epoch resume each other's
subscriptions. A redundant pair is keyed by a shared explicit epoch (decided
2026-10-03: `--hop` no longer exists to key it). Make every container importer
that `moq import --epoch` accepts (ts, fmp4, flv, mkv) meet the contract when
fed one encoded stream. The SRT, RTMP, and RTC gateways and `ts --program all`
are out: they mint an epoch per ingest connection (#4962), so two of them
never share one. Capture is out too: two encoders never align.

- A group's sequence derives from its keyframe's media timestamp (PTS in TS),
  not a per-process counter. Decide how both processes agree across a
  timestamp wrap (TS PTS wraps every 26.5 h) when they started on opposite
  sides of it.
- Frame timestamps derive from the input alone too, and importers refuse a
  rewind. Any offset [Shared import clock](/quest/m1/shared-clock.md)
  applies is input-derived for a same-epoch importer, never from
  `clock.now()` (decided in the 2026-10-06 audit), so two instances shift
  identically.
- The catalog's root `clock` must agree too. Today each importer anchors it
  to its own first-frame arrival (`Clock::arrival` in
  `rs/moq-mux/src/catalog/producer.rs`), which is also the first published
  clock, since each importer holds its catalog until that frame, and [Shared import clock](/quest/m1/shared-clock.md)
  offsets a joining importer by its arrival time. Decided in the 2026-10-05
  audit: redundant importers derive the wall anchor from the input (its PTS
  or PCR) or from the shared epoch, never from arrival, so two catalogs of
  one stream are identical. They pass it through Shared import clock's
  `Input`/offset API rather than a second anchoring path (2026-10-06 audit). Rejected: narrowing the contract to exclude the
  catalog clock.
- An importer announces only once it knows its tracks, so it never refuses a
  track the incumbent serves.
- Docs (`doc/bin/cli.md` "Redundant publishers", the `--epoch` doc comment)
  narrow the contract to one encoded stream fed to each publisher.

Tests: per importer, two instances started at different offsets into the same
input produce the same group sequences and timestamps. End to end: the
issues' 1+1 setup (one relay, two `import ts` sharing one `--epoch`, two
`export ts`) survives the standby joining and the incumbent stopping.

## Required

- [Shared import clock](/quest/m1/shared-clock.md) - the `Input`/offset API this supplies an input-derived anchor through

## Closes

- [#4352](https://github.com/moq-dev/moq/issues/4352) - close this issue when the quest finishes
- [#4354](https://github.com/moq-dev/moq/issues/4354) - close this issue when the quest finishes

## Related

- [Broadcast epochs](/quest/m0/broadcast-epoch/README.md) - a redundant pair shares one epoch
- [E2EE](/quest/m1/e2ee/README.md) - an encrypting publisher refuses a shared epoch, so a redundant pair is plaintext only
