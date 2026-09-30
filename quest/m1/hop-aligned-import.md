# [L] Same-hop importers publish identical tracks

## Goal

Two importers sharing a `--hop` and fed the same encoded stream publish
identical tracks, so a relay can fail over between them. Today two
`moq import ts` fed one forked stream break that contract twice: a standby
refuses every track until it has parsed its PMT, which aborts every
subscriber the moment it connects (#4352), and each numbers its groups from
its own counter, so after a failover `export ts` sees a timestamp rewind and
exits (#4354).

## Plan

Decided: same-hop publishers MUST publish the same broadcasts and tracks.
Keep `--hop` for now: the users' bugs are on one relay today. Cluster
routing replaces it with a shared explicit epoch on `dev` (decided
2026-09-30), and its [`--hop` removal](/quest/m1/cluster-routing/hop-removal.md)
re-keys this quest's docs and tests. The importer work holds under either
key. Make every container importer (ts, fmp4, flv, mkv, and the
SRT, RTMP, and HLS gateways that reuse them) meet the contract when fed one
encoded stream. Capture is out: two encoders never align.

- A group's sequence derives from its keyframe's media timestamp (PTS in TS),
  not a per-process counter. Decide how both processes agree across a
  timestamp wrap (TS PTS wraps every 26.5 h) when they started on opposite
  sides of it.
- Frame timestamps derive from the input alone too. Importers publish the
  stream's own timestamps and refuse a rewind, so nothing shifts them.
- An importer announces only once it knows its tracks, so it never refuses a
  track the incumbent serves.
- Docs (`doc/bin/cli.md` "Redundant publishers", the `--hop` doc comment)
  narrow the contract to one encoded stream fed to each publisher.

Tests: per importer, two instances started at different offsets into the same
input produce the same group sequences and timestamps. End to end: the
issues' 1+1 setup (one relay, two same-hop `import ts`, two `export ts`)
survives the standby joining and the incumbent stopping.

## Closes

- [#4352](https://github.com/moq-dev/moq/issues/4352) - close this issue when the quest finishes
- [#4354](https://github.com/moq-dev/moq/issues/4354) - close this issue when the quest finishes

## Related

- [Remove `--hop`](/quest/m1/cluster-routing/hop-removal.md) - on dev, a redundant pair shares an explicit epoch instead, and this quest's docs and tests are re-keyed to it
- [Broadcast epochs](/quest/m1/broadcast-epoch/README.md) - a redundant pair shares one epoch
