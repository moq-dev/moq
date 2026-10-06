# [S] Replay catalog

## Goal

A replayed recording plays as VOD HLS from the stock server: `moq export
archive`, then `moq import archive`, then `moq export hls` lists the whole
recording with no embedder supplying a catalog. The replay broadcast publishes
its recorded catalog live, stamped with the recording's `store` and `version`,
so the exporter treats its timeline as durable and lists past `--window`. A
`--follow` replay grows like an event playlist.

Choosing which catalog applies to which media group stays with
[Catalog track identity](/quest/m2/catalog-tracks.md).

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
- Keep the logic in `moq-cli`; `moq-archive` stays catalog-agnostic. Select the
  catalog track with the CLI's catalog format as `export archive` does: hang
  and hang.z are stamped, MSF is refused.
- The recorded catalog's `archive` entry describes the source's live
  timelines, not the recording's, so replace it with the timelines the reader
  replays rather than trusting the recorded ones.

Add a CI test that records a broadcast longer than the default window, replays
it, and asserts the stock exporter lists segment 0 with a durable
`timeShiftBufferDepth`. Consider moving the HLS archive tests onto the
republished catalog instead of their hand-built one.

Update `doc/bin/cli.md` and `doc/bin/hls.md` inline with an export, import, and
serve example.

Cover a DVR recording whose catalog outlived its first video segment, and fail
loudly on a recording with no catalog.
