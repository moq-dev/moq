# [S] Replay catalog

## Goal

A replayed recording plays as VOD HLS from the stock server: `moq export
archive`, then `moq import archive`, then `moq export hls` lists the whole
recording with no embedder supplying a catalog. The replay broadcast publishes
its recorded catalog live, stamped with the recording's `store` and `version`,
so an exporter in history mode treats its timeline as durable and lists past
`--window`. A `--follow` replay grows like an event playlist.

[Catalog track identity](/quest/m1/catalog-tracks.md) guarantees a track's
identity never changes for its name, and resolution changes in band below
its ceilings, so the newest recorded catalog describes every group of the
tracks it lists; nothing here picks a catalog per group.

## Plan

Today the reader writes live groups only for timelines; the recorded catalog
is FETCH-only, so a SUBSCRIBE to it on the replay broadcast never sees a group
and `moq-hls` never finds the `archive` entry. The HLS archive tests work around
this by hand-building a catalog.

- Republish each recorded catalog group live in its timeline's order, so the newest
  one is at the live edge and a `--follow` replay picks up catalogs recorded
  after it opened.
- Stamp `store` with the URL passed to `import archive`, and `version` with the
  recording format. Refuse a URL carrying userinfo and strip its query so
  credentials never land in a catalog, and let the importer opt out of
  advertising the URL at all. `replay` stays unset: the timelines live on this broadcast.
- The logic lives in `moq-archive` (`moq_archive::Reader`), so any host of
  the archive behind the root claim republishes and stamps the catalog, not
  only `moq-cli` (decided 09-29 in [DVR rewind](/quest/m1/archive/dvr.md)).
  `moq-cli` only passes the URL and the opt-out. Select the catalog track by
  catalog format as `export archive` does: hang and hang.z are stamped, MSF
  is refused.
- The replay keeps minting a fresh epoch per run (#4942), so a
  live-to-archive handover at one path is a `Restart`, not a resume (decided
  in the 2026-10-08 audit, after #5043's review). Reusing the recorded
  `.info` epoch was rejected for now: the archive writes its own
  `<track>.timeline.z` groups under the live names, and this quest rewrites
  catalog groups, so the same name, epoch, and group number could carry
  different bytes. Revisit only once replayed metadata is byte- and
  sequence-identical to live. So `moq import archive` refuses `--epoch`
  (`takes_epoch` false for `Archive`), and any `moq-archive` host mints its
  own epoch or takes one from its caller, never defaulting to `.info`'s.
- The recorded catalog's `archive` entry describes the source's live
  timelines, not the recording's, so replace it with the timelines the reader
  replays rather than trusting the recorded ones.

A live playlist stays capped even for a durable timeline; listing past the
window is the explicit `moq_hls::export::Config::history` mode, which `moq
export hls` does not expose yet. Give it a flag here. History mode lists from
the records the timeline restates when the exporter joins (at most 256 from a
`moq-mux` publisher), so "the whole recording" also needs the exporter to
read a longer recording's stored history rather than only its live
restatement.

Add a CI test that records a broadcast longer than the default window, replays
it, and asserts the stock exporter lists segment 0 with a durable
`timeShiftBufferDepth`. Consider moving the HLS archive tests onto the
republished catalog instead of their hand-built one.

Update `doc/bin/cli.md` and `doc/bin/hls.md` inline with an export, import, and
serve example.

Cover a DVR recording whose catalog outlived its first video segment, and fail
loudly on a recording with no catalog.

## Required

- [Catalog track identity](/quest/m1/catalog-tracks.md) - the guarantee that one recorded catalog describes every group
- [History from the start](/quest/m1/archive/replay-history.md) - history mode lists a recording from its start, which "lists the whole recording" needs
