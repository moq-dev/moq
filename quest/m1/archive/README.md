# Broadcast archives

## Goal

`moq-archive` stores every track of every broadcast under a path prefix into
any `object_store::ObjectStore` and serves a stored broadcast back as an
ordinary broadcast, with no catalog dependency. A host finds
broadcasts by ANNOUNCE and runs one `archive::Producer` per broadcast, which
finds its tracks by TRACKS and records each through its own SUBSCRIBE; `archive::Consumer` serves a stored prefix, which a
host publishes at a path of its choosing. Track, group, and frame structure and
payloads survive byte for byte, so any consumer of the live broadcast, HLS
included, works on the stored one.

The catalog carries no archive pointer. Timelines survive only as a
per-rendition `hls` field for HLS compatibility.

## Plan

Decided by the maintainer on 2026-10-17, replacing the timeline-indexed
archive (per-track `.timeline.z` records named by the catalog's root
`archive` entry, found through its `replay` broadcast and `store` URL):

- **Formats break in place.** None of this runs in production, so the old
  layout and catalog section are deleted with no version bump or migration
  path; an old prefix holds no keys the new consumer recognizes and is
  refused as empty. This supersedes the earlier rule to preserve the v1 and v2
  formats. The Impact bullet says how to migrate in `moq-archive` and `hang`.
- **Discovery is pull**, decided 2026-10-19 in place of SUBSCRIBE_ALL's push:
  ANNOUNCE finds the broadcasts under a prefix, a moq-lite TRACKS request
  lists each broadcast's tracks as they appear, and the archive SUBSCRIBEs per
  track, gating the lite-07 cut (`broadcast::Consumer::tracks` in Rust,
  `broadcast.Consumer.tracks()` in JS).
  moq-transport answers SUBSCRIBE_TRACKS through the same hook in Rust and
  JavaScript and, in Rust, learns an IETF peer's tracks with a FORWARD=0
  SUBSCRIBE_TRACKS whose held PUBLISHes resume for data. Rejected:
  SUBSCRIBE_ALL, whose relay had to merge pushed feeds into pulled copies
  ([moq-dev/moq#5931](https://github.com/moq-dev/moq/pull/5931)). The archive
  no longer selects tracks from a catalog.
- **The archive holds what the cache stops serving.** Each track records
  through an ascending subscription whose max delay is the publisher's max age
  capped by configuration; a group still missing at the cap is a gap, never a
  stall. Anything younger is still in the live cache.
- **Per-track objects, keyed by position.** Groups coalesce in order into
  objects cut by duration, keyed
  `<prefix>/<track>/<first group>-<first frame>-<first pts>`
  (`<first group>-<first frame>` for an untimed track, `<track>` percent-encoded as one
  segment), beside a small per-track `.info` holding its timescale and
  priority. Each stored sequence keeps its form, group or datagram. A
  track's `end` records how it ended, clean or with its error code, written
  with the broadcast's `.end` once the broadcast's listing ends; without
  them an archive reads as live. A served track advertises the archive's retention (no limit), not
  the source's max age, which would expire archived history. The key listing is the index: no index object,
  manifest, or timeline. The application picks each broadcast's key prefix
  (per path and epoch, say), so a resume in one epoch continues from the
  stored keys and another epoch is another prefix. An optional DVR mode prunes
  old objects. Rejected: an object per whole-broadcast window (every read pulls
  every rendition) and size-based cuts. The
  `archive::Producer` records the rest.
- **Hosts serve archives, relays do not.** `archive::Consumer` serves a stored
  prefix through `broadcast::Requests` and `track::Requests`, the archived
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
- **`hls` is for HLS only.** A per-rendition catalog field names a
  track whose (first group, pts) records mark each HLS segment, each rendition
  cutting its own on a pts grid so they line up wherever keyframes do
  (Catalog HLS field, landed). The root `archive`
  section is deleted everywhere. Rejected: always-on timelines.

Protected broadcasts stay out of scope. Recording keying, retention policy,
credentials, and discovery of stored prefixes belong to the host.

This README owns a new doc page, "Archives", under `doc/concept/`, written
once the children land: what ANNOUNCE and TRACKS discover, the store layout (linking
the moq-archive rustdoc that specifies it), and serving a stored broadcast
from a host. Each child fixes the stale docs it touches.

## Required

- [Fixed HLS target duration](/quest/m1/archive/hls-target.md) - one `EXT-X-TARGETDURATION` for the run, from the declared `hls` duration; an overrun is listed with a warning

## Related

- [e2ee](/quest/m2/e2ee/README.md) - protected broadcasts are excluded initially
- [DVR rewind](/quest/m2/archive-dvr.md) - seeks back through the cache, then an archive, with time ranges
