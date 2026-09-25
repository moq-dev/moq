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

At depth 0 the broadcast stays announced for the producer's life. At depth
1 or more, a group's broadcast is announced while that group has entries and
unannounced once it has none.

## Tracks

Traffic is split by **tier**, an arbitrary label (a billing class, a region)
the relay takes from the auth grant or `--cluster-tier`. Each tier has three
tracks, each in two encodings:

| Track | Frame keyed by | Entry |
| --- | --- | --- |
| `publisher.json` | broadcast path | [Traffic](#traffic) this node sent (egress) |
| `subscriber.json` | broadcast path | [Traffic](#traffic) this node received (ingress) |
| `sessions.json` | auth root | [Presence](#presence) of connected sessions |

The default tier is unprefixed. A named tier prefixes each name with its
label and a slash: tier `region/sjc` publishes `region/sjc/publisher.json`.
Appending `.z` selects the [compressed](#compressed) encoding of the same
track: `publisher.json.z`.

The default tier's six tracks always exist. A named tier's are created on its
first recorded traffic, but a subscriber may ask for them earlier: any name of
the shape `[<tier>/]{publisher,subscriber,sessions}.json[.z]` is accepted and
held open with `{}` until the tier records. Any other name is refused.

## Frames

Every frame is a JSON object mapping a key (broadcast path or auth root) to an
entry. An entry appears while it is **live**, meaning some started counter
still exceeds its ended counterpart so traffic could resume at any moment, and
on any tick its counters changed. Once fully closed it appears one last time
with its final counters and is then dropped. A track with no entries holds `{}`.

The producer drains its counters every interval (one second by default) and
writes only when a track's frame changed, so silence means nothing moved, not
that the producer is gone. The track ends when it is.

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
| `broadcasts_started` / `broadcasts_ended` | A session's first subscription to the broadcast, and its last one closing. Started minus ended is the viewer count. |
| `subscriptions_started` / `subscriptions_ended` | Track subscriptions opened and closed. |
| `fetches` | One-shot group fetches requested, including ones that found nothing. Their payload counts in `bytes`, `frames`, and `groups`. |
| `bytes` / `frames` / `groups` | Payload delivered. |
| `datagrams` | Groups delivered as an unreliable datagram. A subset of `groups`. |
| `stale` | Payload skipped because it aged past a subscriber's latency budget, with the same four fields. Disjoint from the top-level payload counters. |

The last six fields are legacy spellings of the `*_started` and `*_ended`
counters, still written so an older consumer reads a newer relay. A reader
should prefer the canonical name and fall back to the legacy one.

### Presence

```json
{ "acme": { "sessions_started": 12, "sessions_ended": 10, "sessions": 12, "sessions_closed": 10 } }
```

`sessions_started` and `sessions_ended` count connects and disconnects under an
auth root on the tier, whether or not any data flows. `sessions` and
`sessions_closed` are their legacy spellings. A session moved to a new tier
ends on the old one and starts on the new.

### Counters

Every counter is a cumulative, monotonic unsigned integer. A rate is the
difference between two frames divided by the time between them, and a live
count is started minus ended. A frame never shows ended above started.

A counter going **down** means the relay restarted or the entry was dropped
and re-created. Treat it as the start of a fresh segment rather than a
negative rate.

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
  dropped entry. The producer starts a new group once the patches outgrow
  eight times the snapshot's compressed size, or after 256 frames.
- **DEFLATE.** Each group's frames form one raw DEFLATE stream, sync flushed
  per frame with the trailing `00 00 ff ff` stripped, as
  [moq-flate](/draft/moq-flate) specifies. The window starts cold at every
  group and never spans two.

To read one, jump to the newest group, inflate and parse its first frame,
then inflate and apply each following frame as a merge patch, in order. A
reader missing a frame abandons the group and waits for the next.
