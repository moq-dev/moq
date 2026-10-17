# [M] Archive consumer

## Goal

`archive::Consumer` serves one broadcast's stored key prefix as an ordinary
broadcast: every stored track, the archived catalog included, with its
original groups, frames, and TRACK_INFO properties. A host publishes it at a path of its choosing;
moq-relay never uses it. `moq export archive` records through
`archive::Producer` and `moq import archive` serves through
`archive::Consumer`. It replaces `moq_archive::Reader`.

## Plan

Decided by the maintainer on 2026-10-17 (see the
[epic](/quest/m1/archive/README.md)):

- One cached LIST of the prefix is the index: it names the tracks and maps
  any group or pts to an object. Each track's `.info` supplies its TRACK_INFO
  properties before any media is read. A growing archive refreshes with
  StartAfter from the newest known key, and re-reads a track's newest object,
  which the producer may rewrite in place while an untimed group is open.
- Serve through `broadcast::Dynamic` and `track::Dynamic`. Create every listed
  track up front, so a SUBSCRIBE_ALL on the served broadcast sees them all.
- The archived catalog is an ordinary track, so a reader sees its newest
  stored group, not a union of every rendition recorded. A rendition removed
  before the archive ends is therefore not listed at the end; the replay-catalog
  quest's union and `enabled: false` handling are dropped with it.
- The served broadcast takes its epoch from the caller or mints one; it never
  claims the source's epoch, since it is served at another path.

Guidance:

- An absent group below the newest stored one is a gap and reports as dropped.
  A group below the oldest stored key was pruned or never stored and reports
  the same way; history-mode callers rely on the start answering with what
  remains ([History from the retained floor](/quest/m1/archive/history-floor.md)).
- Keep a byte-bounded object LRU so neighbouring groups come from one GET.
- When [Model ranges](/quest/m1/subscribe-ranges/model.md) lands, the
  `track::Dynamic` handler resolves `Time(pts)` ends from the key listing,
  whichever quest lands second.
- Move `rs/moq-hls/src/export/archive_tests.rs` onto a consumer broadcast with
  its archived catalog, deleting the hand-built catalog.
- Update `doc/bin/cli.md` and `doc/bin/hls.md` inline with an export, import,
  and HLS example.

Test with the in-memory store: a round trip preserving structure and payloads
byte for byte, a gap, a pruned start, a growing archive followed through
refresh, an untimed track, and an empty or unrecognized prefix refused loudly.

Public API: `archive::Consumer` replaces `Reader`. Wire: none.

## Required

- [Archive producer](/quest/m1/archive/producer.md) - the layout this serves
