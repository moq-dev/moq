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
[`Producer`](https://github.com/moq-dev/moq/blob/main/rs/moq-archive/src/producer/mod.rs) and
[`Consumer`](https://github.com/moq-dev/moq/blob/main/rs/moq-archive/src/consumer/mod.rs).
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
tracks, and our moq-transport publishers refuse SUBSCRIBE\_TRACKS. Rust can list
a third-party moq-transport publisher that supports that request, but that
does not make a moq-transport hop through our publishers support recording.
No default version set includes it, so both ends of each hop must opt in: the
publishing and recording clients with `--connect-version moq-lite-07-wip`, and
each relay by listing it in [`listen.version`](/bin/relay/config#listen), plus
`connect.version` for its cluster links. A relay left at its default versions
cannot negotiate moq-lite 07.

Recording starts with content the subscription can still receive. A missing
group is given up after the publisher's max age capped by the producer's max
delay, so a gap cannot stall later groups. Datagrams are uncached and can only
be recorded from when the subscription takes effect. An archive cannot recover
content already lost or evicted before it subscribed. The same max delay
(`producer::Config::max_delay`, 10 s by default and in `moq export archive`)
bounds the backlog: content further behind the live edge is skipped, even when
the publisher still caches it.

Pick a distinct store prefix for each publisher instance, such as its broadcast
path and [epoch](/concept/identity). Restarting a recorder on the same instance
and prefix resumes after each track's last stored frame, within that max delay:
an interruption longer than it leaves a gap. A prefix already marked complete
records nothing. Never append a new publisher run that restarts its
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
window and grace; the newest object of each track stays. Pruning runs only while
the recorder runs: once a recording completes or its recorder stops, objects not
yet due stay until something else deletes them, such as a store lifecycle rule.

The [moq-archive rustdoc](https://docs.rs/moq-archive) specifies the format for
the published version. This page follows the
[layout documented in current source](https://github.com/moq-dev/moq/blob/main/rs/moq-archive/src/lib.rs#L8),
which can differ from an installed package; use the reference matching your version.

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
