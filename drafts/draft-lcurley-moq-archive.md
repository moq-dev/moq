---
title: "Media over QUIC - Archive"
abbrev: "moq-archive"
category: info

docname: draft-lcurley-moq-archive-latest
submissiontype: IETF  # also: "independent", "editorial", "IAB", or "IRTF"
number:
date:
v: 3
area: wit
workgroup: moq

author:
 -
    fullname: Luke Curley
    email: kixelated@gmail.com

normative:
  RFC9000:

informative:
  moql: I-D.lcurley-moq-lite
  hang: I-D.lcurley-moq-hang

--- abstract

moq-archive is a storage format for MoQ tracks.
A writer records the groups of a track in arrival order, bundling them into immutable chunk objects suitable for object storage or a filesystem, with an append-only index per track locating every group.
A reader reconstructs individual groups from byte ranges, servicing FETCH requests from the archive as if from a live publisher.

The archive is a faithful record of what arrived: late groups are recorded without head-of-line blocking, gaps are representable, and any track can be archived regardless of its payload.
Presentation-ordered formats such as HLS are views derived from an archive at read time, not storage formats.

--- middle

# Conventions and Definitions
{::boilerplate bcp14-tagged}

Fields annotated `(i)` are variable-length integers using the QUIC encoding ({{RFC9000}}, Section 16).

This document is self-contained: the byte encodings below are frozen by the archive `version` and do not reference any wire protocol version.
Where an encoding happens to coincide with moq-lite {{moql}}, that is called out as an informative note.


# Introduction
moq-lite {{moql}} delivers the present via SUBSCRIBE and the recent past via FETCH, bounded by the publisher's cache.
There is no persistent tier: no standard way to record a broadcast to disk or object storage and later serve it back, whether to a MoQ subscriber (via FETCH), an HTTP client (via byte ranges), or a derived format such as HLS.

Recording by transmuxing into a presentation-ordered media format at write time has structural problems:

- It forces a choice between head-of-line blocking and data loss: a presentation-ordered format cannot represent out-of-order arrival, so a late group must either stall the recorder or be skipped forever.
- Tracks with very small groups (for example audio, where a group is often a single frame) produce pathological object counts and index sizes when stored per-group.
- Only media tracks can be recorded; a track whose payload the recorder does not understand has no representation.
- Derived artifacts (playlists, init segments) get materialized and must then be maintained as retention windows slide.

moq-archive instead records at the track layer: groups of opaque frames, in arrival order, bundled into chunks on a flush interval.
The format is deliberately dumb.
All media semantics (codec configuration, timestamps-to-wall-clock mapping, rendition relationships) live in the recorded tracks themselves — a catalog or timeline track {{hang}} is archived like any other track.


# Terminology
This document uses the moq-lite {{moql}} terms Broadcast, Track, Group, and Frame, plus:

- **Archive**: the stored representation of one broadcast: a broadcast description, and per track an index plus a family of chunks.
- **Chunk**: an immutable object containing the span payloads flushed in one interval, in group-sequence order.
- **Span**: a contiguous range of frames from a single group, stored inside a chunk.
- **Index**: a track's append-only binary log of chunk records, holding all structure: which groups each chunk contains and their byte sizes.
- **Flush**: the periodic act of writing buffered groups out as a new chunk.


# Overview
An archive is a set of objects under a common prefix:

~~~
broadcast.json
<track>/index.bin
<track>/<c>.dat
~~~

`<track>` is the track's name, percent-encoded: every byte outside `A-Z a-z 0-9 _ -` is escaped, so an encoded name can never contain a dot and can never collide with `broadcast.json`.
`<c>` is a chunk number, sequential from 0 within its track.

Metadata is stored exactly once, in the index: chunks are pure payload — concatenated frames with no header, no magic, and no internal structure — the same split as an MP4 file's `moov` and `mdat`.
Chunks are immutable and stored raw (media payloads do not compress).
Each track's index is a binary log, append-only in content — a record, once written, never changes — though the object is rewritten as the archive grows; it is uncompressed and records are length-prefixed, so a reader refreshes with a plain ranged GET and needs no earlier bytes to decode the new ones.
`broadcast.json` is rewritten only when a track is added and once at sealing: tracks can appear mid-broadcast, and re-reading `broadcast.json` is how a storage-only follower discovers them.

The writer subscribes to a track and buffers groups as they arrive.
On each flush it writes one chunk containing every group completed since the previous flush, plus a span for any group that has remained open longer than the flush interval, then appends the chunk's record to the track's index.
A group that arrives late is simply written to whichever chunk is open when it completes; ordering across chunks is arrival order, and only ordering *within* a chunk is by group sequence.

The reader inverts this: given a group sequence, it finds the chunk record(s) listing that group, computes byte offsets from the record alone, and range-reads the payload — one request, no intermediate hop.
Span payloads are stored in a frame encoding designed to match FETCH, so concatenating a group's spans *is* the FETCH response for that group.


# Broadcast Description
`broadcast.json` is a single JSON object describing the archive:

~~~
{
  "version": 1,
  "flush_ms": 5000,
  "flush_bytes": 1048576,
  "complete": false,
  "tracks": {
    "video": { "timescale": 90000, "priority": 1,
               "ordered": false, "latency_max_ms": 4000 }
  }
}
~~~

**version**:
The archive format version, currently 1.
A reader MUST reject an archive with an unrecognized version.
Every byte encoding in this document is frozen by this value.

**flush_ms / flush_bytes**:
The writer's flush thresholds.
Informative for reading, but they tell a live follower how often new chunks can appear (see [Following a live archive](#following)).

**complete**:
`false` while the archive is being written; `true` once sealed.
Once complete, every object in the archive is immutable, this file included.

**tracks**:
A map from track name to the track's immutable publisher properties; `timescale` is REQUIRED, since frame timestamps are meaningless without it.
Entries are added as tracks are discovered and never removed; the percent-encoded name is the track's path prefix.
A track whose payload is itself a description of other tracks — the catalog track of {{hang}} — is listed and recorded like any other track; the archive deliberately carries no media semantics.


# Track Index
`<track>/index.bin` is the track's index: a binary log, one record per chunk, in chunk order — the `i`-th record describes chunk `i`.
It holds ALL of the archive's structure for the track; losing it loses the track (see [Writer Behavior](#writer)).

~~~
Record {
  Length (i)
  Run Count (i)
  Run {
    Group Start (i)
    Group Count (i)
  } ...
  Size (i) ...
  Partial Count (i)
  Partial {
    Group (i)
    Frame Offset (i)
  } ...
}
~~~

**Length**:
The byte length of the rest of the record.
A reader range-reading the log's tail can land on an incomplete record; the length prefix tells it to buffer until the rest arrives.

**Runs**:
Run-length pairs: the exact, ascending set of Group Sequences with a span in this chunk (`Group Start` inclusive, `Group Count` many).
This is membership: a group listed in no record is a gap.
Consecutive sequences collapse into one run, so the encoding stays small even for single-frame audio groups; a late arrival or a hole simply starts a new run.
A chunk MUST NOT contain two spans for the same group.

**Sizes**:
One per group, in run-expansion order: the byte size of that span's payload.
Payloads are stored in this same order, so span `i` begins at the sum of the sizes before it — every byte offset in the chunk is computable from the record alone.
An empty group (zero frames, potentially indicating a gap in the track) has size 0.

**Partials**:
Present for continuation spans: this chunk's span for `Group` starts at `Frame Offset` rather than at 0.
A group split across chunks has one span in each, listed in every corresponding record; a reader concatenates them in frame-offset order.
Within one group, data arrives as an ordered stream, so spans are contiguous continuations — never overlapping, never leaving holes within the recorded prefix.
If a writer nevertheless records overlapping frame ranges for a group (for example after crash recovery re-fetched a group it had partially written), the later chunk wins for the overlapping frames, and readers MUST resolve overlaps in favor of the higher chunk number.

## Append and refresh
The log has no header: the format version lives in `broadcast.json`, and the first record starts at byte 0.
Its content is append-only: bytes once written never change.
A reader refreshes incrementally — over HTTP, a ranged GET from its previous offset, with the ETag (`If-Range`) guarding against a replaced object; on a filesystem, by watching the file size — and parses only the new records.
The index is stored uncompressed precisely so that a ranged read is decodable without any earlier bytes; storage- or transport-layer compression MAY apply transparently.

The writer SHOULD rewrite the index object on every flush: it is the only durable record of the chunk's structure, and until it lands the chunk is unreadable.


# Chunk Format
A chunk, `<track>/<c>.dat`, is the concatenated span payloads of one flush — nothing else.
There is no header, no magic, and no per-span framing: all structure lives in the chunk's index record, and a chunk is unreadable without it (deliberately, the `mdat` model).

Payloads appear in `groups` expansion order (ascending group sequence).
Each payload is the span's frames, encoded as follows.

## Frame Encoding {#frame-encoding}
A span's payload is consecutive frames:

~~~
Frame {
  Timestamp Delta (i)
  Length (i)
  Payload (..)
}
~~~

**Timestamp Delta**:
A signed delta from the previous frame's timestamp, in the track's timescale, encoded as a zigzag-mapped variable-length integer:

- Encode: `unsigned = (signed << 1) ^ (signed >> 63)` (arithmetic right shift).
- Decode: `signed = (unsigned >> 1) ^ -(unsigned & 1)`.

The delta chain is continuous across the spans of one group: the first frame of a group is delta-encoded from 0, and a continuation span's first frame is delta-encoded from the last frame of the previous span.
A continuation span decoded in isolation therefore yields relative timing only; absolute timestamps require starting from the group's first span.

The archive adds no timestamp fields of its own: time appears exactly where the frozen frame encoding carries it, and time-based seeking belongs to application tracks such as the timeline {{hang}}, not to this format.

**Length**:
The size of the payload in bytes.

**Payload**:
The frame's application-specific payload.

This encoding is frozen by the archive `version`; changes require a new version.
Informative note: it is byte-identical to the FRAME message of moq-lite-06, so a server speaking that protocol version can serve a FETCH response by concatenating a group's span payloads verbatim.
A future moq-lite revision that changes FRAME does not affect stored archives; servers re-frame at the boundary instead.


# Writer Behavior {#writer}
On each flush interval (or earlier, when buffered bytes reach `flush_bytes`, or at shutdown), the writer:

1. Collects every group that completed since the previous flush, sorted by group sequence.
2. For every group still open that has been open for longer than the flush interval, takes the frames received so far beyond what was already written, as a continuation span.
   This bounds writer memory and handles unbounded groups (long GOPs, and single-group-forever tracks such as timelines) uniformly.
3. Writes the chunk, then its record: the chunk object MUST be fully written before the record referencing it is stored or published, so a reader following the index never observes a dangling reference.
4. Rewrites the track index object, and publishes the record on a live index track, if any.

`broadcast.json` is written when the archive is created, rewritten when a track is discovered, and rewritten once more at sealing with `complete: true` (after every track's final chunks and index rewrites).

A writer that restarts simply opens the next chunk of each track and continues; the format is append-only and needs no repair step.
A crash between a chunk write and its index rewrite orphans that chunk: the bytes exist but their structure was never recorded, so they are unreadable — the same way an MP4 without its `moov` is unreadable.
This loss is bounded by the flush interval and is the accepted cost of storing metadata exactly once.
If a restarted writer re-receives groups it already wrote, it SHOULD skip the frames it already recorded and write only the missing suffix; if it cannot know (lost state), it MAY rewrite the group in full, relying on the later-chunk-wins rule.

The writer records what arrived.
It MUST NOT wait for a missing group before flushing subsequent ones, and it MUST NOT drop a group merely because it arrived late.


# Reader Behavior
To reconstruct group `g` of a track, a reader scans the track's index for the record(s) whose runs list `g`, computes each span's offset and length from the sizes, range-reads the payloads, and concatenates them in frame-offset order.
The result is a complete frame sequence for the group and MAY be served as the body of a moq-lite FETCH response (see the informative note in [Frame Encoding](#frame-encoding)).

A reader mounted behind a MoQ publisher services FETCH from the archive; a group absent from the archive is a normal FETCH failure (stream reset), which the server MAY instead forward upstream to a live publisher when one exists.

Readers MUST tolerate gaps: group sequences in an archive are not contiguous, and no ordering across chunks may be assumed.
A reader presenting the archive as a track (for example when deriving a presentation-ordered view such as HLS) maps gaps to whatever the target format uses (for example a discontinuity), rather than failing.

## HTTP access
Chunks are immutable and SHOULD be served with long-lived caching.
The track indexes change while the archive is live and SHOULD be served with short TTLs, as should `broadcast.json`; everything becomes immutable once `complete`.
Group reconstruction over HTTP costs one range request per span — usually exactly one — and sequential playback amortizes further by fetching whole chunks.
A relay serving FETCH from an archive keeps the parsed indexes in memory and caches hot chunks rather than re-reading them.

## Following a live archive {#following}
A follower wants to learn about new chunks as they appear.
In order of preference:

1. **A live index track.**
   A writer MAY publish each track's records on a live MoQ track (one record per flush) so a MoQ-connected follower is pushed each record and never polls storage.
   The track conventions (for example a single group carrying one record per frame) are out of scope here; only the record encoding is normative.
2. **A blocking origin.**
   An origin smarter than raw object storage MAY offer blocking reads: a request for an index range (or chunk) that does not yet exist is held until it does, turning polling into long-polling.
   This is the same shape as blocking playlist reload in low-latency HLS and requires no changes to the stored format.
3. **Polling.**
   Against raw object storage, a follower issues conditional or ranged-from-known-offset GETs for the track indexes it plays, at the `flush_ms` cadence.
   This is the degraded mode; a live deployment is expected to provide 1 or 2.

Track indexes never reveal *new tracks*: those appear only in `broadcast.json` (or via live signaling such as a catalog track), so a follower expecting dynamic tracks re-reads it at a coarse cadence.

Once `complete` is set there is nothing to follow, and a pure-HTTP reader needs no polling at all.


# Security Considerations
An archive is data at rest and inherits the confidentiality and integrity properties of its storage; encryption at rest (for example SSE-C) is transparent to this format and out of scope.

A reader parsing an untrusted archive MUST bounds-check everything: record Lengths against the index object, run and partial counts against their record, the sum of a record's sizes against the chunk object's actual size, frame Lengths against their span payload, and partial frame offsets against the spans recorded elsewhere for the group.
Varint fields are subject to the same limits as QUIC.
A malformed index record SHOULD invalidate only the chunks it describes, not the archive.


# IANA Considerations
This document has no IANA actions.


--- back

# Appendix A: Design Notes (Informative)

The layout borrows from formats that solved adjacent problems:

- **MP4 (`moov`/`mdat`)**: metadata stored exactly once, separately from a payload blob that is unreadable without it.
  The track index is the `moov`; chunks are `mdat`.
  Unlike MP4 the metadata is a growable log rather than a finalized header, so the archive is readable while it is being written.
- **Kafka log segments**: an arrival-ordered append log with a compact index mapping logical positions to byte offsets, polled by consumers via their own committed offset.
  Ranged-GET refresh from a reader's previous offset is the same consumption model.
- **DASH on-demand (`sidx`) / HLS byte-range playlists**: an external index of byte ranges into a media blob, fetched first, then ranged into.
  The chunk record plays the `sidx` role, per flush instead of per file.
- **MCAP**: a chunked container for heterogeneous timestamped message streams with per-chunk message indexes.
  Closest in spirit, but single-file with a finalizing footer (an archive must be readable while it grows across objects) and message-schema-centric rather than group/frame-structured, so it is inspiration rather than a dependency.
- **Low-latency HLS blocking playlist reload**: the precedent for the blocking-origin follow mode.

# Appendix B: Worked Example (Informative)

A broadcast with three tracks, archived with `flush_ms: 5000`:

- **video**: 2-second GOPs at 30fps, so a group is 60 frames.
- **audio**: one 20ms frame per group, so 250 groups per flush interval.
- **timeline**: a single group that never closes, appending a record per video group.

Index records are shown symbolically as `runs / sizes / partials`; the wire is varints.

**t=5 (chunk 0)**: video groups 0-1 are complete; group 2 has been open for only 1 second and is held.
Audio groups 0-249 are complete.
The timeline group has been open longer than the interval, so its records so far are written as a continuation span.

~~~
video/0.dat      payloads: g0 (60 frames), g1 (60 frames)
audio/0.dat      payloads: g0 .. g249 (1 frame each)
timeline/0.dat   payloads: g0 frames 0-2
~~~

~~~
video    record 0: runs=[0+2]    sizes=[1310720, 1310720]
audio    record 0: runs=[0+250]  sizes=[164, 158, ...]
timeline record 0: runs=[0+1]    sizes=[56]
~~~

**t=7.2**: audio group 312 is lost in transit; the retransmit arrives at t=10.4, after the next flush.
The writer neither waits nor drops.

**t=10 (chunk 1)**: video groups 2-3 (group 4 held); audio 250-499 except 312; another timeline continuation:

~~~
audio    record 1: runs=[250+62, 313+187]  sizes=[...]
timeline record 1: runs=[0+1]  sizes=[38]  partials=[g0@3]
~~~

The hole is visible in the audio record itself: membership is exact, and group 312 will start a new pair elsewhere.

**t=15 (chunk 2)**: the late group 312 lands in whichever chunk was open when it completed:

~~~
audio    record 2: runs=[312+1, 500+250]  sizes=[160, ...]
~~~

Reads, using only the records above:

- **Video group 1**: listed by chunk 0 at expansion position 1, so its payload is bytes `[1310720, 2621440)` of `video/0.dat` — one range request, straight from the record.
- **Audio group 312** (late): listed only by chunk 2's record, at position 0: bytes `[0, 160)` of `audio/2.dat`.
  Chunk 1 was ruled out from its record alone.
- **Timeline group 0** (split): listed by every timeline record; concatenate the spans in frame-offset order (the partials give each continuation's frame offset) for the group's complete frame sequence.
  A reader wanting only the newest records decodes the last span alone: timing there is relative, and the timeline's own payload records carry their absolute positions.

A live follower subscribed to the index tracks receives each record above as it is written; at t=14 it knows exactly which bytes exist through chunk 1, serves anything older from storage, and bridges the last interval with FETCH before splicing into SUBSCRIBE.

# Appendix C: Open Questions

- Whether to reintroduce optional index compression later (dropped for v1 simplicity; sync-flushed DEFLATE would preserve append-only refresh at the cost of decode state).
  Sizes dominate the index (one varint per audio group), though it remains a rounding error next to the media itself.
- Whether the index should record catalog *generations* (which catalog group covers which chunk range) or leave that entirely to reading the recorded catalog track.
- Whether a per-span checksum is worth the bytes, or whether storage-layer integrity suffices.
- The exact conventions for the live index track (currently deferred to {{hang}}-style tracks and out of scope here).
