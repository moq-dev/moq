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

## Session setup

A dedicated ALPN selects the wire version for moq-lite 03 and newer. The
legacy `moql` ALPN negotiates moq-lite 01 or 02 via `SETUP`. In moq-lite 05
and newer, each side also sends a `SETUP` message with its capabilities.
Rust and TypeScript speak moq-lite 01 through 06 and moq-transport drafts
14 through 22. Clients offer `moq-lite-06` first by default. moq-lite 07 is
still in progress: it negotiates as `moq-lite-07-wip`, and only when both
sides explicitly enable it.

## Discovery

A session can ask for announcements matching a path prefix. The peer replies
with what it can serve today, then streams changes as they come and go. That is
how a conference room learns who joined, how a player learns a stream came
online without polling, and how [relay clusters](/bin/relay/cluster) discover
each other.

An announcement is a **route**: a claim that broadcasts at a path prefix, and
every path beneath it, can be served. By convention a publisher announces each
broadcast's exact path, so subscribers enumerate broadcasts by listing routes,
but a service can announce one short prefix and serve whatever is requested
beneath it, advertising capability without enumerating inventory. A route is
always a prefix, on every wire version and on moq-transport alike; a service
that serves only some of the paths beneath its prefix refuses the rest as they
are requested. Each route carries the chain of relay identities it passed
through, which is how forwarding loops are caught, and a cost, which is how a
subscriber picks among several routes to the same broadcast. A hop of 0 is the
anonymous mark and travels the chain unchanged. A route that passed through an
anonymous hop at any depth ranks below every fully identified route, whatever
the costs say; among anonymous routes, cost keeps ordering.

A broadcast exists only while it is announced, for consumers in the same
process and across a session alike: one that is created but never announced
can be neither discovered nor requested. A broadcast published locally
competes with remote routes to its path on cost like any other route, winning
only a tie. Retracting a route (an unannounce, or the peer's `ANNOUNCE_END`)
stops new requests from resolving through it but leaves subscriptions already
in flight alone: each track runs to its own end, the publisher's FIN or reset.
moq-transport sessions behave the same when a namespace is withdrawn.

### Hidden broadcasts

A path segment starting with `.` hides a route from discovery, the way a
dotfile hides from `ls`. A platform publishes its own broadcasts there (relay
stats under `.stats/`, cluster gossip under `.internal/`) without them turning
up in an app that lists everything and plays what it finds. Only segments
below the requested prefix count: listing the root skips `.stats/node`, but
listing `.stats` shows `node`. A `.` elsewhere in a segment (`catalog.pro`) is
part of the name.

Hiding narrows discovery and nothing else. Subscribing to a hidden path by
name works without asking, and tokens authorize it like any other path. To
list hidden routes too, opt in per announce request:

```rust
let announced = origin.consume().with_hidden(true).announced();
```

```typescript
const announced = connection.announced(Path.Pattern.all(), { hidden: true });
```

On the wire, moq-lite 07 (`moq-lite-07-wip`, opt-in only) carries the opt-in
on each announce request, and
moq-transport carries it as a `SUBSCRIBE_NAMESPACE` parameter once the peer's
`SETUP` says it understands one ([hidden](/draft/moq-hidden)). An older peer
never opts in, so it never discovers hidden routes. Rust sessions always opt in
on the wire and filter per local reader, so a relay mirrors everything and
each consumer decides.

## Path patterns

Rust's `moq_net::Pattern` and TypeScript's `Path.Pattern` from `@moq/net`
describe sets of literal paths. Patterns match the whole path. `room` matches
only `room`, while `room/**` matches `room` and every descendant. Segments are
separated by `/`:

| Segment | Matches |
| --- | --- |
| `room` | That literal segment. |
| `*` | Exactly one nonempty segment. |
| `camera-*` | One segment starting with `camera-`. |
| `pre*suf` | One segment with that prefix and suffix, without overlapping them. |
| `**` | Zero or more segments. |

A pattern may contain at most 32 segments and one `**`. Leading, trailing,
or repeated separators are invalid pattern syntax. The empty pattern matches
the empty path. There is no escape syntax for a literal `*`.
Construction normalizes adjacent `*` and `**`: `*/**` prints as `**/*`,
so equivalent wildcard placements have the same identity.

Patterns never travel as announcements. `origin.dynamic(prefix, route)`
advertises a prefix: the call claims that `prefix` and every path beneath it
*can* be served, not that any exist. A route is a capability, not an inventory:
a subscriber must not treat a prefix as a concrete broadcast name. Use
`publish(path, route)` when the path is known; use
`dynamic` when the set of paths is not, and refuse the requests you will not
serve. A pattern lives in two places: the token, which scopes what a session
may publish and subscribe to, and a local filter a consumer applies to the
prefixes it is told about.

A subscriber watching under a root sees advertisements named relative to that
root. The pattern scope filters which prefixes are visible without changing a
route's prefix. Announce events carry the covered path, captures, and what
happened to it: Rust `announce::Event::{Announced, Updated, Retracted}`, each
holding an `announce::Announce { prefix, captures: Option<Vec<Pattern>>, route }`,
and TypeScript `Announce.Update { path, captures, route, kind }`, where the kind
is announced, updated (a reprice in place), or retracted. Captures are present
when the announced prefix pins every wildcard in the most-specific matching
scope member. The Rust consumer is a `Stream` and the TypeScript one an async
iterable. The Rust consumer also yields one `announce::Event::Live` once the
routes live at subscribe time have all been delivered, including those a peer
session was still sending: moq-lite-05+ counts them in `ANNOUNCE_OK`,
moq-lite-01/02 send them in `ANNOUNCE_INIT`, and older or IETF sessions wait
for the stream to go quiet.

Announcements are hints; requests are the authority. When a subscriber asks
for a covered path the advertiser will not serve, the advertiser refuses that
request rather than narrowing the claim, and no message narrows a route. Token
scope is any pattern union; the session asks for each member's literal head on
the prefix-only wire and filters locally.

```typescript
import { Path } from "@moq/net";

const scope = Path.Pattern.parse("room/**");
scope.matches("room/alice"); // true
scope.contains(Path.Pattern.parse("room/camera-*")); // true
scope.overlaps(Path.Pattern.parse("*/alice")); // true
scope.rebase("room").toJSON(); // ["**"]
Path.Pattern.parse("camera-*").rooted("room").text; // "room/camera-*"
```

`contains` asks whether every path matched by the other pattern is allowed by
this one. `overlaps` asks whether they share any matching path. `rebase` returns
the matching paths relative to a literal root; `rooted` places a pattern beneath
that root and rejects results exceeding the segment limit. Rebasing can require
several patterns: `**/a` rebased at `a` yields both `""` (the root itself) and
`**/a` (deeper paths ending in `a`).

`Patterns` is a union that removes members contained by another member and
orders the remaining members canonically. Its containment check requires one
member to cover the entire requested pattern; it does not combine several
members to prove joint coverage. Equality compares those reduced members.
Use `specificity` to rank structural constraints when selecting rules, and
`contains` to check whether a rule stays within a scope.

## Subscriptions

A subscriber names a broadcast and track. Delivery starts at the oldest group
it can still use, which at the default budget is the latest one, so every group
must begin at a point a fresh subscriber can decode from (a keyframe, a full
JSON snapshot). Groups can be fetched by sequence number too, optionally
bounded to a range of frames, which is how the [HLS gateway](/bin/hls) and the
relay's [HTTP fetch](/bin/relay/http) serve history.

Each subscription carries the knobs that decide behavior under congestion:

| Knob | Effect |
| --- | --- |
| **Priority** (0..255) | Higher-priority tracks get bandwidth first. Audio above video, base layer above enhancement. |
| **Order** | Which group to send first when several are pending. Newest first for live, oldest first for catch-up. |
| **Max age** | How old a non-latest group may get before it is skipped. Zero means "live edge only", and raising it is also what asks for history. |

Max age is measured on the media timeline, not the wall clock, so a backlog
delivered as a burst is still old while a congestion stall never expires
anything on its own. Both ends apply it: the publisher skips a group rather
than sending it, and the subscriber skips it again as it reads, since the
publisher only ever sees the most tolerant budget across its subscribers.

The publisher may declare a retention window per track. Omission sets no limit;
zero keeps only the live edge. The origin cache ceiling and cache pool may still
evict content sooner. Relays preserve the publisher's declaration rather than
substituting a local default. Media tracks explicitly use 30 seconds so a
segmented egress can still find its segments.

IETF carries this value as MAX\_CACHE\_DURATION, received on every supported draft
and sent from draft 17 onward. Drafts 14–16 remain receive-only for compatibility
with older implementations. This is an approximate mapping: IETF measures wall
time, while max age uses media timestamps and always keeps the newest group.
EXPIRES describes subscription lifetime and does not set retention.

Lite-07 encodes a finite limit as milliseconds plus one, with zero meaning no limit.
Lite-05/06 send no limit as `2^53 - 1` milliseconds so older JavaScript readers can
parse it. New readers treat every value at or above that boundary as no limit;
older readers treat it as a finite window of approximately 285,000 years. Their
timer cap schedules periodic age checks and does not shorten that window.

Put together, a conference might use:

| Track | Priority | Order | Max age |
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

## What moq-lite leaves out

Compared with moq-transport: no request IDs (a stream per request instead), no
push (subscribers always ask), fetches within a single group only, no
sub-groups (use a track per SVC layer), no gaps in object numbering, no per-object metadata
(encode it in the payload), no pausing (unsubscribe instead), and UTF-8 names
instead of byte arrays. When a peer negotiates moq-transport the implementation
still enforces this simpler model, faking or refusing the rest.

| Client | Relay | Works |
| --- | --- | --- |
| moq-lite | moq-lite | yes |
| moq-lite | moq-transport | yes |
| moq-transport | moq-lite | without moq-transport-only features |
| moq-transport | moq-transport | depends on the implementations |

## Protocol errors

Session close codes and stream reset codes use separate registries: session code 0
is a clean close, while stream code 0 is an internal error. Rust preserves received
codes as `moq_net::Error::Session(SessionError)` or `Error::Stream(StreamError)`;
JavaScript exposes `SessionError` and `StreamError`. Match the registry before
interpreting the number. Native bindings expose scope, code, kind, and a diagnostic
message; unknown and application codes retain their numeric value. Transport
failures without a protocol code remain separate.

## Local read limits

Group ranges name which groups a reader may deliver. In Rust,
`set_groups(2..=5)` includes group 5, while `set_groups(2..5)` excludes it.
TypeScript spells the endpoints explicitly:
`reader.setGroups({ start: { included: 2 }, end: { included: 5 } })`.

Changing these local limits preserves read progress. Raising the start skips
lower groups; lowering it never rewinds the reader. Raising or removing the
end cap makes unread buffered groups available again. These local limits do
not change upstream demand; subscription preferences control that separately.
The wire encoding is unchanged.
