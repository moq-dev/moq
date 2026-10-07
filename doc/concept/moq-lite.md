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
Streams that depend on negotiated capabilities wait for the peer's `SETUP`;
everything else proceeds immediately. A Rust server holds unidirectional streams
that arrive before the client's `SETUP`, since it routes by the path in `SETUP`. A second Setup Stream closes the session
with `PROTOCOL_VIOLATION`.
Rust and TypeScript speak moq-lite 01 through 06 and moq-transport drafts
14 through 22. Clients offer `moq-lite-06` first by default. moq-lite 07 is
still in progress: it negotiates as `moq-lite-07-wip`, and only when both
sides explicitly enable it. moq-lite 07 also switches every varint from QUIC's
two-bit length prefix to moq-transport's leading-ones form, so values up to 127
take one byte instead of up to 63, and the range widens from 62 to 64 bits.
A relay cannot forward a value past 2^62 - 1 to an older peer, so it fails
that subscription or group and keeps the session.

## Subscription completion

On moq-lite 07, `SUBSCRIBE_END` counts the group streams opened for the
subscription. Rust and TypeScript stop waiting for missing streams once that
many headers have arrived; skipped group sequences add no wait. A stream whose
header arrived is always read to its end before the subscription completes.

A stream reset before its header arrived cannot be counted, so the subscriber
still allows a grace period for late streams. The grace uses the subscription's
nonzero effective maximum age, or one second when no maximum age is set.
moq-lite 05 and 06 instead account for group sequences using received headers,
datagrams, and `SUBSCRIBE_DROP`, from the start group the subscription last
asked for. A lost datagram is not owed, but its hole waits out the grace like a
lost stream.

From moq-lite 06, an end that contradicts the groups received aborts the track:
a group at or past `SUBSCRIBE_END`, or a `SUBSCRIBE_END` below a group already
received. moq-lite 05 specified an inclusive end, so there it only drops that
group or the early boundary.

On moq-lite 07, a subscription ends with both sides' FIN. The subscriber sends
its FIN after settling the tail. A publisher closing gracefully waits for that
FIN or a reset before closing the connection, within its close deadline.
moq-lite 05 and 06 retain transport acknowledgement based draining.

## Group reads across failover

Rust origin readers can keep reading an in-flight group after its source fails.
A group reader waiting on a replacement copy subscribes to it from the frame it
needs, even when the caller is not polling for the next group, and holds that
subscription until the group ends. If that copy refuses the subscription, the
read ends with an error. The copy's declared start says whether the group is
still coming, so a newer group overtaking it on the wire does not end the wait.
A copy that declares no start (a local track) and has advanced past the group
ends the read with an error. This also applies when waiting for the group's
completion; no FETCH is issued. A replacement that later drops the group it is
serving still needs SUBSCRIBE\_DROP support to resolve that wait.

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
subscriber picks among several routes to the same broadcast. A route may also
carry a [publisher epoch](#publisher-epochs), which says which routes serve the
same bytes. A hop of 0 is
the anonymous mark and travels the chain unchanged; when it is the first hop, a
relay puts a random ID, fresh per connection, in front of it. A route that passed through an
anonymous hop at any depth ranks below every fully identified route, whatever
the costs say; among anonymous routes, cost keeps ordering.

On moq-lite 07, an announcement may copy the head of its path and the tail of
its relay chain from one still live on the same stream, so many broadcasts
from a few origins behind the same relays stop repeating those bytes. Rust
compresses when it helps; TypeScript decodes it but always sends literally.

A broadcast exists only while it is announced, for consumers in the same
process and across a session alike: one that is created but never announced
can be neither discovered nor requested. A broadcast published locally
competes with remote routes to its path like any other route: a newer epoch
beats it whatever the cost, so a local broadcast without an epoch loses to any
remote route that carries one, and among equal epochs it wins a cost tie.
Retracting a route (an unannounce, or the peer's `ANNOUNCE_END`)
stops new requests from resolving through it but leaves subscriptions already
in flight alone: each track runs to its own end or failure, or until its last
subscriber leaves, which cancels it upstream. On moq-lite 05 and
newer, a clean end requires `SUBSCRIBE_END` before the publisher's FIN. A FIN
without that declaration fails the subscription with `ProtocolViolation`; older
moq-lite versions use FIN alone. moq-transport requires `PUBLISH_DONE` before FIN.
moq-transport sessions behave the same when a namespace is withdrawn. A route
update that changes its first hop, the original publisher, is not a retraction:
subscriptions in flight carry on through it.

A graceful session close withdraws its announcements and waits up to one
second for transport acknowledgement before disconnecting. Rust uses
`session.close().await`; JavaScript uses `await connection.close()` on an
established connection. An abort skips the withdrawal and ends immediately.
The source origin remains usable by other sessions. Acknowledgement confirms
transport delivery, not that the peer application has finished processing it.
IETF drafts 14 through 16 send their withdrawals on the shared control stream
without waiting, since it has no FIN to acknowledge.

### Publisher epochs

A route may carry an `Epoch`: a UUIDv7 naming the publisher instance behind it,
ordered newest last, with its creation time available as `Epoch::time()` in
Rust or `Epoch.time(epoch)` in TypeScript. A path and an epoch name one
broadcast. The path never changes, so authorization, patterns, and hidden
names work exactly as without one.

- **Publishing**: the epoch is whatever the route names, and nothing is minted
  for you. A publisher mints one per run (`Epoch::mint()` with
  `Route::with_epoch`, or `announce({ epoch: Epoch.mint() })`), so a restart is
  a new broadcast. Replicas of the same content announce the same one. A route
  without an epoch, such as a prefix claim, names no instance.
- **Resolution** ranks the newest epoch first among routes at the same prefix,
  ahead of cost, and a route without an epoch last. Newest means the latest
  UUIDv7 timestamp, so ordering instances minted on different hosts trusts
  their clocks: a restart on a host whose clock runs behind loses to the old
  instance until that one retracts.
- **Re-pricing**: announcing again (or `update` on a dynamic route) replaces
  the whole route, epoch included. Start from the current route, as in
  `broadcast.route()` or `dynamic.route()`, and change the cost, or the update
  names another instance.
- **Resume**: a subscription moves between routes with the same epoch without a
  seam, continuing from the first frame it lacks. A route without an epoch
  names no instance, so its subscriptions stay on that route (a transcoder
  claim stays on the worker that first served it) and end when it goes.
- **Replacement**: when a newer epoch wins the path, subscriptions to the old
  one end with `Unroutable`, even ones in flight, and announcement streams see
  the old route end and the new one start. Clients re-request and get the new
  broadcast instead of stalling on the old.

The epoch travels on moq-lite 07 in `ANNOUNCE_START`, and relays fill it into
the `TRACK`, `SUBSCRIBE`, and `FETCH` they send upstream; a publisher refuses a
request naming another epoch. Older moq-lite versions and moq-transport cannot
carry it: their routes have no epoch, so they keep their subscriptions on one
route and see a replacement as an ordinary end and start at the same path. In
a mixed deployment, where the same content reaches a relay over both an older
link (no epoch) and a moq-lite 07 link (with one), the epoch-carrying route
supersedes the other each time it appears, so a flapping moq-lite 07 link cuts
the viewers resolved through the older one.

### Hidden broadcasts

A path segment starting with `.` hides a route from discovery, the way a
dotfile hides from `ls`. A platform publishes its own broadcasts there (relay
stats under `.stats/`) without them turning up in an app that lists
everything and plays what it finds. Only segments
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
`SETUP` says it understands one ([hidden](/draft/moq-hidden)). An older MoQ Lite peer
never opts in, so it never discovers hidden routes.
IETF peers that omit the MoQ Hidden setup option receive all authorized
namespaces, including dot-prefixed namespaces. Peers that declare it opt in
per subscription; a prefix naming the dot segment itself also lists its
children. Rust sessions always opt in
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
route's prefix. When several routes advertise one prefix, each reader sees the
best route its scope can use, so a cheaper route scoped elsewhere never hides
it. Announce events carry the covered path, captures, and what
happened to it: Rust `announce::Event::{Start, Update, End}`, each
holding an `announce::Announce { prefix, captures: Option<Vec<Pattern>>, route }`,
and TypeScript `Announce.Event`, whose `kind` is `"start"`, `"update"`, or
`"end"` alongside the same `Announce.Announce` fields. An update is a
reprice in place. Captures are present when the announced prefix pins every
wildcard in the most-specific matching scope member. The Rust consumer is a
`Stream` and the TypeScript one an async iterable. Neither marks when the
routes live at subscribe time have all arrived: an origin merges many sessions
that connect and reconnect independently, so drive loading and offline UI from
the connection status instead.

Announcements are hints; requests are the authority. When a subscriber asks
for a covered path the advertiser will not serve, the advertiser refuses that
request rather than narrowing the claim, and no message narrows a route. Token
scope is any pattern union; the session asks for each member's literal head on
the prefix-only wire and filters locally. In Rust and TypeScript,
`origin.scope(root, patterns)` narrows the handle's permissions and presents paths
relative to `root`. Nested scopes intersect with their parent. In Rust,
`origin.mount(at, target)` reads the subtree at `at` from `target` instead: a
request for `at/rest` joins the one front at `target/rest`, announcements under
`target` present under `at`, the handle's patterns still authorize `at/rest`,
and nothing is published beneath `at`. Mounts never chain: a mount point that
overlaps another mount's point or any target, its own included, is refused. A session receiving
into that scoped origin asks for the literal heads of its allowed patterns,
coalescing duplicate or nested heads. An unscoped origin still asks for the empty
prefix, covering every namespace. These subscriptions include hidden routes;
each local announcement reader decides whether to show them.

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
| **Max delay** | How far a non-latest group may fall behind the live edge before it is skipped. Zero means "live edge only", and raising it is also what asks for history. |

Max delay is measured on the media timeline, not the wall clock, so a backlog
delivered as a burst is still late while a congestion stall never expires
anything on its own. Both ends apply it: the publisher skips a group rather
than sending it, and the subscriber skips it again as it reads, since the
publisher only ever sees the most tolerant budget across its subscribers.

A route failover is invisible to max delay: a group open across the change carries
on from the new route at the frame where the old one stopped, and a group only a
replaced route that went quiet still holds is given up once it falls a full
budget behind the new route's live edge. A successor group with no timestamp
leaves the preceding group's reach unbounded until its first frame arrives; if
it is dropped first, the next group takes its place.

A relay cancels its upstream subscription once nobody subscribes, but keeps its
copy of the track for 30 seconds after the last reader leaves, so a returning
reader or the next fetch finds its cache. That copy is not live meanwhile:
readers get nothing from its cache until the source answers again, since how
stale it is cannot be told. lite-07 and moq-transport answer with their largest
position, and older lite versions are asked from the head of the newest cached
group, so their first frame says where their feed is. A feed that has moved on
past everything cached leaves the cache below it to fetches.

The publisher may declare a retention window per track. Omission sets no limit;
zero keeps only the live edge. The origin cache ceiling and cache pool may still
evict content sooner. Relays preserve the publisher's declaration rather than
substituting a local default. Media tracks explicitly use 30 seconds so a
segmented egress can still find its segments.

IETF carries this value as MAX\_CACHE\_DURATION, received on every supported draft
and sent from draft 17 onward. A relay reads it from FETCH\_OK as well as SUBSCRIBE\_OK, so a track it
only fetches still learns its window. Drafts 14–16 remain receive-only for compatibility
with older implementations. This is an approximate mapping: IETF measures wall
time, while the publisher's retention window uses media timestamps and always keeps the newest group.
EXPIRES describes subscription lifetime and does not set retention.

Lite-07 encodes a finite limit as milliseconds plus one, with zero meaning no limit.
Lite-05/06 send no limit as `2^53 - 1` milliseconds so older JavaScript readers can
parse it. New readers treat every value at or above that boundary as no limit;
older readers treat it as a finite window of approximately 285,000 years. Their
timer cap schedules periodic age checks and does not shorten that window.

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

Datagrams are live-only. A subscription gets the ones sent after it opened and
inside its group range, never a buffered backlog, and nothing caches them, so
`FETCH` treats a datagram group as one that does not exist. Use a group for
anything a late joiner needs.

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
failures without a protocol code remain separate. A deliberate local close ends
received tracks cleanly after their delivered groups; a peer close ends tracks
and open group readers with the session error.

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
