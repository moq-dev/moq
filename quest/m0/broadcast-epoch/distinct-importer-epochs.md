# [L] Redundant importers run on distinct epochs

## Goal

Two importers fed one stream and publishing one path, a redundant ingest
pair, each mint their own epoch, and a consumer that follows restarts (an
announcement-following player, `moq export ts --stitch`) moves to the
survivor as a marked restart; any other consumer ends, as on any restart. `moq --epoch` is gone, so only replicas of
one publisher's bytes (one process announced through several relays or CDNs)
share an epoch, and same-epoch identity holds by construction. The importer
and mux machinery that existed only so independent importers agree is
reverted.

Today two `moq import ts` sharing an `--epoch` number groups per process, so
a relay resumes a subscription mid-group on other media and `moq export ts`
exits on a rewind (#4354).

Non-goal: hitless failover between importers. That stays downstream, as
2022-7 legs of one broadcast ([Redundant TS exports](/quest/m2/ts-hitless.md)),
or upstream, by merging legs before a single importer.

## Plan

Decided 2026-10-19 by the maintainer, settling the redundancy-model
decision quest for switchover and superseding the 2026-10-13 active-active
plan (the deleted `epoch-anchor` and `hop-aligned-import` quests) and
#5895's one-second group grid: independently fed importers
cannot publish identical tracks. Each sees its own loss, start point, and
history, and once loss or reordering reaches one, "same epoch, same content"
is false and a mid-group resume splices two streams. Every alignment rule so
far (#5625's epoch-derived clock and eras, #5894's catalog-clock grid cuts
and windows, #5895's grid numbering) added cost and still left audio after
loss, the catalog tracks, and the timeline divergent. Considered instead:
numbering groups by their start time (DASH `$Time$` style), which fixes audio
under loss but keeps every other alignment rule.

- Remove `moq --epoch` and its plumbing: `epoch_refusal`, the shared-epoch
  placement in `rs/moq-cli/src/publish.rs` (`timebase`), and the `--epoch`
  text in `doc/bin/cli.md`, `doc/concept/moq-lite.md`, and
  `doc/bin/relay/cluster.md`. It never shipped (absent from `release`), so no
  MIGRATE entry. Every `moq` run mints its own epoch. Route-level epochs in
  moq-net and the bindings stay: an application that forwards or replicates
  one publisher's bytes still announces that publisher's epoch.
- Failover uses what exists: the newest epoch wins new requests, losing it
  makes a still-live older one win as a new broadcast, players follow the
  announce `Restart`, and `export ts --stitch` follows a replacement as a
  program switch. Unchanged (maintainer, over staying on the current instance
  until it ends): `--stitch` follows a newer epoch at once, so starting or
  restarting the backup importer is a marked seam for every stitching export
  even when nothing failed.
- Audit the importers and `moq-mux`: revert every behavior that exists only
  so same-epoch importers agree, even a harmless one, unless a single
  importer is better or simpler with it, and justify each kept item in the
  PR (maintainer, over reverting only what has a user-visible cost).
  Candidates: #5625's clock placement and per-value eras from the epoch (the
  13.25 h wall-mapping bound and the half-period failure cases go with
  them), #5894's catalog-clock grid fallback cuts and the two-second
  `randomAccess` and `muxRate` windows of the m2ts section, history-free
  rules that replaced simpler history-based ones (such as the removed
  passthrough clamp), and catalog fields derived from the epoch instead of
  the importer's history. Update `drafts/draft-lcurley-moq-mpegts.md` for
  every wire-visible reversal (the m2ts section is in no published draft),
  and drop the e2ee draft's "explicit or shared epoch" wording if nothing
  can hand a core one anymore.
- One quest and one PR (maintainer, over splitting the removal from the
  audit).

Tests: #4354's 1+1 setup on distinct epochs, on lite-07: one relay, two
`moq import ts` fed one stream (and again with `--passthrough`), two
`moq export ts --stitch`. Stopping the serving importer, and starting a
backup, each produce one marked program switch and no exit, and a restarted
importer wins again as the newest. Run it on the bufferbloat harness and
report the gap a viewer sees at each switch. Tests that only proved same-epoch
agreement go with the code they covered.

Public API: removes `moq --epoch`, plus whatever `moq-mux` surface the audit
reverts; report each item. Wire: the m2ts catalog section changes as the
audit decides, in no published draft.

## Closes

- [#4354](https://github.com/moq-dev/moq/issues/4354) - close this issue when the quest finishes

## Related

- [Redundant ingest guide](/quest/m1/redundant-ingest-guide.md) - documents running a redundant pair over multiple routes, for any publisher
- [Redundant TS exports](/quest/m2/ts-hitless.md) - hitless legs of one broadcast, downstream of the importer
- [E2EE](/quest/m2/e2ee/README.md) - an encrypting publisher never takes an epoch it did not mint
