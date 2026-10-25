# [M] Archive consumer

## Goal

`archive::Consumer` serves one broadcast's stored key prefix as an ordinary
broadcast: every stored track, the archived catalog included, with its
original groups, frames, timescale, and priority. A host publishes it at a path of its choosing;
moq-relay never uses it. `moq export archive` already records through
`archive::Producer`; `moq import archive` serves through
`archive::Consumer`. It replaces `moq_archive::Reader`, and with it goes
`.info`'s tolerance of the `epoch` an older `Writer` stored.

## Plan

Decided by the maintainer on 2026-10-17 (see the
[epic](/quest/m1/archive/README.md)):

- One cached LIST of the prefix is the index: it names the tracks and maps
  any group or pts to an object. Each track's `.info` supplies its timescale
  and priority before any media is read. A growing archive refreshes each
  track with its own cursor, a LIST of that track's key prefix StartAfter its
  newest known key, since one prefix-wide cursor skips a new key under a
  lexically earlier track. A delimiter LIST of the broadcast prefix finds
  tracks added since. Every object is written once, an open group continuing
  in the next object (decided 2026-10-21).
- Each served track advertises no Publisher Max Age limit (TRACK_INFO's
  encoded `0`), not the source's: the store's retention bounds what it holds,
  and DVR pruning is the publisher dropping a group early, which TRACK_INFO
  permits. Advertising the source's bound would expire every archived group
  older than it relative to the newest.
- Serve through `broadcast::Dynamic` and `track::Dynamic`. Create every listed
  track up front, so TRACKS on the served broadcast lists them all.
- The archived catalog is an ordinary track, so a reader sees its newest
  stored group, not a union of every rendition recorded. A rendition removed
  before the archive ends is therefore not listed at the end; the replay-catalog
  quest's union and `enabled: false` handling are dropped with it.
- The served broadcast takes its epoch from the caller or mints one; it never
  claims the source's epoch, since it is served at another path.
- Each stored sequence replays in its recorded form: a group as a group, a
  datagram as a datagram (`track::Producer::insert_datagram`), so the replay
  keeps the source's lossy, uncached datagram contract.
- A track's `end`, reached by its `StartAfter` refresh since it sorts after
  every object key, ends it as recorded. A clean end mirrors
  `moq_archive::Reader::finish` today: refreshing stops, the track is
  finished at the recorded end (`track::Producer::finish_at`), and
  committed groups stay servable. An error end serves the stored groups and
  then aborts the track with the recorded code (`StreamError::from_code`),
  failing loud as the source did rather than reading as live. The
  broadcast's `.end`, seen by the delimiter LIST, stops the search for
  added tracks. The producer writes every marker only once the broadcast
  has ended, so a live consumer finishes tracks only then; until then an
  ended track just stops growing, and a re-added one grows again. Without markers the
  archive is live and followed through refresh, which is how a producer
  that crashed or was stopped reads. This replaces the caller-driven
  `Reader::finish`.

Guidance:

- An absent group below the newest stored one is a gap and reports as dropped.
  A group below the oldest stored key was pruned or never stored and reports
  the same way.
- Keep a byte-bounded object LRU so neighbouring groups come from one GET.
- A `track::Fill` names group positions only, so a `Time(pts)` end the cache
  can't resolve never reaches the handler. Resolving one from the key listing
  (the object, then the group containing pts from its group timestamps) needs
  a fill that carries the anchor.
- Cover HLS export over a consumer broadcast with its archived catalog:
  playlists read only the timelines, and a segment GETs only its own
  rendition's objects. The Reader-based `moq-hls` archive tests went with
  `moq-archive`'s public segment API.
- Update `doc/bin/cli.md` and `doc/bin/hls.md` inline with an export, import,
  and HLS example.

Test with the in-memory store: a round trip preserving structure and payloads
byte for byte, a gap, a pruned start, a growing archive followed through
refresh (including two tracks where a new key sorts before the other track's
cursor, and a track added mid-recording), history older than the source's max
age still served, an untimed track, an interleaved group and datagram track
replayed in its original forms, a completed archive whose replay ends at
each track's recorded end (including mid-group), an errored track
replaying its stored groups and then the recorded error, a track removed
and re-added under its name replaying both runs in sequence, a growing archive
with no markers staying open, and an empty or unrecognized prefix refused loudly.

Public API: `archive::Consumer` replaces `Reader`. Wire: none.
