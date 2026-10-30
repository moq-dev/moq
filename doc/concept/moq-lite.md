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

Rust and TypeScript speak moq-lite 01 through 06 and moq-transport drafts 18
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

A broadcast is named by a path: non-empty segments joined by `/`, such as
`room/alice`. A path never starts or ends with `/` or doubles one, and a `\`
only escapes a `\` or `/` (how a moq-transport namespace field holding one is
spelled), so each name has one spelling, and every API and peer refuses any
other. The empty path is the root, which prefixes every path.

## Discovery

A session can ask for announcements matching a path prefix. The peer replies
with what it can serve today, then streams changes as they come and go. That is
how a conference room learns who joined, how a player learns a stream came
online without polling, and how [relay clusters](/bin/relay/cluster) discover
each other.

Once that first reply is read, the reader has **synced**: a broadcast not
announced by then is absent rather than late, so a player can say "offline"
instead of waiting. A relay replies from what it holds now and never waits on
its own upstreams. moq-lite 05 and later, and moq-transport with the
[active-count](/draft/moq-active-count) extension, mark exactly where the first
reply ends. Older versions sync on the peer's answer, or as soon as the
request is sent, as a best effort. Reading an origin fed by sessions, a reader
has synced once every current session's first reply is in, so a reconnect
takes it back until the new reply lands.

An announcement is a **route**: a claim that broadcasts at a path prefix, and
every path beneath it, can be served. By convention a publisher announces each
broadcast's exact path, so subscribers enumerate broadcasts by listing routes.
A service can instead announce one short prefix and serve whatever is requested
beneath it, advertising capability without enumerating inventory. A route is
always a prefix. A service that serves only some of the paths beneath it
refuses the rest as they are requested. Announcements are hints; the request is
what decides.

Inside a broadcast, a reader can list its tracks (moq-lite-07): each track
with its fixed properties, a synced mark, then tracks as they
come and go. Listing is discovery, not delivery: subscribe to each track you
want, and its groups still cached upstream reach you late. A broadcast lists
the tracks its publisher creates, never one served only on request, and a path
served under a prefix claim has nothing to list. Like announcements, a relay
syncs on what it holds now, so a fresh relay syncs empty and its upstream's
tracks follow.

Each route carries the chain of relay identities it passed through, which is
how forwarding loops are caught, and a cost, which is how a subscriber picks
among several routes to the same broadcast. It may also carry a
[publisher epoch](#publisher-epochs), which says which routes serve the same
bytes. A hop of 0 is the anonymous mark and travels the chain unchanged. A
route that passed through an anonymous hop at any depth ranks below
every fully identified route, whatever the costs say.

A relay operator can lower the [hop-path cap](/bin/relay/cluster#topology) to
limit the detours explored after a withdrawal. It drops longer paths while
preserving route selection among those within the cap.

A broadcast exists only while it is announced, in the same process and across
a session: one that is created but never announced can be neither discovered
nor requested. Retracting a route stops new requests from resolving through it
and leaves subscriptions already in flight alone until each track ends or its
last subscriber leaves. A graceful session close withdraws its announcements
and waits up to one second for them to be delivered. An abort skips that.

A broadcast has no finished state. Online means announced, and offline means
no route is left. The end of content is a track FIN: the publisher finishes
each track, and a hang broadcast also finishes its catalog. A finished track
stays servable to the subscribers it already has, including through a relay
whose upstream session has since gone. How long a finished broadcast stays
announced for late readers is the application's retention policy, such as the
`--linger` of [`moq import`](/bin/cli), not a protocol state.

### Publisher epochs

A route may carry an epoch: a UUIDv7 naming the publisher instance behind it.
A path and an epoch name one broadcast. The path never changes, so
authorization and hidden names work exactly as without one.

- **One instance, one epoch.** A publisher mints an epoch per run. Replicas of
  the same content announce that same epoch. A prefix claim names no instance:
  its worker answers each requested path by announcing that exact path under
  the output's own epoch. Restarting one output is then a new epoch every hop
  sees, so a viewer returning within a relay's cache window never gets the old
  output's groups.
- **Newest wins.** Among routes at the same prefix, the newest epoch ranks
  first, ahead of cost, and a route without an epoch ranks last. Newest means
  the latest UUIDv7 timestamp, so hosts with skewed clocks can lose a restart
  until the old instance retracts.
- **Same epoch resumes.** A subscription moves between routes with the same
  epoch without a seam, continuing from the first frame it lacks. That is a
  floor, not a promise: groups the outage held back still arrive only while
  the subscriber's max age covers them, so a route that dies silently, noticed
  only at the idle timeout, costs whatever aged out meanwhile. A route
  without an epoch keeps its subscriptions until that route goes, and a track
  that fails ends instead of resuming, so request the path again.
- **Another instance restarts.** A newer epoch, or, without one, another route
  winning the path, is announced as a restart. Subscriptions already answered
  stay on the old instance until dropped or none of its routes remain, which
  ends them with an error, while new requests get the new one, so a player
  follows the restart by requesting the path again. Without an epoch, a request
  still unanswered when another instance takes the path, even after a
  withdrawal, ends the same way, since that instance may be the one answering
  it. A withdrawal alone leaves it to its answer.
  Held handles can still read their old cached content while a subscription
  on the old instance stays open, but cannot open new epochless requests once
  the restart is delivered. Request the path again for
  new content.
  That is how a restarted encoder, whose group numbers start over, takes the
  name instead of stalling viewers on the old sequence. A relay drops its copy
  of the old instance, so a new subscriber never sees its cached groups.

The epoch and the restart message travel on moq-lite 07, which is opt-in.
moq-transport carries the epoch when both sides offer the [epoch
extension](/draft/moq-epoch), on by default. A restart onto another epoch is
advertised before the old one is withdrawn, so the path is never absent, and one
that gains or loses an epoch is an end then a start. Older moq-lite versions
and a side that declines the extension carry no
epoch. Without an epoch, any other route winning is a restart, even a reconnect
over the same path, since nothing says it serves the same bytes. A peer that
ignores the end and start may keep cached objects of the old run under the same
names, so an end and a start is not a cache flush.

`moq` and `moqsink` mint a fresh epoch per run, kept across reconnects, and the
RTMP, SRT, and WHIP ingests mint one per connection. Independent importers use
distinct epochs, even when fed one stream. Only replicas of one publisher's
bytes share its epoch, such as one publisher announced through multiple relays. See [Clustering](/bin/relay/cluster) for
how a relay uses this, and [identity and recovery](/concept/identity) for
choosing epochs, following restarts, and answering work on demand.

### Multiple CDNs

An endpoint can hold sessions to several CDNs at once, each feeding one shared
origin, and rank them: moq.pro as primary, another CDN as secondary. The rank is
a per-link preference the endpoint sets (higher wins, 0 is neutral, negative demotes), never sent on the wire. It decides after
the newest epoch and before cost, so metrics from different operators are never
compared. A subscription uses the preferred link that has a route and moves to the
next when that route goes; a publisher pushes the same broadcast to every CDN.

The move resumes without a seam only between routes with the same epoch, which
needs moq-lite 07, or moq-transport with the epoch extension,
on both links. A CDN link that carries no epoch makes moving onto it, or back
off it, a restart, and the player requests the path again. Both links must
also speak the same protocol and draft: a copy's track info records the
moq-transport draft it crossed, and a copy whose track info differs is refused,
ending the track.

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

## Authorization

On moq-lite 07 (`moq-lite-07-wip`, opt-in) each side presents a token on its
own Auth stream and learns what it may publish and subscribe to: a union of
[path patterns](#path-patterns), delivered exactly as issued rather than widened
to a prefix. Right after setup both sides present the credential the connection
already carried (the URL token, a client certificate, or nothing), so a
publisher learns before anyone subscribes whether its broadcasts can reach the
peer. More tokens can be added without reconnecting; the session's scope is the
union of every open token's grant. A request outside the known union waits
while a non-empty token already presented is unanswered, then proceeds or
fails against the settled union; a covered request does not wait. Each wait
logs its path and duration. To renew without interrupting live requests the
new grant still covers, await the new token's acceptance before dropping the
old one. A token the peer has no verifier for, or
whose grant is too large to send, fails with `Unsupported` and ends only that
token; the session and its other grants stay up. A relay with an
[auth server](/bin/relay/auth#in-band-renewal) verifies these tokens through it,
so a fresh grant can keep the connection alive after its original credential
expires; under public rules or a client certificate it refuses them as
`Unsupported`.

A subscription or fetch that loses access resets with the `UNAUTHORIZED` stream
code and the session stays up. A client that publishes outside its grant closes
the session with `UNAUTHORIZED`, naming the path. A session whose last
credential expires closes with `EXPIRED_AUTH_TOKEN` instead: refresh the token
and reconnect, rather than treating the credential as invalid. moq-transport carries the same
exchange through the [MoQ Auth extension](/draft/moq-auth), with
the same patterns. Older versions have no grant; the URL token keeps working
everywhere. On moq-transport an added token is kept even without the extension,
so an announcement can name it: it rides that `PUBLISH_NAMESPACE` as its
authorization token. Naming a fresh token renews the announcement in place with
`REQUEST_UPDATE`, so it can outlive the token it started with. A token added to a reconnecting
connection's auth handle is presented on every session it makes, so the
announcement keeps it across a reconnect with no re-add or re-announce; one
added to a single session's handle ends with that session. moq-lite has no
per-request token, so there an announcement rides the session's scope.

## Subscriptions

A subscriber names a broadcast and track. Delivery starts at the oldest group
it can still use, which at the default budget is the latest one, so every group
must begin at a point a fresh subscriber can decode from (a keyframe, a full
JSON snapshot). A moq-lite 05 or moq-transport publisher starts at the latest
group whatever the budget, so a subscription there reads older groups only from
a starting group it names. Groups can also be fetched by sequence number, which is how the
[HLS gateway](/bin/hls) and the relay's [HTTP fetch](/bin/relay/http) serve
history.

Native Rust subscriptions, track queries, and announcement interest on
QUIC, WebTransport, or qmux fail locally when the peer has granted no stream
credit, without retrying. Group fills on an accepted track and a subscription's
joining FETCH wait for credit while their readers still want them. Iroh still
waits; browser-backed Rust retains the browser's behavior. Once admitted, these requests have no answer deadline:
each lasts until answered, its demand leaves, or its session closes.
Announcement-interest refusal is logged and releases discovery sync. The merged
announcement listing keeps its existing event and sync API, with no per-source
error result, and continues receiving announcements from unrelated sources.

Each subscription carries the knobs that decide behavior under congestion:

| Knob | Effect |
| --- | --- |
| **Priority** (-128..127, default 0) | Higher-priority tracks get bandwidth first. Audio above video, base layer above enhancement. The publisher's track priority breaks ties between subscriptions at one priority. From a native Rust publisher over QUIC or WebTransport (iroh excepted), subscriptions at the same priority and publisher priority split bandwidth evenly by bytes, streams and datagrams alike, however many groups each has queued. A browser publisher and other transports without send groups are not covered. |
| **Order** | Which group to send first when several are pending. Newest first for live (the default), oldest first for catch-up. Needs moq-lite 07 or moq-transport; older moq-lite always sends newest first. |
| **Max delay** | How long a non-latest group stays worth delivering before it is skipped. Zero means "live edge only", and raising it is also what asks for history. Unset leaves the publisher's retention window alone to bound it. |
| **Ranges** | Which groups to ask for: from a group, a number of groups back from the latest, or the group containing a presentation time, optionally up to an end. Each group arrives whole, from its first frame. The default asks for everything max delay allows. Max delay still caps every range. |

On moq-lite 05 and later, a publisher explicitly reports groups that will not arrive.
This lets a finished subscription settle without waiting for missing groups.
A reader of a reported group, including one still arriving, ends with the publisher's error, such as `OLD` or `NOT_FOUND`.
Before moq-lite 07 that code is application-specific, so every reported group ends with `OLD`.
Datagram delivery stays best effort.

Ranges are both a request and a filter. A subscriber reads only the groups its
own ranges cover, resolved again whenever it replaces them, even when another
subscriber of the same track asked for more. Widening them, including lowering
the start, also serves the cached groups they newly cover, never one already
served. A group they cover below the live feed that the cache lacks (evicted
before the subscriber first looked for it, skipped, or below where the feed
starts) is asked of the track's dynamic handler
as a fill, which a relay serves by fetching upstream one group at a time, skipping
a group once every subscriber that asked for it narrows past it; with
no handler it is reported as dropped once the feed stops waiting for it. One
evicted after the subscriber saw it cached, but before it read it, is skipped. A relay merges its subscribers' ranges into one upstream request, which
moq-lite-07 carries range by range and older wire versions as the single span
covering them. A relative or time anchor resolves on the hop that publishes the
track, and over moq-lite-07, which carries a relative or time start upstream and
reports the live edge and media time, so a copy resolves the ends itself. A Rust
moq-transport copy accepts anchors its cache can already
resolve and sends group bounds upstream; it refuses an unknown time end. Older
moq-lite versions accept absolute anchors but refuse time anchors and relative anchors other
than the live and default starts. An untimed track refuses a time anchor. At the live
edge, a time end
may let one group through before its first frame shows it starts past the end:
the first group that has yet to start is handed out without waiting for that
frame, and the next one waits on it. A datagram is judged by its own timestamp.

Order only decides what goes first, never what is skipped, so catching up oldest
first needs a max delay that covers the backlog. When a relay serves subscribers of
both orders from one upstream subscription, it asks upstream for newest first, and
still sends each subscriber its groups in that subscriber's own order. Over
moq-transport the upstream order is fixed by whichever subscriber arrived first.

Max delay starts counting when a group's successor arrives, on two clocks, and
whichever reaches it first skips the group: the wall clock since the successor
arrived at this hop, and how far the successor's first timestamp trails the
live edge. Media time catches a backlog delivered as a burst; the wall clock
catches a publisher whose timestamps stall and an untimed track. The newest
group has no successor, so it is never skipped. Both ends apply it: the
publisher skips a group rather than sending it, and the subscriber skips it
again as it reads, since the publisher only ever sees the most tolerant budget
across its subscribers.

A track is timed or untimed: it declares a timescale or none, and a frame that
doesn't match is refused. No receiver fills in an arrival time, so an untimed
track is judged by the wall clock alone. Tracks from moq-lite before 05, or from
moq-transport without a `TIMESCALE`, arrive
untimed. moq-lite 05 and 06 cannot declare an untimed track, so there its
frames go out stamped with their send time; the work-in-progress 07 carries it
untimed.

A publisher refuses a group that starts before a lower one, and so does a
moq-lite 07 receiver. Since moq-lite 05 and 06 send times look like media time,
a receiver drops every timestamp it gets over them and the track arrives
untimed.
Timestamps received over moq-transport, whose peers may stamp arrival times,
are not checked. Rust relays preserve them across moq-lite 07 when both peers
negotiate the [ietf extension](/draft/moq-ietf), which carries their origin and
keeps them unchecked. A moq-lite peer without it receives that track untimed.

A publisher may [compress](/concept/compression) a track, which pays off for
repetitive data such as JSON and not for media. Each group's frames become one
DEFLATE stream, which only the work-in-progress moq-lite 07 carries. A relay
forwards it as received and inflates each group once for every other
subscriber, moq-transport included, so readers always see the original frames.

A relay cancels its upstream subscription once nobody subscribes, but keeps its
copy of the track for 30 seconds after the last reader leaves, so a returning
reader or the next fetch finds it. That copy is not live meanwhile: readers get
nothing from it until the source answers again, and its latest group is unknown
until then too, so a player starting at the latest waits for the answer instead
of starting from what the ended feed left. A reader's latest group stays within
the groups it limits itself to, and ends once the track does.

The publisher may declare a retention window per track. Omission sets no limit;
zero keeps only the live edge. Relays preserve that declaration, and may still
evict sooner under their own cache ceiling. Media tracks explicitly use 30
seconds so a segmented egress can still find its segments. Retention ages a
group by the same two clocks as max delay and always keeps the newest group.
A fetch obeys the same window, and may name a tighter max delay of its own.
IETF carries the same idea as `MAX_CACHE_DURATION`, measured in wall time alone.

Put together, a conference might use:

| Track | Priority | Order | Max delay |
| --- | --- | --- | --- |
| audio | 100 | ascending | 500 ms |
| video | 50 | descending | 2 s |

Under light congestion video drops the tail of a group; under heavy congestion
video stops and audio lags by at most 500 ms. No protocol change, just knobs.

### Refresh

A publisher can opt a track into refresh by setting its byte ratio. A new
subscriber asks for a fresh group only when the current group's bytes after
its first frame exceed that ratio times the first frame's size. A ratio of 2,
for example, permits two snapshots' worth of deltas before a join asks for a
new snapshot. Equality does not trigger it, and neither elapsed time nor more
deltas trigger it without a join.

The request does not hold back cached groups: a joiner may receive the old
group before the publisher answers. The ratio is a refresh trigger, not a cap
on the bytes that joiner downloads.

The publisher drives the track's `Demand::wanted`: when it passes the largest
group written, open a new group with a keyframe or complete snapshot. A Rust
moq-json snapshot producer answers this by driving its `refresh()` alongside
updates. It restates the current value at its newest source timestamp, with a
new group number. Existing readers continue into that group too.

Each relay judges a join from its own cache and forwards fresh-group demand
upstream when needed, all the way to the publisher. Preserving the ratio across
sessions requires opt-in moq-lite 07 (`moq-lite-07-wip`) at every hop. Older
moq-lite versions do not carry refresh; moq-transport's dynamic-groups feature
carries it with a ratio of zero, so any join after a delta asks for a new group.
Tracks without a ratio, JSON append streams, and importers that cannot force
keyframes do not use this mechanism.

## Datagrams

Since moq-lite 05, a publisher can send a tiny single-frame group as a QUIC
datagram: unreliable, unordered, under about 1200 bytes, and never
retransmitted. It suits real-time audio and sensor data. There is no stream
fallback, so a datagram that doesn't fit isn't delivered that way.

A datagram shares its subscription's priority and group order with streams,
so a low-priority datagram waits behind higher-priority groups instead of
jumping the queue. From a native Rust publisher over QUIC or WebTransport
(iroh excepted), a full send buffer drops the lowest-priority subscription's
oldest datagram.

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
