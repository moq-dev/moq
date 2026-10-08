---
title: moq-lite
description: The generic pub/sub layer, a simple forward-compatible subset of moq-transport
---

# moq-lite

moq-lite is the pub/sub protocol this project speaks. It is a deliberately
small subset of the IETF [moq-transport](/concept/standard) draft, so it works
against any moq-transport relay (including
[Cloudflare](https://moq.dev/blog/first-cdn)) while staying simple enough to
implement in an afternoon. The wire spec is
[draft-lcurley-moq-lite](/draft/moq-lite).

Rust and TypeScript speak moq-lite 01 through 06 and moq-transport drafts 14
through 22. Clients offer `moq-lite-06` first. moq-lite 07 is still in progress
(`moq-lite-07-wip`, and only when both sides enable it). It is the version that
carries [publisher epochs](#publisher-epochs).

## Terminology

| moq-lite | Meaning | moq-transport name |
| --- | --- | --- |
| **Session** | One connection, publishing and subscribing at once. | Session |
| **Origin** | The set of broadcasts visible to a session, scoped by the URL path. | (none) |
| **Broadcast** | A named, discoverable collection of tracks from one publisher. | Namespace |
| **Track** | A live sequence of groups, delivered out of order until closed. | Track |
| **Group** | A sequence of frames delivered reliably and in order, on its own QUIC stream. | Group |
| **Frame** | A sized chunk of bytes. | Object |
| **Datagram** | One unreliable frame sent as a QUIC datagram instead of a group. | Datagram |

## Discovery

A session can ask for announcements matching a path prefix. The peer replies
with what it can serve today, then streams changes as they come and go. That is
how a conference room learns who joined, how a player learns a stream came
online without polling, and how [relay clusters](/bin/relay/cluster) discover
each other.

An announcement is a **route**: a claim that broadcasts at a path prefix, and
every path beneath it, can be served. By convention a publisher announces each
broadcast's exact path, so subscribers enumerate broadcasts by listing routes.
A service can instead announce one short prefix and serve whatever is requested
beneath it, advertising capability without enumerating inventory. A route is
always a prefix. A service that serves only some of the paths beneath it
refuses the rest as they are requested. Announcements are hints; the request is
what decides.

Each route carries the chain of relay identities it passed through, which is
how forwarding loops are caught, and a cost, which is how a subscriber picks
among several routes to the same broadcast. It may also carry a
[publisher epoch](#publisher-epochs), which says which routes serve the same
bytes. A hop of 0 is the anonymous mark and travels the chain unchanged. A
route that passed through an anonymous hop at any depth ranks below
every fully identified route, whatever the costs say.

A broadcast exists only while it is announced, in the same process and across
a session: one that is created but never announced can be neither discovered
nor requested. Retracting a route stops new requests from resolving through it
and leaves subscriptions already in flight alone until each track ends or its
last subscriber leaves. A graceful session close withdraws its announcements
and waits up to one second for them to be delivered. An abort skips that.

### Publisher epochs

A route may carry an epoch: a UUIDv7 naming the publisher instance behind it.
A path and an epoch name one broadcast. The path never changes, so
authorization and hidden names work exactly as without one.

- **One instance, one epoch.** A publisher mints an epoch per run. Replicas of
  the same content announce that same epoch. A prefix claim, and any route
  without an epoch, names no instance.
- **Newest wins.** Among routes at the same prefix, the newest epoch ranks
  first, ahead of cost, and a route without an epoch ranks last. Newest means
  the latest UUIDv7 timestamp, so hosts with skewed clocks can lose a restart
  until the old instance retracts.
- **Same epoch resumes.** A subscription moves between routes with the same
  epoch without a seam, continuing from the first frame it lacks. A route
  without an epoch keeps its subscriptions until that route goes, which is how
  a transcoder claim stays on the worker that first served it.
- **A newer epoch replaces.** Subscriptions to the old instance end, and
  clients re-request the path to get the new broadcast. That is how a restarted
  encoder, whose group numbers start over, takes the name instead of stalling
  viewers on the old sequence.

The epoch travels on moq-lite 07, which is opt-in. Older moq-lite versions and
moq-transport cannot carry it, so their routes have no epoch: a relay keeps a
subscription on the route it first resolved through, and viewers of a
restarted publisher wait until the old session closes. In a mixed deployment
the epoch-carrying route supersedes the one without each time it appears, so a
flapping moq-lite 07 link cuts viewers that resolved through the older link.

`moq` and `moqsink` mint a fresh epoch per run, kept across reconnects, and the
RTMP, SRT, and WHIP ingests mint one per connection. Replicas share one by
passing the same `moq --epoch`, except under [encryption](/concept/hang#encryption). See [Clustering](/bin/relay/cluster) for how a
relay uses this.

### Hidden broadcasts

A path segment starting with `.` hides a route from discovery, the way a
dotfile hides from `ls`. A platform publishes its own broadcasts there (relay
stats under `.stats/`) without them turning up in an app that lists everything
and plays what it finds. Only segments below the requested prefix count:
listing the root skips `.stats/node`, but listing `.stats` shows `node`.

Hiding narrows discovery and nothing else. Subscribing to a hidden path by name
works, and tokens authorize it like any other path. Listing hidden routes is an
explicit opt-in on the announce request. On moq-transport that opt-in is the
[hidden](/draft/moq-hidden) extension; a peer that never declares it receives
every authorized namespace, dot-prefixed ones included.

## Path patterns

Patterns describe sets of literal paths. They are how a token scopes what a
session may publish and subscribe to, and how a consumer filters the
announcements it is told about. They never travel as announcements: the wire
carries prefixes, and each side filters locally.

A pattern matches the whole path. `room` matches only `room`, while `room/**`
matches `room` and every descendant. Segments are separated by `/`:

| Segment | Matches |
| --- | --- |
| `room` | That literal segment. |
| `*` | Exactly one nonempty segment. |
| `camera-*` | One segment starting with `camera-`. |
| `pre*suf` | One segment with that prefix and suffix, without overlapping them. |
| `**` | Zero or more segments. |

A pattern may contain at most 32 segments and one `**`. There is no escape for
a literal `*`. The empty pattern matches the empty path.

A subscriber watching under a root sees advertisements named relative to that
root. When several routes advertise one prefix, each reader sees the best route
its scope can use.

## Subscriptions

A subscriber names a broadcast and track. Delivery starts at the oldest group
it can still use, which at the default budget is the latest one, so every group
must begin at a point a fresh subscriber can decode from (a keyframe, a full
JSON snapshot). Groups can also be fetched by sequence number, which is how the
[HLS gateway](/bin/hls) and the relay's [HTTP fetch](/bin/relay/http) serve
history.

Each subscription carries the knobs that decide behavior under congestion:

| Knob | Effect |
| --- | --- |
| **Priority** (0..255) | Higher-priority tracks get bandwidth first. Audio above video, base layer above enhancement. |
| **Order** | Which group to send first when several are pending. Newest first for live, oldest first for catch-up. |
| **Max delay** | How far a non-latest group may fall behind the live edge before it is skipped. Zero means "live edge only", and raising it is also what asks for history. |

Max delay is measured on the media timeline, not the wall clock, so a backlog
delivered as a burst is still late while a congestion stall never expires
anything on its own. Both ends apply it: the publisher skips a group rather
than sending it, and the subscriber skips it again as it reads, since the
publisher only ever sees the most tolerant budget across its subscribers.

A track is timed or untimed: it declares a timescale or none, and a frame that
doesn't match is refused. No receiver fills in an arrival time. An untimed
group never falls past max delay, so a new subscriber starts at its latest
group unless it names a start. Tracks from moq-lite before 05, or from
moq-transport without a `TIMESCALE` (every track on drafts 14-16), arrive
untimed. No moq-lite version can declare an untimed track, so on 05 and later
its frames go out stamped with their send time.

A relay cancels its upstream subscription once nobody subscribes, but keeps its
copy of the track for 30 seconds after the last reader leaves, so a returning
reader or the next fetch finds it. That copy is not live meanwhile: readers get
nothing from it until the source answers again.

The publisher may declare a retention window per track. Omission sets no limit;
zero keeps only the live edge. Relays preserve that declaration, and may still
evict sooner under their own cache ceiling. Media tracks explicitly use 30
seconds so a segmented egress can still find its segments. Retention uses media
timestamps and always keeps the newest group. IETF carries the same idea as
`MAX_CACHE_DURATION`, measured in wall time, which is only an approximate match.

Put together, a conference might use:

| Track | Priority | Order | Max delay |
| --- | --- | --- | --- |
| audio | 100 | ascending | 500 ms |
| video | 50 | descending | 2 s |

Under light congestion video drops the tail of a group; under heavy congestion
video stops and audio lags by at most 500 ms. No protocol change, just knobs.

## Datagrams

Since moq-lite 05, a publisher can send a tiny single-frame group as a QUIC
datagram: unreliable, unordered, under about 1200 bytes, and never
retransmitted. It suits real-time audio and sensor data. There is no stream
fallback, so a datagram that doesn't fit isn't delivered that way.

Nothing caches a datagram. A subscription gets the ones inside its group
range, and a new one may get the few still in the publisher's short send
buffer, but `FETCH` never returns one. A publisher that knows the sequence was
a datagram refuses with `NOT_FETCHABLE` (moq-lite-07), otherwise with
`NOT_FOUND`. Use a group for anything a late joiner needs.

## What moq-lite leaves out

Compared with moq-transport: no request IDs (a stream per request instead), no
push (subscribers always ask), fetches within a single group only, no
sub-groups (use a track per SVC layer), no gaps in object numbering, no
per-object metadata (encode it in the payload), no pausing (unsubscribe
instead), and UTF-8 names instead of byte arrays. When a peer negotiates
moq-transport the implementation still enforces this simpler model, faking or
refusing the rest. The [standards](/concept/standard) page lists the limits a
peer actually observes.

| Client | Relay | Works |
| --- | --- | --- |
| moq-lite | moq-lite | yes |
| moq-lite | moq-transport | yes |
| moq-transport | moq-lite | without moq-transport-only features |
| moq-transport | moq-transport | depends on the implementations |
