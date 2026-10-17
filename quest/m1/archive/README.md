# Broadcast archives

## Goal

`moq-archive` stores every track of every broadcast under a path prefix into
any `object_store::ObjectStore` and serves a stored broadcast back as an
ordinary broadcast, with no catalog dependency. `archive::Producer` records
from one SUBSCRIBE_ALL; `archive::Consumer` serves a stored prefix, which a
host publishes at a path of its choosing. Track, group, and frame structure and
payloads survive byte for byte, so any consumer of the live broadcast, HLS
included, works on the stored one.

The catalog carries no archive pointer. Timelines survive only as an opt-in
`hls` field for HLS compatibility.

## Plan

Decided by the maintainer on 2026-10-17, replacing the timeline-indexed
archive (per-track `.timeline.z` records named by the catalog's root
`archive` entry, found through its `replay` broadcast and `store` URL):

- **Formats break in place.** None of this runs in production, so the old
  layout and catalog section are deleted with no version bump or migration
  path; an old prefix holds no keys the new consumer recognizes and is
  refused as empty. This supersedes the earlier rule to preserve the v1 and v2
  formats. Add the `moq-archive` and `hang` entries to their `MIGRATE.md`.
- **Discovery is SUBSCRIBE_ALL**, a prefix-scoped moq-lite request for every
  track of every broadcast under a path, gating the lite-07 cut
  ([SUBSCRIBE_ALL](/quest/m1/archive/subscribe-all.md)). IETF SUBSCRIBE_TRACKS
  maps onto it 1:1, and PUBLISH onto the same hook
  ([SUBSCRIBE_TRACKS and PUBLISH](/quest/m1/archive/subscribe-tracks.md)): one
  mechanism, and IETF clients supported. Rejected: a per-broadcast request
  fanned out per announced broadcast, and exact-namespace only. The archive no
  longer selects tracks from a catalog.
- **The archive holds what the cache stops serving.** Each track records
  through an ascending subscription whose max delay is the publisher's max age
  capped by configuration; a group still missing at the cap is a gap, never a
  stall. Anything younger is still in the live cache.
- **Per-track objects, keyed by position.** Groups coalesce in order into
  objects cut by duration, keyed `<prefix>/<track>/<first group>-<first pts>`
  (`<first group>` for an untimed track), beside a small per-track `.info`
  holding its properties. The key listing is the index: no index object,
  manifest, or timeline. The application picks each broadcast's key prefix
  (per path and epoch, say), so a resume in one epoch continues from the
  stored keys and another epoch is another prefix. An optional DVR mode prunes
  old objects. Rejected: an object per whole-broadcast window (every read pulls
  every rendition) and size-based cuts. The
  [producer](/quest/m1/archive/producer.md) records the rest.
- **Hosts serve archives, relays do not.** `archive::Consumer` serves a stored
  prefix through `broadcast::Dynamic` and `track::Dynamic`, the archived
  catalog included as an ordinary track, and a host publishes it at a path of
  its choosing. Rejected: a moq-relay storage fallback. Also rejected: serving
  the archive as a covering-prefix route under the original path, which
  moq-lite routing defeats three ways: an exact live route shadows any prefix
  route at any cost, an Epoch-pinned request is UNROUTABLE unless the winning
  route has exactly that Epoch, and a refusal never falls through to another
  route. Stitching archived history into the original path is deferred.
- **Seek by time.** A `Time(pts)` range end
  ([Subscribe ranges](/quest/m1/subscribe-ranges/README.md)) is resolved by
  whoever holds the track, the cache or an archive consumer. DVR seeks with it.
- **`hls` is for HLS only, and opt-in.** A per-rendition catalog field names a
  track whose (first group, pts) records mark each HLS segment, and a publisher
  enabling it aligns segments across renditions
  ([Catalog HLS field](/quest/m1/archive/catalog-hls.md)). The root `archive`
  section is deleted everywhere. Rejected: always-on timelines.

Protected broadcasts stay out of scope. Recording keying, retention policy,
credentials, and discovery of stored prefixes belong to the host.

This README owns a new doc page, "Archives", under `doc/concept/`, written
once the children land: what SUBSCRIBE_ALL records, the store layout (linking
the moq-archive rustdoc that specifies it), and serving a stored broadcast
from a host. Each child fixes the stale docs it touches.

## Required

- [SUBSCRIBE_ALL](/quest/m1/archive/subscribe-all.md) - one prefix-scoped moq-lite-07 request delivers every track of every broadcast under a path, including later ones, and relays fan it out from one upstream request
- [Archive producer](/quest/m1/archive/producer.md) - `archive::Producer` stores every matching broadcast as duration-cut per-track objects keyed by group and pts, recording gaps instead of stalling, with an optional DVR mode
- [Archive consumer](/quest/m1/archive/consumer.md) - `archive::Consumer` serves a stored prefix as a broadcast from its key listing, and `moq export/import archive` use the new pair
- [Catalog HLS field](/quest/m1/archive/catalog-hls.md) - the root `archive` section is gone; an opt-in per-rendition `hls` field marks each aligned HLS segment's first group and pts and declares its nominal duration
- [SUBSCRIBE_TRACKS and PUBLISH](/quest/m1/archive/subscribe-tracks.md) - IETF SUBSCRIBE_TRACKS maps 1:1 onto SUBSCRIBE_ALL and PUBLISH onto its hook, instead of NOT_SUPPORTED
- [Fixed HLS target duration](/quest/m1/archive/hls-target.md) - one `EXT-X-TARGETDURATION` for the run, from the declared `hls` duration; an overrun is listed with a warning
- [History from the retained floor](/quest/m1/archive/history-floor.md) - HLS history over an archive consumer broadcast whose start was pruned lists from its oldest stored segment

## Related

- [Rust identity enforcement](/quest/m1/catalog-identity-rs.md) - a track's definition never changes for its name, so the archived catalog describes every group of the tracks it lists
- [e2ee](/quest/m2/e2ee/README.md) - protected broadcasts are excluded initially
- [DVR rewind](/quest/m2/archive-dvr.md) - seeks back through the cache, then an archive, with time ranges
