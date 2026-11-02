---
title: Archives
description: Record every listed track into an object store and serve it back as a broadcast
---

# Archives

`moq-archive` records a broadcast into any
[`object_store::ObjectStore`](https://docs.rs/object_store), then serves the
stored prefix back as an ordinary [moq-lite](/concept/moq-lite) broadcast.
It records tracks without interpreting their payloads, so a catalog, media,
chat, and application data follow the same path. No catalog or archive pointer
is needed to record or serve one.

Use [`moq export archive` and `moq import archive`](/bin/cli#archive) for a
ready-made host, or embed the Rust
[`Producer`](https://docs.rs/moq-archive/latest/moq_archive/struct.Producer.html) and
[`Consumer`](https://docs.rs/moq-archive/latest/moq_archive/struct.Consumer.html).
The host owns broadcast discovery, storage credentials, recording prefixes,
retention policy, and the paths where recordings are published.

## Discover and record

The host follows ANNOUNCE to find broadcasts under a path prefix and runs one
`archive::Producer` per broadcast. An announcement can also advertise a service
prefix without enumerating its broadcasts, so it is not an inventory of every
path that service could answer. See [discovery](/concept/moq-lite#discovery).

Each producer uses TRACKS to discover that broadcast's tracks, including ones
added later, and opens an ascending SUBSCRIBE for each. TRACKS lists tracks the
publisher exposes, not tracks served only on request. It does not select from
the media catalog or follow its references into other broadcasts: the host
must record those broadcasts separately too.

Across our publishers and relays, track discovery needs **moq-lite 07
(`moq-lite-07-wip`, opt-in) on every hop**. Older moq-lite versions cannot list
tracks, and our moq-transport publishers refuse SUBSCRIBE_TRACKS. Rust can list
a third-party moq-transport publisher that supports that request, but that
does not make a moq-transport hop through our publishers support recording.
Choose the protocol versions at setup; see [transport](/concept/transport).

Recording starts with content the subscription can still receive. A missing
group is given up after the publisher's max age capped by the producer's max
delay, so a gap cannot stall later groups. Datagrams are uncached and can only
be recorded from when the subscription takes effect. An archive cannot recover
content already lost or evicted before it subscribed.

Pick a distinct store prefix for each publisher instance, such as its broadcast
path and [epoch](/concept/identity). Restarting a recorder on the same instance
and prefix resumes after each track's last stored frame; a prefix already marked
complete records nothing. Never append a new publisher run that restarts its
group numbers to an old prefix. `moq export archive` makes this choice for you.

## What the store holds

Tracks have separate directories, with each track name percent-encoded as one
segment. Within one broadcast prefix:

- Each track's `.info` stores its timescale, when timed, and priority.
- Data objects are keyed by their first group, frame index, and presentation
  timestamp; untimed tracks omit the timestamp. The sorted key listing is the
  index, with no manifest or timeline object.
- Each track's `end` records its final position or stream error. The broadcast's
  `.end` marks the recording complete, after its track markers are written.

Objects coalesce content by duration, using media time for timed tracks and
wall time for untimed ones. A flush deadline also stores idle tracks, splitting
long groups between frames when needed. Groups, frame positions, timestamps,
payloads, and the distinction between groups and datagrams survive storage.
With DVR pruning enabled, old superseded objects are deleted after the retention
window and grace; the newest object of each track stays.

See the [moq-archive rustdoc layout](https://docs.rs/moq-archive/latest/moq_archive/#layout)
for the format. For this checkout's layout and recording behavior, start at
[`rs/moq-archive`](https://github.com/moq-dev/moq/tree/main/rs/moq-archive).

## Serve from a host

Create an output broadcast, call `archive::Consumer::open` with the stored
prefix and that broadcast's producer, then announce its publication and run the
consumer. Opening creates the stored tracks before the host announces them.
The host chooses the output path and a fresh epoch, as
[`moq import archive`](/bin/cli#archive) does. A relay routes that publication
like any other broadcast; it does not look in object storage on a cache miss.

Each track starts live at its newest stored sequence and serves older groups
from storage when requested by group position. It advertises no max age, so the
source's live retention cannot expire archived history. A growing recording is
periodically listed for new tracks and objects. Once `.end` appears, refreshing
stops and tracks end as recorded; without it, including when a recorder stopped
early, the archive stays live. Empty or unrecognized prefixes are refused.

Serving history is not paced playback from the beginning. Datagrams remain
lossy datagrams, never fetchable groups. Time ranges currently resolve against
the served track's cache, not the archive's stored keys, so they cannot reliably
seek old stored content. There is no automatic switch from a live broadcast to
its archive at the original path.

The archived catalog is an ordinary track. [HLS](/bin/hls) can serve a recording
whose renditions include their `hls` record tracks, just as it serves a live
broadcast; a rendition without those records gets no playlist. The catalog's
per-rendition [`hls`](/concept/hang#catalog) field describes HLS segmentation,
not archive discovery. Protected broadcasts are outside this archive workflow.
