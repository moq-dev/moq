---
title: Stats
description: The stats broadcasts a relay publishes, their tracks, and their JSON encoding
---

# Stats

A relay publishes its traffic counters as ordinary MoQ broadcasts, so any
subscriber can read them: a dashboard, a billing meter, an aggregator in
another region. This page is the wire contract for those broadcasts, enough to
read them in any language. [`moq-stats`](https://docs.rs/moq-stats) is the Rust
producer and consumer, [`@moq/stats`](https://github.com/moq-dev/moq/tree/main/js/stats)
the JavaScript consumer (node discovery, typed frames, and bucket runs followed
through the track listing), and the relay's [`[stats]`](/bin/relay/config#stats)
section turns it on.

## Media reports

Media reports use hang's catalog discovery, separately from a relay's
traffic broadcasts below. `stats: { track }` names a publisher's snapshot;
`echo: { path }` solicits a viewer's `feedback.json` snapshot on an
application-named `.echo` broadcast. Both snapshots map the catalog's
rendition IDs to their counters and carry optional transport measurements.
The publisher reports only renditions it writes; a viewer reports a
referenced rendition under its ID in the catalog that listed it.

The shared schemas are `hang::{stats, echo}::Snapshot` and
`Hang.Stats.SnapshotSchema` / `Hang.Echo.SnapshotSchema` in `@moq/hang`.
Container-specific sections can sit beside `transport` and `renditions`.
Counters are cumulative; readers derive rates over an interval and treat a
replacement or decrease as a reset. Bitrate, latency, RTT, estimated rate,
and sample age are gauges. Loss is cumulative lost bytes and packets,
with unavailable transport measurements absent. Missing rendition counters
mean zero, and JavaScript preserves omission. A known-zero gauge or transport
measurement stays explicit, so it remains distinct from unavailable.

`moq import --report` and `moq export --echo` report them from the
[CLI](/bin/cli#reports-and-feedback), and `moq import --echo-path` solicits
feedback. An import counts the frames and payload
bytes it writes to each rendition, and video keyframes; an export counts what
it reads and records its newest arrival. In the browser, `<moq-publish stats>`
and `echo-path` do the same for the [publisher](/lib/js/publish#stats-and-feedback),
and `<moq-watch echo>` reports what its [player](/lib/js/watch#feedback)
received, decoded, and played, with only the PROBE round-trip time as its
transport. See the
[hang draft](/draft/moq-hang#media-stats) for track framing, field units, and
the feedback convention.

### MPEG-TS sections

A TS import or export adds sections of its own, which
`moq_mux::container::ts::stats::Report` reads. As above, counters are
cumulative and omitted at zero, and a gauge is omitted until it is measured.

`mpegts` holds `streams`, one row per elementary stream keyed by its PID in
decimal (plus a `.si` row for an SI PID that dropped sections), and the
stream-wide counters. A row has `track` (the track suffix),
`class` (`audio`, `video`, or `data`; an unknown class reads as `data`),
`units` (access units delivered), and `quiet`, a gauge: milliseconds of
program clock since its last access unit. Its losses are `resyncs`, bytes
`discarded` scanning for audio sync, `unconfirmed` frames, `damaged` units,
`unplaced` units the program clock could not place, the times the stream was
`refused`, and its shares of `continuityCountError`, `transportError`, and
`ptsError`. A TS-scrambled packet on a carried stream counts as `refused`.

Every PID but the null PID either takes a route or is counted. A listed stream
the import cannot carry, such as `0x86` without a SCTE-35 registration, gets a
row with an empty `track` and one `refused`. Once every program's PMT is in, a
PID that no PAT, PMT, or captured SI table claims, such as the CAT, conditional
access, or ATSC PSIP, gets a `data` row with an empty `track` and `refused`
counting its packets, with one warning when it starts. An import of one `--program` of a
multi-program stream cannot tell another program's PIDs from these, so it
counts none of them.

The stream-wide counters count ETSI TR 101 290 checks at that standard's
fixed limits:

| Field | Check |
| --- | --- |
| `tsSyncLoss` | 1.1 `TS_sync_loss` |
| `syncByteError` | 1.2 `Sync_byte_error` |
| `patError` | 1.3 `PAT_error_2` |
| `continuityCountError` | 1.4 `Continuity_count_error` |
| `pmtError` | 1.5 `PMT_error_2` |
| `transportError` | 2.1 `Transport_error` |
| `crcError` | 2.2 `CRC_error`, on the PAT and PMT only |
| `pcrRepetitionError` | 2.3a `PCR_repetition_error` |
| `pcrDiscontinuityIndicatorError` | 2.3b `PCR_discontinuity_indicator_error` |
| `ptsError` | 2.5 `PTS_error` |

An import grades the stream as it arrived. An export builds every header
itself and runs no checks, so its counters stay zero and only its rows'
`units` and `quiet` move. Neither grades the wire a receiver sees downstream.
The checks grade the encoder's insertion, not the network's delivery: a forward
PCR step over 100 ms counts as both PCR errors, and a missing or frozen PCR
counts once per outage, clearing when the PCR advances again. The table, PCR
and PTS intervals run on a health clock that follows the PCR and, across a
stall, the packets the multiplex carries at the constant rate its PCR measured,
so a file read instantly grades the same as a live feed, however either is
batched. Until a constant rate is measured, or on VBR input, the clock falls
back to arrival once each input chunk is read, which includes upstream delays.
A fully stopped input adds no time, by timer or at EOF: the PCR's step when it
resumes counts an encoder outage. 1.6 `PID_error` is covered by each
row's `units` and `quiet`: a dead video path behind a live mux still sends
PCR-only packets on its PID, which a packet count reads as live.
`PCR_accuracy_error` is not measured. No verdict is computed: a sparse stream
such as SCTE-35 is quiet for seconds by design, so an alarm reads counter
rates and `quiet` against its own thresholds.

An export also reports `release`, how it keeps up with the source: what it
drops, trims and never receives, and how its output clock tracks the
source's. `export ts --passthrough` reports it without `mpegts`.

| Field | Meaning |
| --- | --- |
| `dropped` | Frames lost from the output after their stream began: missed their release deadline, or a video track dropped them waiting for its next keyframe. |
| `trimmed` | Frames dropped before their stream's first went out, so the output starts on a keyframe. Nothing is missing for these. |
| `gaps` | Times the source skipped media it will never deliver: a group lost or expired upstream, or a resubscription. A declared break is not one. |
| `margin` | Gauge, milliseconds: the least time any frame arrived before its deadline over the last second. Negative when one missed. |
| `offset` | Gauge, milliseconds: how far the output clock sits from the source's plus the delay. Positive holds frames longer than the delay. The clock pulls it shut over ten minutes. |
| `drift` | Gauge, parts per million: the source clock's rate against ours, positive running fast. |
| `outOfTolerance` | Measurements that found the source clock past the 30 ppm ISO/IEC 13818-1 allows, which the output cannot follow. |

### Health

`hang::health` and `Hang.Health` in `@moq/hang` grade a client from two
consecutive reports: `publisher` takes a pair of `stats` snapshots, `viewer` a
pair of `.echo` snapshots. Each report names the broadcast it was read from
(its path and identity), which observed every number in it; the caller states the
interval between the two and how long ago the newer one arrived, as a `Duration`
in both languages. Both languages compute the same result from the shared
fixtures in `rs/hang/fixtures/health.json`.

The result holds the deltas over the interval (frames, bytes, and bitrate per
rendition; lost bytes and packets for the connection) and a verdict for the
connection and each rendition: `unknown`, `healthy`, `degraded`, or
`unhealthy`. Each verdict carries its evidence, every input with its value and
grade, and is the worst graded input. An input the report cannot show is
listed as `unknown`, and a verdict with nothing graded is `unknown`: a browser
publisher that knows only its PROBE RTT grades on RTT alone. A report older
than the stale limit is `unknown` throughout, as is a transport sample whose
`age` plus the report's exceeds it, and a client with no report has no sample at all. A changed
path or identity (`replaced`) or a decreasing counter (`decrease`) is a reset:
the deltas start over, so only gauges are graded until the next pair.
Container sections such as `mpegts` are not read.

| Input | Role | Value | Degraded | Unhealthy |
| --- | --- | --- | --- | --- |
| `rtt` | both | Round-trip time, ms. | 300 | 800 |
| `loss` | publisher | Transport bytes lost per media byte sent. | 0.02 | 0.1 |
| `headroom` | publisher | Estimated `rate` over the media bitrate sent. | below 1.5 | below 1 |
| `skipped` | publisher | Frames skipped, as a share of frames sent or skipped. | 0.01 | 0.05 |
| `late` | viewer | Late frames per frame received. | 0.01 | 0.05 |
| `errors` | viewer | Decode errors per frame received. | 0.01 | 0.05 |
| `stalled` | viewer | The share of the interval spent stalled. | 0.01 | 0.05 |

These are defaults the caller can override, as is the five-second stale limit.
A viewer's loss counters describe its own uplink, so they are reported but not
graded. Loss and headroom divide the connection's transport counters by the
report's own media, so they hold when the broadcast is the only one its
connection publishes. The transport limits were measured on one paced TS feed
through a shaped link over five-second intervals. The rendition limits are not
measured against real traffic, and no CLI viewer reports `late`, `errors`, or
`stalled`.

A publisher whose link falls below its media rate sends no report at all,
since its report track sits below every media track. Its health goes stale
and reads `unknown`; only its viewers' playout counters can show the damage.

## Broadcasts

Each node publishes one broadcast under a prefix, `.stats` by default, which
[moq-lite](/concept/moq-lite) hides from announce listings that don't ask for
it. Traffic under the prefix is never counted, so serving stats doesn't
generate more stats.

```text
<prefix>/node/<node>
```

- `<node>` tells relays sharing a cluster apart. It may span several segments
  (`sjc/1`), and is omitted along with its slash when unset: `<prefix>/node`.
- The literal `node` segment leaves room for sibling categories under the same
  prefix, so a consumer skips any path without `node` where it expects one.

The broadcast stays announced for the producer's life. Each announcement
carries a fresh [identity](/concept/moq-lite#publisher-identities) on its
route, so a restarted node is a new broadcast at the same path: it replaces the
old one, and no relay serves it groups cached under the old identity. A relay
taking over through [handoff](/bin/relay/config#handoff) announces its broadcast
only once the relay it replaced exits, so a node has one identity at a time;
its counters run from its start, and its first publish carries them. A reader
treats each identity as its own set of counters. The identity rides moq-lite 07
announcements; over anything else the receiving session mints one per
announcement, so a reader sees the same change as an end and a start at the path.
A minted identity also changes on every reconnect, so it cannot tell a restart from a
redial. When a minted identity follows a minted one while the departed one is still
within the aggregating reader's grace ([`Config::grace`](https://docs.rs/moq-stats/latest/moq_stats/aggregate/struct.Config.html#structfield.grace)),
its first frame decides: counters that continue are the same node, and a new bucket
epoch or a counter going down is a restart, which adds to the old counters. A return
after the grace starts over, so its earlier traffic counts twice.
An aggregating reader pins each node's subscription to its identity.

Every bucket shares the node's broadcast, so a token scoped to the stats prefix
reads every bucket's tracks; scope by bucket with an aggregating reader instead.

### Root broadcasts

A relay with [`roots`](/bin/relay/config#stats) on also gives each root its own
stats, readable with a grant on that root alone. A session scoped to a root reads
them at the same `<prefix>/node/<node>` paths, relative to its root, carrying the
same tracks:

- traffic on the broadcast paths at or under the root, named relative to it, and
- the sessions and HTTP requests scoped to the root or to a root beneath it.

Nothing of another root's sessions or paths appears, and reading them counts as
no root's traffic. Traffic another root's session causes on the root's paths,
such as reading them through a grant's mount, counts on those paths; what the
root's own sessions read through a mount counts on the mount's target instead.
Each node announces a root's broadcast while it serves a session scoped to
exactly that root, plus the linger, so a returning root is a new identity.

The relay publishes each root's broadcast at
`<prefix>/root/<root>/node/<node>`, with the root escaped into one segment, and
mounts `<prefix>/root/<root>` read-only at `<root>/<prefix>`: no session can
publish there, and the top-level node broadcast is the empty root's.

## Tracks

Traffic is split by **tier**, an arbitrary label (a billing class, a region)
the relay takes from the auth grant, or from `--cluster-tier` for links it
dials and LAN peers it admits. The default tier's label is empty. Every track
but a bucket's comes in two encodings:

| Track | Frame keyed by | Entry |
| --- | --- | --- |
| `totals.json` | tier | [Totals](#totals) for the whole node |
| `[<bucket>/]bucket.<epoch>.json` | | [Bucket](#bucket): one bucket's totals, paths, and labels, every tier |
| `[<label>/]presence.json` | tier | [Presence](#presence): sessions under one stats label |
| `[<path>/]prefix.json` | tier | [Prefix](#prefix) traffic under one broadcast prefix |

Every track uses the one [encoding](#encoding). `totals.json` always exists.

### Bucket tracks

The relay sorts traffic into buckets by the first `depth` segments of each
broadcast path, and sessions by the first `depth` segments of each stats label;
a path shorter than `depth` buckets under all of its segments. Each counter run
of a bucket is a `<bucket>/bucket.<epoch>.json` track (`bucket.<epoch>.json` for the
empty bucket, the only one at depth 0), named by the run's `epoch` and carrying
all of the bucket's tiers, both directions, and its sessions, so a reader holds
one subscription per bucket.

A reader discovers the live buckets from the broadcast's track listing (TRACKS
in moq-lite 07). At depth 0 the one bucket's track is listed for the producer's
life. At depth 1 or more, a bucket's track is listed from its first entry until
it has lingered empty (five minutes by default), then unlisted and ended. A
bucket that returns within the linger keeps its track and epoch, and its totals
continue. The node's announcement never changes, so bucket churn costs no
announce traffic across a cluster. Over a hop that cannot carry a listing
(moq-lite 06 and older, or moq-transport, whose `SUBSCRIBE_TRACKS` our relay
refuses) the listing is empty or refused. A bucket's track name carries its
run's epoch, so a reader finds it only through the listing; one that already
knows the name can subscribe while it is listed, and is refused otherwise.

A bucket that returns after its linger is a new counter run under a new track:
its frames carry a new `epoch`, which also names the track, and count from
zero. No track name is ever reused, so each run numbers its groups from 0, and
group 0 is where its counters start, for every reader and relay cache. Holding
a bucket's track doesn't count as an entry, so a reader follows the listing to
the next run. A frame too large to cache ends only that bucket's track.

### Requested tracks

A presence track exists only while subscribed. Ask for `<label>/presence.json`
for any stats label (`presence.json` for the empty label). The relay answers
with the label's current sessions and counts from there, holding a label with
no sessions open at `{}`, and stops producing it once the last subscriber
leaves. A prefix track works the same way for any broadcast prefix: ask for
`<path>/prefix.json` (`prefix.json` for the empty prefix). Any other name is
refused.

## Frames

Every frame is a JSON object mapping a key to an entry, except a
[bucket](#bucket) track's. A track with no entries holds `{}`.

The producer drains its counters every interval (one second by default) and
writes only when a track's frame changed, so silence means nothing moved, not
that the producer is gone. The track ends when the producer does, or when its
bucket track is unlisted; the bucket may return later as a new track instance. A
frame too large to cache (32 MB) ends its track with a `FRAME_TOO_LARGE` error,
measured by its uncompressed size, since a reader inflates the whole group.

Outside bucket tracks, which number their groups from 0, a group's number is
its publish's scheduled time divided by the interval, so the interval is its
timescale. Publishes are scheduled at least an interval
apart, even after a late one, so each opens its groups in an interval of its
own, and a track recreated later numbers its groups past every group its
earlier identity wrote.

### Totals

```json
{
  "": { "publisher": { "bytes": 1048576, ... }, "subscriber": { ... }, "sessions": { ... } },
  "region/sjc": { ... }
}
```

Each tier that recorded anything in the node's run, with its egress (`publisher`)
and ingress (`subscriber`) [Traffic](#traffic) and its [Presence](#presence),
summed over every broadcast and stats label. Unlike a bucket's paths and labels,
an entry that ends stays counted, so the next frame always carries everything
since the run began: a reader that misses frames loses nothing while the
node stays announced. Billing reads this track.

### Bucket

```json
{
  "epoch": "0192f4e6-7b8a-7c3d-9e1f-2a3b4c5d6e7f",
  "tiers": {
    "": {
      "publisher": { ... }, "subscriber": { ... }, "sessions": { ... },
      "paths": { "acme/live": { "publisher": { ... } } },
      "labels": { "acme": { ... } }
    }
  }
}
```

`epoch` is a UUIDv7 minted for each identity of the bucket's track, and a
reader keeps each epoch's counters apart, as it does for a broadcast's identities.
`tiers` maps each tier the bucket recorded on since then. Its top-level
`publisher`, `subscriber`, and `sessions` are the bucket's [totals](#totals), so
a totals reader parses them unchanged, and like the node's totals they keep
every entry that ended. A lingering bucket's frames repeat its final totals: a
reader that misses every frame from the bucket's last change until it is
unlisted loses that identity's tail, so a meter reads the node's totals.

`paths` holds each broadcast path's egress (`publisher`) and ingress
(`subscriber`) [Traffic](#traffic). A path appears from its first nonzero
counter until the relay drops its counters, even while idle: the last viewer
can leave while the publisher still holds the path, and a returning viewer
resumes the same counters. So a path missing from a frame had no counters, and
one that returns starts from zero; the totals keep what it counted. A side that
has never moved is left out and reads as zero.

`labels` holds each stats label's [Presence](#presence), while a session or
HTTP request holds its context and on the tick its last one ends. While a bucket
lingers empty, its `paths` and `labels` are empty.

### Per-label tracks

A presence track's entries are cumulative from when it was first held: a
session that ends stays counted, and so does every one after it, while
anyone holds the track. A track held again starts over.

Wire bytes start at zero on every hold, including the first. Other presence
fields seed from the root's current live-session counters; they do not recover
closed roots from before the hold. Node totals keep the full run's
history. Each recreated track starts a fresh snapshot group numbered by its
publish interval, past any group of an earlier hold, so relay caches can
distinguish the two.

### Prefix

```json
{
  "": {
    "publisher": { ... }, "subscriber": { ... },
    "self": { "publisher": { ... }, "subscriber": { ... } },
    "children": { "live": { "publisher": { ... }, "subscriber": { ... } } }
  }
}
```

Each tier the prefix recorded on, with three views of its [Traffic](#traffic):
the top-level `publisher` and `subscriber` roll up every broadcast at or under
the prefix, so a [totals](#totals) reader parses them unchanged; `self` is the
broadcast at exactly the prefix, apart from any nested under it; and
`children` rolls up each direct child, keyed by its next path segment.

Like presence, the rollup and `self` are cumulative from when the track was
first held: each frame sums the live broadcasts' counters plus the final
counters of those that ended since. A broadcast that was live before the hold
carries its history from before it, so absolute values are not authoritative:
take differences within one subscription, and read the [totals](#totals) for
real sums. Started minus ended stays exact. A child appears while a broadcast
under it has counters, and leaves after the frame carrying its last one; its
tail stays in the rollup, and a child that returns starts over. Children are
never capped, so roughly ten thousand across four tiers outgrow the 32 MB frame
limit: the track then ends with `FRAME_TOO_LARGE` instead of going stale.

## Entries

### Traffic

```json
{
  "acme/live": {
    "announces_started": 1, "announces_ended": 0, "announced_bytes": 9,
    "broadcasts_started": 3, "broadcasts_ended": 1,
    "subscriptions_started": 6, "subscriptions_ended": 2,
    "fetches": 0,
    "bytes": 1048576, "frames": 900, "groups": 30, "datagrams": 0,
    "inflated": 0,
    "stale": { "bytes": 0, "frames": 0, "groups": 0 }
  }
}
```

| Field | Counts |
| --- | --- |
| `announces_started` / `announces_ended` | Announces and unannounces of the broadcast. |
| `announced_bytes` | The broadcast name's length, summed over each announce and unannounce. Not part of `bytes`. |
| `broadcasts_started` / `broadcasts_ended` | On egress (`publisher`), a session's first subscription to the broadcast and its last one closing. Started minus ended is the viewer count. Ingress (`subscriber`) leaves both at zero: it does not count viewers. |
| `subscriptions_started` / `subscriptions_ended` | Track subscriptions opened and closed. |
| `fetches` | One-shot group fetches requested, including ones that found nothing. Their payload counts in `bytes`, `frames`, and `groups`. |
| `bytes` / `frames` / `groups` | Payload delivered. `bytes` includes datagram payloads. |
| `datagrams` | Datagrams of datagram tracks delivered. A datagram belongs to no group, so it counts in neither `groups` nor `frames`. |
| `inflated` | Bytes inflated from compressed groups for plain readers, counted once per group however many share the copy. Not part of `bytes`, which counts payload as stored or sent. |
| `stale` | Payload skipped because it aged past a subscriber's latency budget, with `bytes`, `frames`, and `groups`. Disjoint from the top-level payload counters. |

### Presence

```json
{
  "acme": {
    "requests": 3, "sessions_started": 12, "sessions_ended": 10,
    "sent_bytes": 1048576, "received_bytes": 524288,
    "sessions_refused": { "refused": 0, "unavailable": 0, "request": 0, "forbidden": 0, "membership": 0, "expired": 0 },
    "sessions_failed": { "idle_timeout": 1, "transport": 0, "application": 0, "expired": 0 },
    "announces_peak": 40, "subscriptions_peak": 900,
    "wire_tx": 2048, "wire_rx": 1024, "credit_stalls": 2, "credit_stalled_us": 350000,
    "credit_held_readers_us": 120000, "pacing_blocked_us": 40000,
    "routes_refused": 0,
    "routes": {
      "changed": 4, "rewritten": 120,
      "sent": { "dead": 1, "drain": 0 }, "received": { "dead": 0, "drain": 1 },
      "refused": { "path": 0, "scope": 0, "looped": 0 },
      "closed": { "stalled": 0, "overrun": 0, "malformed": 0, "role": 0, "unasked": 0, "other": 0 }
    }
  }
}
```

`sessions_started` and `sessions_ended` count connects and disconnects, whether
or not any data flows: under a stats label on the tier for a bucket's `labels`,
under the label on each tier for `presence.json`, and under every label in the
bucket or node on the tier for its totals. A session moved to a new tier ends on the old one and starts
on the new. `sent_bytes` and `received_bytes` count the payload this node sent
to and received from those sessions and HTTP requests, the same bytes a
bucket's `paths` count. A changed label moves later presence and bytes; bytes
before the change remain under the old label. Older readers ignore these fields
and newer readers default missing fields to zero.

The label is the grant's `label`, a path the auth server picks to attribute
usage, independent of the root that scopes what the session may access. A grant
without a `label` counts under the empty label, never under its root.

`wire_tx` and `wire_rx` count transport bytes sent and received, separately
from `Traffic.bytes`, which counts media payload. QUIC and WebTransport count
UDP bytes, including retransmissions and transport overhead. The transport
updates a connection's counters as it sends and receives, and a stats tick reads
them without polling the session or activating its periodic sampler. A retag
flushes the old tier and label first; closing a session records its final delta.
An embedder creates a `transport::Wire`, registers it with `stats::Session::wire`,
records its connection's cumulative totals on it, and holds the returned guard.
Dropping the guard records the final reading.
`moq-srt` fills one per connection through the `wire` field of its `Import` and
`Export`: SRT packet bytes, headers and retransmits included, without IP and
UDP, refreshed every 250 ms, so a connection's last quarter second goes uncounted.

A qmux session over TCP, which is every WebSocket session, counts the kernel's
`TCP_INFO` bytes for its socket. `wire_tx` includes TLS, WebSocket, and qmux
framing and TCP retransmits, and excludes TCP and IP headers. `wire_rx` is the
bytes TCP delivered, so a peer's duplicate retransmissions are not counted. The
session copies them at most four times a second, and once more when it ends.
Only Linux (4.19 or later) reports them: a session on another platform, or on a
socket the relay holds no handle for, contributes zero.

Other unsupported transports, including browsers, contribute zero. Unavailable
counters are never estimated from payload. Readers default missing wire fields
to zero for older relays.

`wire_lost` counts the sent bytes QUIC declared lost. It lands on the label and
tier current when the loss is declared, so a session retagged in between moves
loss away from the row that counted those bytes in `wire_tx`; across a whole
node, the ratio of the two is the loss rate. `cwnd_blocked_us` and `pacing_blocked_us` count
the microseconds a connection had data ready to send held back by its congestion
controller: by a full congestion window, or by the pacer spreading sends at the
controller's rate. Time growing here points at the path setting the pace,
though a peer's stream-level credit can still hold other streams back. Native QUIC and WebTransport report them, at the
same lag as the credit counters below; other transports contribute zero.

`credit_stalls` and `credit_stalled_us` count how often, and for how many
microseconds, a connection had a write blocked with all of the peer's
connection-level flow-control credit (QUIC `MAX_DATA`) spent. An episode starts
when a write blocks with every credited byte sent and ends when the peer grants
more, the writer gives up, or the connection closes; a write held only by this
node's own send buffer is not one. A rising count means the peer is not reading
or not granting credit, as opposed to loss or congestion. Native QUIC,
WebTransport, and Rust qmux sessions report them through the same wire counters
as `wire_tx`; other transports contribute zero. A wait in progress counts up to
the transport's latest step, at most a keep-alive behind.

`credit_held_readers_us` and `credit_held_evicted_us` are the receiving side of
the same wait: the microseconds a peer had a connection's whole receive window
spent while this node evicted nothing to free it. `readers` counts while a
reader had data it had not looked at yet, or waited behind a gap, so a reader
running behind holds the window. `evicted` counts while an evicted reader had
yet to stop its stream, keeping the credit of the bytes it took. A window held
by a rate cap rather than by readers counts in neither. The same transports
report them, at the same lag.

The four credit fields and the three congestion fields above are omitted while
zero, as they are on most labels, and readers default a missing one to zero.

`sessions_refused` counts sessions turned away before admission, by reason.
They never start a session. `expired` counts a verified credential that expired
before admission. An auth-server or embedded refusal can name a root from a
verified credential and an optional tier; that root is the refusal's stats label,
with an absent tier using the default. A refusal without a verified root counts
under the empty label, never the dialed path. An embedder can also count a refusal
with `stats::Handle::refuse`, under the label and tier it resolved. Older readers
ignore `expired`; newer readers default a missing counter to zero.

`sessions_failed` counts admitted sessions that ended abnormally, by kind: a
subset of `sessions_ended`, so the live count is unchanged. A clean close by
either side, a drain, or a shutdown is not a failure. A grant that expires
mid-session counts as `expired`; one revoked or narrowed out from under it
counts as `application`. Each session records how it ended, so a grant revoked
after its session already closed does not count, and a dialed link that
reconnects fails only if its last session did. A reader treats a missing object
as zeros.

`requests` counts admitted request/response exchanges under the token's label and
tier, including HTTP `/fetch` and `/announced`. They never start or end a session.
A fetch's delivered group bytes count against the broadcast path on that tier;
`/announced` delivers no media bytes. The auth decider can give HTTP its own tier
using `Transport::Http`. An older reader ignores `requests`; a newer reader
reads it as zero when absent. A request moved to another tier stays counted where
it was admitted; later traffic follows the new tier.

`announces_peak` and `subscriptions_peak` are the most broadcasts announced to
the relay, and subscriptions held on it, by any one session. Compare announces
with the relay's [client or peer session limits](/bin/relay/config#session-and-peer),
past which a session is closed with `TOO_MANY_REQUESTS`, and subscriptions with
its QUIC `max_streams`, which bounds them. In a bucket's `labels` a peak never
goes down while the label has a session; in the totals and presence tracks it
never goes down while the track lives. Summing
nodes or labels takes the largest.

`routes_refused` counts the routes and claims that the label's moq-lite-07
sessions forwarded and the relay refused, keeping the session: the total of
`routes.refused`. The relay withdraws each one, so a neighbour on a newer
release costs only that route.

`routes` counts what the label's moq-lite-07 sessions did on the route layer,
each under the label of the session whose peer caused it. Its size never
depends on how many broadcasts or relays the mesh has, and a label with nothing
counted leaves it out, which readers read as zeros. When rolling a mesh onto
a new release one relay at a time, compare a canary's rates with its
neighbours':

| Field | Counts | A rise during a rollout means |
| --- | --- | --- |
| `changed` | Times a route to a relay changed on what the peer sent or how its session ended, counting only changes that alter how its broadcasts are served (not a move to another path of the same rank). | The mesh churns through this peer: links flapping, or two releases ranking routes differently. |
| `rewritten` | Broadcasts rewritten into the origin behind those changes: every broadcast the relay still claims, once per change. | Each change is costing the relay work in proportion to what lies behind it. Divide by `changed` for the cost of one. |
| `sent` / `received` | DEAD (a relay-to-relay session closed) and DRAIN (a relay sent GOAWAY) withdrawals, by kind. | Relays restarting or draining. A DRAIN rate without matching DEADs is a drain that never finishes. |
| `refused` | Refused routes and claims by reason: `path` (a path this relay cannot hold), `scope` (outside the session's root or grant, or what the relay serves), `looped` (the way already crosses this relay, or exceeds the hop limit). | `path` or `looped`: the releases disagree on route content. `scope`: a grant narrower than what the peer announces. |
| `closed` | Sessions the route layer closed, by kind: `stalled` (the peer stopped reading its route stream), `overrun` (it read too slowly for what changed), `malformed` (it sent what the rule never builds, or broke the stream), `role` (what its role never sends), `unasked` (its stream fell out of step), `other` (such as announcing past the session's cap). | Anything here on a canary that its neighbours lack: the releases disagree on the route wire. `stalled` and `overrun` alone point at an overloaded peer. |

A route that ended takes its claims with it, so they count as claims ending
rather than in `rewritten`. On moq-lite-07 a route change updates a broadcast
in place: the broadcast's `announces_started` and `announces_ended` count it
coming and going, not its routes changing.

### Counters

Every counter is a cumulative, monotonic unsigned integer. A rate is the
difference between two frames divided by the time between them, and a live
count is started minus ended. A frame never shows ended above started.

A new identity starts every counter from zero, so a reader summing a node over
time adds each identity's counters rather than diffing across them. Within one
identity the totals never go down. A bucket's path or label counter going **down**
means the entry was dropped and re-created, and a presence or prefix counter going down
means the track was held again. Treat it as the start of a fresh segment
rather than a negative rate. On a route whose identity a session minted (an older relay, or
a session older than moq-lite 07), a counter going down also means the node restarted.

A reader ignores unknown fields, so a newer relay can add counters, and
defaults a missing field to zero, so it can read an older relay.

## Encoding

Every stats track is a [moq-json](/lib/rs/moq-json) snapshot track that sets
the track's [compressed](/concept/compression) flag, so a session
that can signal compression carries it DEFLATEd and every other reads an
inflated copy. Stats frames change little between ticks, so the deltas and
compression together cost a fraction of the full objects' bytes.

- **Groups.** A group's first frame is the full object as UTF-8 JSON. Each
  later frame is an [RFC 7396](https://www.rfc-editor.org/rfc/rfc7396) merge
  patch against the value so far: it carries only the changed counters, and
  `null` removes a dropped entry. A joiner who finds the group's patches
  outweighing eight snapshots asks for a fresh group, which opens within an
  interval.

To read one, jump to the newest group, parse its first frame, then apply each
following frame as a merge patch, in order. A reader missing a frame abandons
the group and waits for the next.
