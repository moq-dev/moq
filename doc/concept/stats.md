---
title: Stats
description: The stats broadcasts a relay publishes, their tracks, and both JSON encodings
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

`moq import --stats` and `moq export --echo` report them from the
[CLI](/bin/cli#stats-and-feedback). An import counts the frames and payload
bytes it writes to each rendition, and video keyframes; an export counts what
it reads and records its newest arrival. See the
[hang draft](/draft/moq-hang#media-stats) for track framing, field units, and
the feedback convention.

### MPEG-TS sections

A TS import or export adds sections of its own, which
`moq_mux::container::ts::stats::Report` reads. As above, counters are
cumulative and omitted at zero, and a gauge is omitted until it is measured.

`mpegts` holds `streams`, one row per elementary stream keyed by its PID in
decimal, and the stream-wide counters. A row has `track` (the track suffix),
`class` (`audio`, `video`, or `data`; an unknown class reads as `data`),
`units` (access units delivered), and `quiet`, a gauge: milliseconds of
program clock since its last access unit. Its losses are `resyncs`, bytes
`discarded` scanning for audio sync, `unconfirmed` frames, `damaged` units,
and its shares of `continuityCountError`, `transportError`, and `ptsError`.

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
| `trimmed` | Frames dropped before their stream's first went out, so the output starts on a whole group led by a keyframe. Nothing is missing for these. |
| `gaps` | Times the source skipped media it will never deliver: a group lost or expired upstream, or a resubscription. A data track's declared break counts too. |
| `margin` | Gauge, milliseconds: the least time any frame arrived before its deadline over the last second. Negative when one missed. |
| `offset` | Gauge, milliseconds: how far the output clock sits from the source's plus the delay. Positive holds frames longer than the delay. The clock pulls it shut over ten minutes. |
| `drift` | Gauge, parts per million: the source clock's rate against ours, positive running fast. |
| `outOfTolerance` | Measurements that found the source clock past the 30 ppm ISO/IEC 13818-1 allows, which the output cannot follow. |

## Broadcasts

Each node publishes under a prefix, `.stats` by default, which
[moq-lite](/concept/moq-lite) hides from announce listings that don't ask for
it. Traffic under the prefix is never counted, so serving stats doesn't
generate more stats.

```text
<prefix>/node/<node>               depth 0: one broadcast per node
<prefix>/<group>/node/<node>       depth N: one broadcast per group per node
```

- `<node>` tells relays sharing a cluster apart. It may span several segments
  (`sjc/1`), and is omitted along with its slash when unset: `<prefix>/node`.
- `<group>` is the first `depth` segments of each broadcast path (for traffic)
  or auth root (for sessions), so a consumer can scope an announce to one
  tenant. A path shorter than `depth` groups under all of its segments.
- The literal `node` segment leaves room for sibling categories under the same
  prefix, so a consumer skips any path without `node` where it expects one. A
  group segment literally named `node` is ambiguous; don't use one.

Each announcement carries a fresh [publisher epoch](/concept/moq-lite#publisher-epochs)
on its route, so a restarted node or a group returning from idle is a new
broadcast at the same path: it replaces the old one, and no relay serves it
groups cached under the old epoch. A reader treats each epoch as its own set of
counters. The epoch rides moq-lite 07 announcements; over older versions and
moq-transport a reader sees the same change as an end and a start at the path.
An aggregating reader pins each node's subscription to its epoch, and when one
publisher is reached through both kinds of link (with and without an epoch), it
carries only that path's outgoing counters across the change.

At depth 0 the broadcast stays announced for the producer's life. At depth
1 or more, a group's broadcast is announced while that group has entries, and
for a linger (five minutes by default) after its last one leaves. A group that
returns within the linger keeps its broadcast and epoch, so viewer churn
doesn't unannounce and re-announce it across the mesh, and its totals
continue. Once the linger elapses with the group still empty, the broadcast is
unannounced and its counters dropped; a group that returns later announces a
new epoch counted from zero. Within one epoch, group numbers keep increasing
across recreated tracks and may have gaps.

## Tracks

Traffic is split by **tier**, an arbitrary label (a billing class, a region)
the relay takes from the auth grant, or from `--cluster-tier` for links it
dials and LAN peers it admits. The default tier's label is empty. Each track
comes in two encodings:

| Track | Frame keyed by | Entry |
| --- | --- | --- |
| `totals.json` | tier | [Totals](#totals) for the whole group |
| `[<tier>/]publisher.json` | broadcast path | [Traffic](#traffic) this node sent (egress) |
| `[<tier>/]subscriber.json` | broadcast path | [Traffic](#traffic) this node received (ingress) |
| `[<tier>/]sessions.json` | auth root | [Sessions](#sessions) connected now |
| `[<root>/]presence.json` | tier | [Sessions](#sessions) under one auth root |

The default tier's per-tier tracks are unprefixed. A named tier prefixes each
name with its label and a slash: tier `region/sjc` publishes
`region/sjc/publisher.json`. Appending `.z` selects the
[compressed](#compressed) encoding of the same track: `publisher.json.z`.

`totals.json` and the default tier's six per-tier tracks always exist. A named
tier's are created on its first recorded traffic, but a subscriber may ask for
them earlier: any name of the shape
`[<tier>/]{publisher,subscriber,sessions}.json[.z]` is accepted and held open
with `{}` until the tier records.

A presence track exists only while subscribed. Ask for `<root>/presence.json`
for any auth root in the group (`presence.json` for the empty root). The relay
answers with the root's current sessions and counts from there, holding a root
with no sessions open at `{}`, and stops producing it once the last subscriber
leaves. A root outside the group, or any other name, is refused.

## Frames

Every frame is a JSON object mapping a key to an entry. A track with no
entries holds `{}`.

The producer drains its counters every interval (one second by default) and
writes only when a track's frame changed, so silence means nothing moved, not
that the producer is gone. The track ends when the producer does, or at
depth 1 or more when its group's broadcast is unannounced; the group may
return later as a new broadcast.

### Totals

```json
{
  "": { "publisher": { "bytes": 1048576, ... }, "subscriber": { ... }, "sessions": { ... } },
  "region/sjc": { ... }
}
```

Each tier that recorded anything in the group's epoch, with its egress
(`publisher`) and ingress (`subscriber`) [Traffic](#traffic) and its
[Sessions](#sessions), summed over every broadcast and auth root in the group.
Unlike the per-path tracks, an entry that ends stays counted, so the next
frame always carries everything since the epoch began: a reader that misses
frames loses nothing while the group stays announced. Billing reads this
track.

A lingering group's frames repeat its final totals. A reader that misses every
frame from the group's last change until it unannounces loses that epoch's
tail, so a meter that only reads totals under-counts by it.

### Per-tier tracks

A traffic entry appears from its first nonzero counter until the relay drops
its counters, even while idle: the last viewer can leave while the publisher
still holds the path, and a returning viewer resumes the same counters. So a
key missing from a frame had no counters, and one that returns starts from
zero; the totals keep what it counted. A sessions entry appears while a
session or HTTP request holds its context and on the tick its last one ends. While a group
lingers empty, these tracks hold `{}`.

### Presence

A presence track's entries are cumulative from when it was first held: a
session that ends stays counted, and so does every one after it, while
anyone holds the track. A track held again starts over.

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
| `broadcasts_started` / `broadcasts_ended` | On `publisher.json`, a session's first subscription to the broadcast and its last one closing. Started minus ended is the viewer count. `subscriber.json` leaves both at zero: ingress does not count viewers. |
| `subscriptions_started` / `subscriptions_ended` | Track subscriptions opened and closed. |
| `fetches` | One-shot group fetches requested, including ones that found nothing. Their payload counts in `bytes`, `frames`, and `groups`. |
| `bytes` / `frames` / `groups` | Payload delivered. |
| `datagrams` | Groups delivered as an unreliable datagram. A subset of `groups`. |
| `stale` | Payload skipped because it aged past a subscriber's latency budget, with the same four fields. Disjoint from the top-level payload counters. |

The last six fields are legacy spellings of the `*_started` and `*_ended`
counters, still written so an older consumer reads a newer relay. A reader
should prefer the canonical name and fall back to the legacy one.

### Sessions

```json
{ "acme": { "requests": 3, "sessions_started": 12, "sessions_ended": 10, "sessions": 12, "sessions_closed": 10, "announces_peak": 40, "subscriptions_peak": 900 } }
```

`sessions_started` and `sessions_ended` count connects and disconnects, whether
or not any data flows: under an auth root on the tier for `sessions.json`,
under the root on each tier for `presence.json`, and under every root in the
group on the tier for `totals.json`. `sessions` and `sessions_closed` are their
legacy spellings. A session moved to a new tier ends on the old one and starts
on the new.

`requests` counts admitted request/response exchanges under the token's root and
tier, including HTTP `/fetch` and `/announced`. They never start or end a session.
A fetch's delivered group bytes count against the broadcast path on that tier;
`/announced` delivers no media bytes. The auth decider can give HTTP its own tier
using `Transport::Http`. An older reader ignores `requests`; a newer reader
reads it as zero when absent. A request moved to another tier stays counted where
it was admitted; later traffic follows the new tier.

`announces_peak` and `subscriptions_peak` are the most broadcasts announced to
the relay, and subscriptions held on it, by any one session. Compare them with
the relay's [client or peer session limits](/bin/relay/config#session-and-peer):
a session that goes past one is closed with `TOO_MANY_REQUESTS`. On
`sessions.json` a peak never goes down while the root has a session; on the
totals and presence tracks it never goes down while the track lives. Summing
nodes or roots takes the largest.

### Counters

Every counter is a cumulative, monotonic unsigned integer. A rate is the
difference between two frames divided by the time between them, and a live
count is started minus ended. A frame never shows ended above started.

A new epoch starts every counter from zero, so a reader summing a node over
time adds each epoch's counters rather than diffing across them. Within one
epoch the totals never go down. A per-tier counter going **down** means the
entry was dropped and re-created, and a presence counter going down means the
track was held again. Treat it as the start of a fresh segment rather than a
negative rate. On a route without an epoch (an older relay, or a session older
than moq-lite 07), a counter going down also means the node restarted.

A reader ignores unknown fields, so a newer relay can add counters, and
defaults a missing field to zero, so it can read an older relay.

## Encodings

Both encodings carry identical frames; pick by bandwidth. Only the track name
says which one a track uses: the payload has no marker.

### Plain

On a `.json` track each changed frame is its own group holding one frame, the
full object as UTF-8 JSON. A reader takes the newest group.

### Compressed

A `.json.z` track is a [moq-json](/lib/rs/moq-json) snapshot track with
compression on. Stats frames change little between ticks, so it costs a
fraction of the plain track's bytes.

- **Groups.** A group's first frame is the full object. Each later frame is an
  [RFC 7396](https://www.rfc-editor.org/rfc/rfc7396) merge patch against the
  value so far: it carries only the changed counters, and `null` removes a
  dropped entry.
- **DEFLATE.** Each group's frames form one raw DEFLATE stream, sync flushed
  per frame with the trailing `00 00 ff ff` stripped, as
  [moq-flate](/draft/moq-flate) specifies. The window starts cold at every
  group and never spans two.

To read one, jump to the newest group, inflate and parse its first frame,
then inflate and apply each following frame as a merge patch, in order. A
reader missing a frame abandons the group and waits for the next.
