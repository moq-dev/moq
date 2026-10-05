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

Decided: every publisher of one path MUST publish the same broadcasts and
tracks, since any route covering a path resumes its subscriptions. A redundant
pair is keyed by a shared explicit epoch (decided 2026-10-03: `--hop` no
longer exists to key it). Make every container importer (ts, fmp4, flv, mkv,
and the SRT, RTMP, and HLS gateways that reuse them) meet the contract when
fed one encoded stream. Capture is out: two encoders never align.

- A group's sequence derives from its keyframe's media timestamp (PTS in TS),
  not a per-process counter. Decide how both processes agree across a
  timestamp wrap (TS PTS wraps every 26.5 h) when they started on opposite
  sides of it.
- Frame timestamps derive from the input alone too. Importers publish the
  stream's own timestamps and refuse a rewind, so nothing shifts them.
- An importer announces only once it knows its tracks, so it never refuses a
  track the incumbent serves.
- Docs (`doc/bin/cli.md` "Redundant publishers", the `--epoch` doc comment)
  narrow the contract to one encoded stream fed to each publisher.

Tests: per importer, two instances started at different offsets into the same
input produce the same group sequences and timestamps. End to end: the
issues' 1+1 setup (one relay, two `import ts` sharing one `--epoch`, two
`export ts`) survives the standby joining and the incumbent stopping.

## Required

- [Remove `--hop`](/quest/m0/broadcast-epoch/hop-removal.md) - the shared `--epoch` this keys a redundant pair on

## Closes

- [#4352](https://github.com/moq-dev/moq/issues/4352) - close this issue when the quest finishes
- [#4354](https://github.com/moq-dev/moq/issues/4354) - close this issue when the quest finishes

## Related

- [Broadcast epochs](/quest/m0/broadcast-epoch/README.md) - a redundant pair shares one epoch
