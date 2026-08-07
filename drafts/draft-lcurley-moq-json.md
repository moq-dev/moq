---
title: "Media over QUIC - JSON Tracks"
abbrev: "moq-json"
category: info

docname: draft-lcurley-moq-json-latest
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
  moql: I-D.lcurley-moq-lite
  moqt: I-D.ietf-moq-transport
  JSON: RFC8259
  MERGEPATCH: RFC7396

informative:
  flate:
    title: "Media over QUIC - Group Compression"
    target: https://datatracker.ietf.org/doc/draft-lcurley-moq-flate/
    author:
      - fullname: Luke Curley
    date: false
  HLS: RFC8216
  msf: I-D.ietf-moq-msf

--- abstract

This document specifies three encodings for publishing JSON over a Media over QUIC track: **snapshot** (one value updated over time), **stream** (an append-only log), and **window** (a log that appends at the tail and removes from the head, like an HLS playlist).
Each encoding maps JSON values onto the transport's group model so a subscriber can join mid-stream and old state can be evicted from caches.
An optional layer compresses each group as one shared DEFLATE window, so repetitive values shrink sharply.
The encodings are defined for both moq-lite and moq-transport.

--- middle

# Conventions and Definitions
{::boilerplate bcp14-tagged}


# Introduction
Live applications publish more than media: a catalog of available tracks, a timeline mapping groups to timestamps, an event log, a set of participants.
These are naturally JSON documents that change over time, and they change in three distinct ways:

- A **snapshot** is one value where only the latest matters, such as a catalog. Intermediate updates may be collapsed or skipped.
- A **stream** is an ordered log where every record matters, such as telemetry. Nothing is ever superseded.
- A **window** is an ordered log with a moving head and tail, such as a media timeline over a bounded cache, or anything shaped like an HLS media playlist {{HLS}}. Records are appended at the tail and removed from the head as the content they describe becomes unavailable.

This document specifies how each is encoded onto a track.
The common design is that a group is self-contained: its first frame carries enough state to start from, and later frames in the group are incremental.
A subscriber joins at the latest group and needs nothing older, so relays and publishers can drop older groups without breaking new subscribers.


# Terminology
This document is written using moq-lite {{moql}} terminology and applies equally to moq-transport {{moqt}}:

| moq-lite | moq-transport |
|:---------|:--------------|
| Broadcast | Track Namespace |
| Track | Track |
| Group | Group |
| Group Sequence | Group ID |
| Frame | Object |
| latest group | Largest Group ID |

Every encoding in this document depends on in-order delivery of the frames within a group.
Over moq-transport, a publisher MUST send all Objects of a group on a single Subgroup, with Object IDs assigned sequentially from 0 and no gaps.
Datagrams MUST NOT be used.

A subscriber joins at the latest group: the default in moq-lite, or a Largest Object/Group filter in moq-transport.


# Common Rules
Each frame decodes to exactly one JSON value {{JSON}} encoded as UTF-8.
The frame boundary is the value boundary: there is no delimiter inside a frame and no value spans frames.

A receiver MUST ignore unknown members of the JSON objects defined by this document, so that encodings remain extensible.

A track uses exactly one of the three encodings below.
Which encoding (and whether [Compression](#compression) applies) is communicated out-of-band, typically by the application's catalog or a [naming convention](#track-naming).


# Snapshot Tracks
A snapshot track carries a single JSON value updated over time.
Only the most recent value matters; a subscriber that falls behind skips ahead.

Each group is self-contained:

- Frame 0 is the complete value.
- Each subsequent frame is a JSON Merge Patch {{MERGEPATCH}}, applied to the result of the previous frame.

A subscriber reads the latest group, decodes frame 0, and applies each following frame in order.
When a newer group arrives, the subscriber switches to it and starts over from its frame 0; groups older than the newest carry no information the newest lacks.

A publisher starts a new group when the update cannot be expressed as a merge patch: the root value is not an object, or a member's new value is a genuine `null` (which a merge patch cannot represent, since `null` means removal).
A publisher SHOULD also start a new group once the patches written to the current group exceed the size of its snapshot frame, bounding the work a new subscriber replays, and MUST start one before exceeding any transport limit on frames per group.


# Stream Tracks
A stream track carries an ordered, append-only log.
Every record is preserved and delivered in order.

The entire log is a single group.
Each frame is one record, appended to the group as it is produced.
A publisher MUST NOT start a second group.

Because a subscriber always starts at frame 0, the log's history is bounded by how long the publisher and relays retain the group's early frames.
Once they are gone, new subscribers cannot join the track.
A stream track is therefore best for short-lived or bounded logs; a log that must outlive the cache should use a [window track](#window-tracks) instead.


# Window Tracks
A window track carries an ordered log that grows at the tail and shrinks at the head, like an HLS media playlist {{HLS}}: records are appended as content is produced and removed as the content they describe expires.

Every record occupies a stable **position**: the count of records ever appended before it.
Positions start at 0 and never repeat, so a record is identified by its position across groups, group switches, and removals.
This is the same role the media sequence number plays in HLS.

Each group is self-contained.
Frame 0 is a snapshot of the current window:

~~~
{
  "offset": 1042,
  "values": [ ... ]
}
~~~

**offset**:
The position of the first record in `values`: the total number of records removed from the head over the lifetime of the log.
Required, 0 at the start of the log.
It MUST be a non-negative integer, and a receiver MUST reject a snapshot whose offset is absent or not one: every position derives from it, so a bad offset silently corrupts them all rather than failing.
It MUST NOT exceed 2^53-1, so a receiver using IEEE 754 doubles (a JSON parser's usual number type) counts exactly.

**values**:
Every record currently in the window, oldest first.
Required, and may be empty.
The sum of `offset` and the number of values MUST NOT exceed 2^53-1.
A receiver that has already delivered records MUST reject a later snapshot whose end precedes the next position it expects, since accepting it would reuse positions and silently discard subsequent records.

Each subsequent frame carries exactly one operation:

~~~
{ "append": <value> }
~~~

**append**:
Adds one record at the tail of the window.

~~~
{ "trim": 3 }
~~~

**trim**:
Removes the given number of records from the head of the window and advances the effective offset by the same amount.
The count MUST be a non-negative integer, MUST be greater than 0, and MUST NOT exceed the number of records currently in the window.

A publisher MUST NOT write both members in one frame.
A receiver that sees both MUST treat the frame as malformed, since applying them would append and retract in an order this document does not define.

A frame carrying *neither* member is not an error: that is how an operation introduced by a later revision appears to a receiver that predates it, and per [Common Rules](#common-rules) unknown members are ignored.
A receiver MUST skip such a frame without changing the window.

## Publishing
A publisher appends a record the moment it becomes valid and trims the moment the head records become invalid, so the live window is always current.

A publisher SHOULD start a new group once the records trimmed since the group's snapshot outnumber the records still in the window: the group is then mostly dead weight for a new subscriber, and a fresh snapshot costs no more than what has already been trimmed.
This bounds the total bytes at roughly twice an append-only log.
A publisher MUST start a new group before exceeding any transport limit on frames per group.

The new group's snapshot reflects the window at that moment; the previous group SHOULD be finished (closed cleanly).
Old groups carry no information the newest lacks, so publishers and relays MAY drop them at any time.

## Consuming
A subscriber reads the latest group, decodes the snapshot, and applies each operation in order.
When a newer group arrives, the subscriber switches to it, even mid-group.

Positions make the switch lossless: a subscriber that has processed records up to position `p` skips the new snapshot's records at positions at or below `p` and continues from there.
If the new snapshot's `offset` is greater than `p + 1`, the intervening records were trimmed before the subscriber saw them; whether that matters is up to the application (for a timeline it is a gap; for a cache index it is nothing).


# Compression
Any of the encodings can be compressed with group compression {{flate}}, whose unit of compression is also the group: every frame of a group shares one DEFLATE window.
The fit is deliberate.
Frame 0 (a snapshot, or the first records of a log) seeds the window, and every later frame compresses against it, so repetitive values shrink sharply while each group stays independently decodable.


# Track Naming
This section is a convention, not a requirement; the application's catalog is authoritative.

A plaintext JSON track SHOULD use a name ending in `.json`.
A compressed track appends `.z` per the convention of {{flate}}: `catalog.json.z`, or `video.timeline.z` for a track whose plaintext suffix is implied by the application.
A publisher MAY publish plaintext and compressed siblings carrying the same content, letting each subscriber pick.


# Security Considerations
The considerations of the underlying transport ({{moql}} or {{moqt}}) apply, as do those of {{flate}} when a track is compressed.

JSON parsing is a well-known attack surface.
Receivers SHOULD bound the size and nesting depth of the values they accept, and treat all content as untrusted application data.

A malicious publisher of a window track can lie in either direction: advertising records for unavailable content, or trimming records for content that still exists.
A subscriber MUST treat the window as a hint about availability, not a guarantee.


# IANA Considerations
This document has no IANA actions.


--- back

# Acknowledgments
{:numbered="false"}

The snapshot-per-group structure follows the catalog and media timeline design of {{msf}}, extended here with explicit head removal.
