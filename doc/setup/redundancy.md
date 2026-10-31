---
title: Redundant Ingest
description: Protect publishers and delivery routes without confusing broadcast identity
---

# Redundant ingest

Choose what must survive: a publisher failing, a delivery route failing, or
both. Two independent publishers can announce one path for automatic switching,
or use separate paths so the subscriber chooses. Several routes carrying one
publisher's broadcast can resume the same stream.

| Setup | What a failure does |
| --- | --- |
| Independent publishers, one path | Switches to another broadcast at that path. |
| Independent publishers, separate paths | Leaves selection to the subscriber. |
| One publisher, several routes | Resumes the same broadcast on another route. |

A [path and publisher epoch](/concept/moq-lite#publisher-epochs) identify one
broadcast. Every independent publisher mints its own epoch, even if both read
the same source. Sharing an epoch would claim that their group and frame
numbers name the same content, which independently running importers cannot
promise. See [identity and recovery](/concept/identity) for the whole contract.

## Independent publishers on one path

1. Run the publishers on separate hosts and ingress routes. Connect their
   relays into a [cluster](/bin/relay/cluster) so the viewer's relay can reach
   both. Give both publishers permission to announce the same full path.
2. Publish the same broadcast name under the same auth path. Each publisher
   keeps its own epoch; `moq` creates one automatically.
3. Use epoch-capable connections on every hop: moq-lite 07, which is opt-in,
   or moq-transport with the [epoch extension](/draft/moq-epoch)
   enabled. Keep the publisher hosts' clocks synchronized, since epochs order
   by their UUIDv7 creation time.

For example, two HLS importers can publish `event.hang` through different
relays. Run each command on its publisher host, with its own source URL:

```bash
# Publisher A
moq --connect https://ingest-a.example.com/anon \
    --connect-version moq-transport-18 --broadcast event.hang \
    import hls https://source-a.example.com/live/index.m3u8

# Publisher B
moq --connect https://ingest-b.example.com/anon \
    --connect-version moq-transport-18 --broadcast event.hang \
    import hls https://source-b.example.com/live/index.m3u8
```

The relays must forward these announcements under the same path; publishing to
two disconnected relays does not make one a fallback for the other. The same
setup applies to other publishers, including browser capture and ingest
gateways. See [Production](/setup/prod) for TLS and authentication.

At the same path, the newest epoch wins new requests, ahead of route cost.
Starting B after A therefore switches followers to B even while A is healthy.
If B's route goes away and A remains live and reachable, A wins again as a new
broadcast. This is not a fixed primary/standby priority: restarting either
publisher can make it the newest.

The switch is announced as a `Restart`. Players that follow announcements
request the path again and begin the replacement broadcast. Existing
subscriptions stay on the old instance while it remains available, unless the
consumer chooses to follow the restart.

For MPEG-TS output, opt into following replacements:

```bash
moq --connect https://edge.example.com/anon \
    --connect-version moq-transport-18 --broadcast event.hang \
    export ts --stitch --follow 10s > event.ts
```

`--stitch` follows a replacement as a program switch: its catalog supplies a
new PMT and every PID flags the discontinuity. Starting a backup can cause
this switch too. The old instance keeps feeding output until the replacement's
clock and first media are usable, bounded by `--follow`, at lower subscription
priority so the replacement can acquire on a congested link. The switch drops
the old instance's buffered tail, so the new program plays at `--delay` instead
of carrying that tail as added latency. An unimpaired replacement usable within
`--follow` adds no gap. Congestion can starve the old instance sooner or leave
the replacement acquiring past the window, causing a gap. The epochs' timestamps
are not aligned, so this is still a marked program discontinuity, not hitless
failover.
`--follow 10s` also bounds waiting for a publisher after an end; choose that
window for your application. Without `--stitch`, the TS export stays on the old
instance while it is up, then fails on a replacement. Other stdout export
formats end with their broadcast; they do not follow a replacement. TS
passthrough also cannot stitch. See [CLI retention and latency](/bin/cli#retention-and-latency).

A connection without epochs cannot prove continuity or apply the newest-epoch
rule: routing chooses among its epochless routes instead, and another route
winning is a restart. Do not rely on same-path continuity through such a hop.

## Independent publishers on separate paths

Publish each stream under its own name, such as `event/a.hang` and
`event/b.hang`. The end subscriber can subscribe to both and decide whether to
switch between them, combine them, or keep them separate. This is useful when
selection should depend on the media or operator policy rather than which
publisher started last.

Each publisher produces its own stream with its own epoch. The relay routes
the two paths independently; it does not align their frames or stitch their
output. The subscriber owns any transition between them.

## One publisher through several routes

Keep a single publisher instance and distribute copies of its broadcast
through multiple relays or CDNs. All copies retain its path, epoch, group
numbers, and frame content.

Connect the publisher's relay to multiple peers, then make those routes
reachable from the subscriber's relay. An application can also hold sessions
to [multiple CDNs](/concept/moq-lite#multiple-cdns) feeding one shared origin
and rank its links. Do not start a second importer to create the second copy.

When the serving route fails or another route with the same epoch wins, the
subscription resumes from the first frame it lacks. This needs an explicit
matching epoch across every hop, routes whose copies report identical track
info (so the same protocol and draft on each), and a surviving route that can still serve
those frames within the broadcast's retention. It preserves broadcast
continuity, but a late recovery can still miss playback deadlines. Losing the
only publisher cannot be repaired by adding routes to it.

## Hitless protection at the boundaries

For ST 2022-7 protection downstream, form both RTP legs from one broadcast and
supply matching RTP packetization in the downstream system. Independent
publishers switching at one path are different broadcasts, so their output is
not a pair of identical protection legs.

To protect the input to an importer hitlessly, merge duplicate legs before a
single importer, using upstream protection equipment. That importer then
publishes one broadcast, which can travel over several MoQ routes.
