# [M] Same-epoch importers publish identical tracks

## Goal

Two importers publishing one path, such as a redundant pair sharing an
explicit `--epoch`, and fed the same encoded stream publish identical tracks,
so a relay can fail over between them. Today two `moq import ts` fed one
forked stream break that contract: each numbers its groups from its own
counter, so after a failover `export ts` sees a timestamp rewind and exits
(#4354).

## Plan

Decided: every publisher of one path and epoch MUST publish the same
broadcasts and tracks, since routes with the same epoch resume each other's
subscriptions. A redundant pair is keyed by a shared explicit epoch (decided
2026-10-03: `--hop` no longer exists to key it). Every importer that
`moq import --epoch` accepts meets the contract when fed one encoded stream:
ts with one program, ts `--passthrough` (a whole multiplex included), fmp4,
flv, and the HLS pull that reuses them. RTMP and SRT imports, the WHIP
listener, and `ts --program all` are out: they mint an epoch per ingest
connection (#4962), so two of them never share one. Publishers whose
timestamps or bytes come from the instance (avc3, capture, `rtc --connect`,
archive, and transcode) refuse a shared epoch.

Decided 2026-10-13 by the maintainer: every importer of an epoch stays live
and derives one coordinate from the epoch and its source alone
([Epoch anchor](/quest/m2/epoch-anchor.md)), superseding the same day's plan
where later importers adopted the first one's. This quest numbers groups on
that coordinate and proves the pair end to end. A raw PTS or PCR value alone
does not supply it, and arrival time chooses only the era. Considered
instead: one importer per epoch with redundancy only downstream, which stays
the supported 1+1 until this lands, and a contract narrowed to what one input
proves, which fails at the first failover across a timestamp wrap.

- A group's sequence derives from its first frame's time on the epoch
  timeline (the keyframe in the demultiplexed lane, the group's first object
  in passthrough), not a per-process counter, so two importers number the
  same group alike wherever each started. A source may start consecutive
  groups at one timestamp (`container::Producer` accepts a keyframe at the
  previous group's start), which time alone cannot number without a
  history-derived counter: settle a tie-breaker the source carries, or
  refuse equal starts under a shared epoch, and cover it in the pair test.
- Frame timestamps derive from the input and the epoch's coordinate alone,
  never from `clock.now()` (2026-10-06 audit), and importers refuse a rewind.
- The catalog's root `clock` is the epoch's, never an importer's arrival
  (2026-10-05 audit; narrowing the contract to exclude the catalog clock was
  rejected). Every other catalog field an importer derives from its history
  derives from the epoch and the content instead, never from the live
  catalog.
- The tracks an importer generates are tracks too: the catalog tracks
  (hang, compressed, MSF) and `*.timeline`. Today catalog groups are
  numbered locally (`Outputs::emit`), the MSF frame is stamped with
  `Timestamp::now()`, and timeline records start at zero, so an importer
  started later would reuse earlier group or record numbers for newer bytes.
  Their numbering and timestamps come from the epoch timeline and the
  content, never the instance or the live track.
- Docs (the `--epoch` paragraph in `doc/bin/cli.md`, the `--epoch` doc
  comment) state the contract: one encoded stream fed to each publisher, no
  access to the live broadcast needed, and a wall mapping only as accurate as
  the epoch's wall time matches the source.

Tests: per importer, two instances started at different offsets into one
input, including the later one starting after a timestamp wrap the earlier
crossed, publish identical catalogs, group sequences, and timestamps, on
the generated catalog and timeline tracks too, starting after earlier
catalog updates and timeline records. End to end: the issues' 1+1 setup (one
relay, two `import ts` sharing one `--epoch`, two `export ts`) survives the
second importer starting, before and after a wrap, and either importer
stopping and restarting; repeat it with `--passthrough`.

## Closes

- [#4354](https://github.com/moq-dev/moq/issues/4354) - close this issue when the quest finishes

## Required

- [Epoch anchor](/quest/m2/epoch-anchor.md) - every importer derives one catalog clock and timestamp era from the epoch

## Related

- [Broadcast epochs](/quest/m0/broadcast-epoch/README.md) - a redundant pair shares one epoch
- [E2EE](/quest/m2/e2ee/README.md) - an encrypting publisher refuses a shared epoch, so a redundant pair is plaintext only
- [Deterministic TS legs](/quest/m2/ts-hitless.md) - what two exports of the pair render from the common coordinate
