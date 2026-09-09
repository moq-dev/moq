# [M] Recording format

## Goal

The HANG draft's Recording section (`drafts/draft-lcurley-moq-hang.md:739-913`)
specifies the object layout the archive line implements: a per-track `.info`
and segment objects, the timeline stored as an ordinary track, object listing
as bootstrap and recovery, and per-track omission when a PUT fails.
`just drafts check` passes.

## Plan

Why the section changes: S3 cannot append. The draft's `.timeline` object
(:826-859) grows by appending complete groups and is followed with ranged
GETs, which on an object store means rewriting the whole object per segment
and an entity-validator dance on every retention trim. Its writer also records
a whole-segment gap when any one track's object fails (:870-874), discarding
media that was stored fine.

Rewrite the section to this design:

- Layout (:750-775). Two object kinds only:

  ```text
  <prefix>/<encoded-track>/.info
  <prefix>/<encoded-track>/<segment>
  ```

  Keep the percent encoding of track names (:762-764), so an encoded name
  never contains `/` or starts with `.`. Drop `.complete` and `.timeline`
  (:754-755) and rename `.track` to `.info`, the one reserved name.
- Track objects (:777-793). `.info` is versioned JSON carrying the immutable
  `priority` and `timescale`, with the meanings of moq-lite `TRACK_INFO`.
  Delete :788-789: there is no broadcast epoch, and `Publisher Max Latency` is
  not stored because a reader serves with its own policy.
- Segment objects (:795-824). A versioned binary envelope: a header with the
  format version, a group/frame table (group sequence, each frame's timestamp,
  payload offset and length), then the payload bytes. The table locates one
  group or a `frame_start` without parsing frames, and a decoder bounds-checks
  every entry against the retrieved length before touching a payload. Update
  the Security Considerations paragraph (:922-925) from `Length` fields to the
  table.
- The Timeline Object (:826-859). Delete the section. The timeline is an
  ordinary track stored by the same `(track, segment)` rule and named by the
  catalog's `archive` entry ([catalog](/quest/m1/archive/catalog.md)). Each of
  its objects holds the complete Window groups (Track Framing, :590-616)
  closed in that span, so a reader replays checkpoints in order and nothing is
  ever rewritten.
- Writer (:861-882). A track's `.info` and segment object are durable before
  the record naming them is published, as today. A failed track PUT omits only
  that track from the record; the other tracks' ranges stay. This is the
  per-track counterpart of `Pending::gap()`
  (`rs/moq-mux/src/timeline.rs:712-719`), which clears every track;
  [writer](/quest/m1/archive/writer.md) adds the method beside it. No
  completion marker: a clean end flushes the final partial segment and
  finishes the timeline track.
- Bootstrap and recovery. Listing the prefix is how a reader or a restarting
  writer learns which timeline and track segments exist. A record the listing
  cannot back is not served.
- Retention (:884-896). Replace the ranged-GET and atomic-replace text with:
  expiry trims the timeline (`Producer::pop`, `timeline.rs:983`) and makes the
  trimmed timeline durable before the expired segment's objects are deleted.
- Reader (:898-913). Keep the whole-object GET rule. Drop the `.complete` and
  `.timeline` caching paragraph (:902-905). An unknown envelope version or a
  table entry outside the object is a missing segment, never an invalid
  recording.

Validate with `just drafts check`. No code changes; the crates follow in
[store](/quest/m1/archive/store.md), writer, and reader.

## Related

- [Archive store](/quest/m1/archive/store.md) - implements the layout and codecs this section specifies
