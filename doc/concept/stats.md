---
title: Stats
description: The stats broadcasts a relay publishes, their tracks, and their JSON encoding
---

# Stats

A relay publishes its traffic counters as ordinary MoQ broadcasts, so any
subscriber can read them: a dashboard, a billing meter, an aggregator in
another region. This page is the wire contract for those broadcasts, enough to
read them in any language. [`moq-stats`](https://docs.rs/moq-stats) is the Rust
producer and consumer, and the relay's [`[stats]`](/bin/relay/config#stats)
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
it reads and records its newest arrival. See the
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
The PCR checks are graded on PCR values, not arrival: they speak for the
encoder's insertion, since the network re-times arrival, and a forward step
over 100 ms counts as both. 1.6 `PID_error` is covered, more strictly, by each
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
(its path and epoch), which observed every number in it; the caller states the
interval between the two and how long ago the newer one arrived. Both
languages compute the same result from the shared fixtures in
`rs/hang/fixtures/health.json`.

The result holds the deltas over the interval (frames, bytes, and bitrate per
rendition; lost bytes and packets for the connection) and a verdict for the
connection and each rendition: `unknown`, `healthy`, `degraded`, or
`unhealthy`. Each verdict carries its evidence, every input with its value and
grade, and is the worst graded input. An input the report cannot show is
listed as `unknown`, and a verdict with nothing graded is `unknown`: a browser
publisher that knows only its PROBE RTT grades on RTT alone. A report older
than the stale limit is `unknown` throughout, as is a transport sample whose
`age` plus the report's exceeds it, and a client with no report has no sample at all. A changed
path or epoch (`replaced`) or a decreasing counter (`decrease`) is a reset:
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
through a shaped link over five-second intervals; no CLI reporter emits
`skipped`, `late`, `errors`, or `stalled`, so their limits are not measured.

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
carries a fresh [publisher epoch](/concept/moq-lite#publisher-epochs) on its
route, so a restarted node is a new broadcast at the same path: it replaces the
old one, and no relay serves it groups cached under the old epoch. A reader
treats each epoch as its own set of counters. The epoch rides moq-lite 07
announcements, and moq-transport with the
[epoch extension](/draft/moq-epoch); over anything else a reader sees the same
change as an end and a start at the path.
An aggregating reader pins each node's subscription to its epoch, and when one
publisher is reached through both kinds of link (with and without an epoch), it
carries only that path's outgoing counters across the change.

Every group shares the node's broadcast, so a token scoped to the stats prefix
reads every group's tracks; scope by group with an aggregating reader instead.

## Tracks

Traffic is split by **tier**, an arbitrary label (a billing class, a region)
the relay takes from the auth grant, or from `--cluster-tier` for links it
dials and LAN peers it admits. The default tier's label is empty. Every track
but a group's comes in two encodings:

| Track | Frame keyed by | Entry |
| --- | --- | --- |
| `totals.json` | tier | [Totals](#totals) for the whole node |
| `[<group>/]group.json` | | [Group](#group): one group's totals, paths, and labels, every tier |
| `[<label>/]presence.json` | tier | [Presence](#presence): sessions under one stats label |
| `[<path>/]prefix.json` | tier | [Prefix](#prefix) traffic under one broadcast prefix |

Every track uses the one [encoding](#encoding). `totals.json` always exists.

### Group tracks

The relay buckets traffic into groups by the first `depth` segments of each
broadcast path, and sessions by the first `depth` segments of each stats label;
a path shorter than `depth` groups under all of its segments. Each group is a
`<group>/group.json` track (`group.json` for the empty group, the only one at
depth 0) carrying all of the group's tiers, both directions, and its sessions,
so a reader holds one subscription per group.

A reader discovers the live groups from the broadcast's track listing (TRACKS
in moq-lite 07, `SUBSCRIBE_TRACKS` in moq-transport draft 18 and later). At
depth 0 the one group's track is listed for the producer's life. At depth 1 or
more, a group's track is listed from its first entry until it has lingered
empty (five minutes by default), then unlisted and ended. A group that returns
within the linger keeps its track and epoch, and its totals continue. The
node's announcement never changes, so group churn costs no announce traffic
across a cluster. Over a hop that cannot carry a listing (moq-lite 06 and
older, or moq-transport before draft 18) the listing is empty or refused; a
reader that knows a group's name can still subscribe to it while it is listed,
and is refused otherwise.

A group that returns after its linger is a new instance of its track: its
frames carry a new `epoch` and count from zero, and its group numbers, which
count publish intervals, continue past the old instance's. Holding a group's
track doesn't count as an entry, so a reader follows the listing to the next
instance. A frame too large to cache ends only that group's track.

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
[group](#group) track's. A track with no entries holds `{}`.

The producer drains its counters every interval (one second by default) and
writes only when a track's frame changed, so silence means nothing moved, not
that the producer is gone. The track ends when the producer does, or when its
group track is unlisted; the group may return later as a new track instance. A
frame too large to cache (32 MB) ends its track with a `FRAME_TOO_LARGE` error,
measured by its uncompressed size, since a reader inflates the whole group.

A group's number is its publish's scheduled time divided by the interval, so
the interval is its timescale. Publishes are scheduled at least an interval
apart, even after a late one, so each opens its groups in an interval of its
own, and a track recreated later numbers its groups past every group its
earlier instance wrote.

### Totals

```json
{
  "": { "publisher": { "bytes": 1048576, ... }, "subscriber": { ... }, "sessions": { ... } },
  "region/sjc": { ... }
}
```

Each tier that recorded anything in the epoch, with its egress (`publisher`)
and ingress (`subscriber`) [Traffic](#traffic) and its [Presence](#presence),
summed over every broadcast and stats label. Unlike a group's paths and labels,
an entry that ends stays counted, so the next frame always carries everything
since the epoch began: a reader that misses frames loses nothing while the
node stays announced. Billing reads this track.

### Group

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

`epoch` is a UUIDv7 minted for each instance of the group's track, and a
reader keeps each epoch's counters apart, as it does for a broadcast's epochs.
`tiers` maps each tier the group recorded on since then. Its top-level
`publisher`, `subscriber`, and `sessions` are the group's [totals](#totals), so
a totals reader parses them unchanged, and like the node's totals they keep
every entry that ended. A lingering group's frames repeat its final totals: a
reader that misses every frame from the group's last change until it is
unlisted loses that instance's tail, so a meter reads the node's totals.

`paths` holds each broadcast path's egress (`publisher`) and ingress
(`subscriber`) [Traffic](#traffic). A path appears from its first nonzero
counter until the relay drops its counters, even while idle: the last viewer
can leave while the publisher still holds the path, and a returning viewer
resumes the same counters. So a path missing from a frame had no counters, and
one that returns starts from zero; the totals keep what it counted. A side that
has never moved is left out and reads as zero.

`labels` holds each stats label's [Presence](#presence), while a session or
HTTP request holds its context and on the tick its last one ends. While a group
lingers empty, its `paths` and `labels` are empty.

### Per-label tracks

A presence track's entries are cumulative from when it was first held: a
session that ends stays counted, and so does every one after it, while
anyone holds the track. A track held again starts over.

Wire bytes start at zero on every hold, including the first. Other presence
fields seed from the root's current live-session counters; they do not recover
closed roots from before the hold. Node totals keep the full epoch's
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
    "stale": { "bytes": 0, "frames": 0, "groups": 0, "datagrams": 0 },
    "announced": 1, "announced_closed": 0,
    "broadcasts": 3, "broadcasts_closed": 1,
    "subscriptions": 6, "subscriptions_closed": 2
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
| `bytes` / `frames` / `groups` | Payload delivered. |
| `datagrams` | Groups delivered as an unreliable datagram. A subset of `groups`. |
| `inflated` | Bytes inflated from compressed groups for plain readers, counted once per group however many share the copy, plus received compressed datagrams, which inflate on arrival. Not part of `bytes`, which counts payload as stored or sent. |
| `stale` | Payload skipped because it aged past a subscriber's latency budget, with the same four fields. Disjoint from the top-level payload counters. |

The last six fields are legacy spellings of the `*_started` and `*_ended`
counters, still written so an older consumer reads a newer relay. A reader
should prefer the canonical name and fall back to the legacy one.

### Presence

```json
{
  "acme": {
    "requests": 3, "sessions_started": 12, "sessions_ended": 10,
    "sessions": 12, "sessions_closed": 10,
    "sent_bytes": 1048576, "received_bytes": 524288,
    "sessions_refused": { "refused": 0, "unavailable": 0, "request": 0, "forbidden": 0, "lan": 0, "expired": 0 },
    "sessions_failed": { "idle_timeout": 1, "transport": 0, "application": 0, "expired": 0 },
    "announces_peak": 40, "subscriptions_peak": 900,
    "wire_tx": 2048, "wire_rx": 1024
  }
}
```

`sessions_started` and `sessions_ended` count connects and disconnects, whether
or not any data flows: under a stats label on the tier for a group's `labels`,
under the label on each tier for `presence.json`, and under every label in the
group or node on the tier for its totals. `sessions` and `sessions_closed` are their
legacy spellings. A session moved to a new tier ends on the old one and starts
on the new. `sent_bytes` and `received_bytes` count the payload this node sent
to and received from those sessions and HTTP requests, the same bytes a
group's `paths` count. A changed label moves later presence and bytes; bytes
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

Unsupported transports, including browsers and the current WebSocket/qmux
adapter, contribute zero. WebSocket wire accounting requires the separate
qmux counter source; unavailable counters are never estimated from payload.
Readers default missing wire fields to zero for older relays.

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
its QUIC `max_streams`, which bounds them. In a group's `labels` a peak never
goes down while the label has a session; in the totals and presence tracks it
never goes down while the track lives. Summing
nodes or labels takes the largest.

### Counters

Every counter is a cumulative, monotonic unsigned integer. A rate is the
difference between two frames divided by the time between them, and a live
count is started minus ended. A frame never shows ended above started.

A new epoch starts every counter from zero, so a reader summing a node over
time adds each epoch's counters rather than diffing across them. Within one
epoch the totals never go down. A group's path or label counter going **down**
means the entry was dropped and re-created, and a presence or prefix counter going down
means the track was held again. Treat it as the start of a fresh segment
rather than a negative rate. On a route without an epoch (an older relay, or a session older
than moq-lite 07), a counter going down also means the node restarted.

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
