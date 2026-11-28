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
carries [publisher identities](#publisher-identities).

## Terminology

| moq-lite | Meaning | moq-transport name |
| --- | --- | --- |
| **Session** | One connection, publishing and subscribing at once. | Session |
| **Origin** | The set of broadcasts visible to a session, scoped by the URL path. | (none) |
| **Broadcast** | A named, discoverable collection of tracks from one publisher. | Namespace |
| **Track** | A live sequence of groups, delivered out of order until closed. | Track |
| **Group** | A sequence of frames delivered reliably and in order, on its own QUIC stream. | Group |
| **Frame** | A sized chunk of bytes. | Object |
| **Datagram** | One unreliable payload of a datagram track, sent as a QUIC datagram. | Datagram |

A broadcast is named by a path: non-empty segments joined by `/`, such as
`room/alice`. A path never starts or ends with `/` or doubles one, and a `\`
only escapes a `\` or `/` (how a moq-transport namespace field holding one is
spelled), so each name has one spelling, and every API and peer refuses any
other. An escaped `/` stays inside its segment: `room/a\/b` has two segments,
one per namespace field. The empty path is the root, which prefixes every path.

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
served under a prefix claim has nothing to list. A relay lists its upstream's
tracks as they arrive and syncs when its upstream does, so a track listed after
the synced mark is new at the source, through any number of hops. A moq-transport
upstream never signals a complete list, so a relay listing one syncs at once
with the tracks it already knows.

Each route carries the chain of relay identities it passed through, which is
how forwarding loops are caught, and a cost, which is how a subscriber picks
among several routes to the same broadcast. On moq-lite 07 it also carries a
[publisher identity](#publisher-identities), which says which routes serve the same
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
each track, and a hang broadcast also finishes its catalog. A track can also
finish with an error, such as a replayed recording whose source failed: readers
get its groups, then the error. Only moq-lite-07 carries that error across a
hop; older versions reset instead. A finished track stays servable to the subscribers it already has, including through a relay
whose upstream session has since gone. How long a finished broadcast stays
announced for late readers is the application's retention policy, such as the
`--linger` of [`moq import`](/bin/cli), not a protocol state.

### Publisher identities

Every route carries an identity: any non-empty text of at most 256 UTF-8 bytes,
such as a public key, naming one continuous timeline of groups at its path,
plus an epoch, a counter within that identity, like a cloud instance ID that
survives a reboot and a count of its boots. A path and an identity name one
stream. The path never changes, so authorization and hidden names work exactly
as before.

- **One stream, one identity.** Every broadcast mints a random identity and
  keeps it across reconnects. Replicas of the same content announce that same
  identity. A prefix claim is its own stream: its worker answers each requested
  path by announcing that exact path under the claim's identity, so on
  moq-lite 07 a viewer resolved through the claim keeps playing (a claim that
  names a stable identity gives each output a fresh one instead). When that
  output ends, the claim takes a fresh identity, so a restarted output is a
  new stream every hop sees, and a viewer returning within a relay's cache
  window never gets the old output's groups. A different worker has its own
  identity, so it never inherits those viewers.
- **Most specific, then highest epoch, then rank.** The most specific prefix
  wins. Within one identity only its highest epoch is eligible, ahead of cost.
  Among identities the order is link priority, cost, a broadcast published
  locally, hop length, then a hash of the identity and the path, so relays
  agree and a pool's paths spread over its hosts. See
  [choosing a route](/concept/identity#choosing-a-route).
- **Same identity resumes.** A subscription moves between routes with the same
  identity without a seam, continuing from the first frame it lacks. That is a
  floor, not a promise: groups the outage held back still arrive in full for a
  history range, but for a live join only while its max covers them, so a
  route that dies silently, noticed only at the idle timeout, costs a live
  viewer whatever aged out meanwhile.
- **Another identity restarts.** Another identity winning the path is
  announced as a restart. Subscriptions already answered stay on the old
  stream until dropped or none of its routes remain, which ends them with an
  error, while new requests get the new one, so a player follows the restart
  by requesting the path again. That is how a restarted encoder, whose
  timestamps start over, takes the name instead of stalling viewers on the old
  timeline. A relay drops its copy of the old stream, so a new subscriber never
  sees its cached groups.

The identity and the epoch travel on moq-lite 07, which
is opt-in. `ANNOUNCE_START` carries the identity and epoch, `ANNOUNCE_UPDATE`
a higher epoch, and `TRACK`, `TRACKS`, `SUBSCRIBE` and `FETCH` name the identity they
pin. A restart onto another identity, or a higher epoch of the same one, is
advertised before the old one is withdrawn, so the path is never absent. Older
moq-lite versions and moq-transport carry no identity, so the receiving session
mints one for each announcement, flagged as minted: a reconnect or a
re-announcement through such a hop is another stream, and the first
subscription through a claim is reset once the output's route arrives. A peer
that ignores the end and start may keep cached objects of the old run under
the same names, so an end and a start is not a cache flush.

Independent importers use distinct identities, even when fed one stream. Only
replicas of one publisher's bytes share its identity, such as one publisher
announced through multiple relays. See [Clustering](/bin/relay/cluster) for
how a relay uses this, and [identity and recovery](/concept/identity) for
choosing identities, following restarts, and answering work on demand.

### Multiple CDNs

An endpoint can hold sessions to several CDNs at once, each feeding one shared
origin, and rank them: moq.pro as primary, another CDN as secondary. The rank is
a per-link priority the endpoint sets (higher wins, 0 is neutral, negative demotes), unrelated to a subscription's priority. It decides
among identities before cost, so metrics from different operators are never
compared. After the earlier keys of [choosing a route](/concept/identity#choosing-a-route), a subscription uses the
route with the highest priority, on moq-lite 07 the lowest of any link along its path, and moves to the
next when that route goes; a publisher pushes the same broadcast to every CDN.

The move resumes without a seam only between routes with the same identity,
which needs moq-lite 07 on both links. A CDN link that carries no identity
makes moving onto it, or back off it, a restart, and the player requests the
path again. Both links must
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

## Grants and filters

A grant, what a token or an origin scope lets a session publish and subscribe
to, is a set of path prefixes. A prefix covers its path and everything beneath
it: `room` covers `room`, `room/alice`, and `room/alice/cam`, but not `roomy`.
The empty prefix covers everything. A grant holds no wildcard: `room/**`, `**`,
`*`, or `cam-*.hang` is refused where the grant is made, so write `room` for
`room/**` and the empty prefix for `**`. Tokens and auth server grants spell a
prefix as `room/**` or `**` until releases before 0.22 retire, since those read
a bare member as one path. Since every grant covers
whole subtrees, a route a grant admits always serves whole subtrees too: a claim
broader than its publisher's grant is announced at each granted prefix beneath
it instead, so a claim at `room` through a grant of `room/alice` is announced at
`room/alice`, and no reader is shown it where it cannot serve. Each relay it
crosses does the same with the grant it takes the claim under.

Patterns are discovery filters: they pick which announcements a consumer lists,
and never widen or narrow what it may request. They never travel as
announcements: the wire carries prefixes, and each side filters locally.

A pattern matches the whole path. `room` matches only `room`, while `room/**`
matches `room` and every descendant. Segments are separated by `/`:

| Segment | Matches |
| --- | --- |
| `room` | That literal segment. |
| `*` | Exactly one nonempty segment. |
| `camera-*` | One segment starting with `camera-`. |
| `pre*suf` | One segment with that prefix and suffix, without overlapping them. |
| `**` | Zero or more segments. |

A pattern may contain at most 32 segments and one `**`. A literal escapes a `/`
the way a path does, so `room/a\/b/*` names a namespace field holding a `/`;
there is no escape for a literal `*`. The empty pattern matches the empty path.

A subscriber watching under a root sees advertisements named relative to that
root. When several routes advertise one prefix, each reader sees the best route
its scope can use. A listing reports only routes whose literal name matches its
filter: under `**/*.chat`, a claim at `room` is left out even though it could
serve `room/lobby.chat`, and so is a route above the root. Requests still
resolve through such covering routes, and a session still forwards them to its
peer, which routes through them. Code that routes or forwards opts in with the
origin consumer's covering view: `covering()` in Rust, `{ covering: true }` in
JS.

## Authorization

On moq-lite 07 (`moq-lite-07-wip`, opt-in) each side presents a token on its
own Auth stream and learns what it may publish and subscribe to: a set of
[path prefixes](#grants-and-filters), delivered exactly as issued. Right after setup both sides present the credential the connection
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
and reconnect, rather than treating the credential as invalid. Older versions
and moq-transport have no grant; the URL token keeps working everywhere. On
moq-transport an added token is kept for the requests that name it, so an
announcement can name it: it rides that `PUBLISH_NAMESPACE` as its
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
a starting group it names. Groups can also be fetched by start, which is how the
[HLS gateway](/bin/hls) and the relay's [HTTP fetch](/bin/relay/http) serve
history.

Native Rust subscriptions, track queries, and announcement interest on
QUIC, WebTransport, or qmux fail locally when the peer has granted no stream
credit, without retrying. Group fills on an accepted track and a subscription's
joining FETCH wait for credit while their readers still want them.
Browser-backed Rust retains the browser's behavior. Once admitted, these requests have no answer deadline:
each lasts until answered, its demand leaves, or its session closes.
Announcement-interest refusal is logged and releases discovery sync. The merged
announcement listing keeps its existing event and sync API, with no per-source
error result, and continues receiving announcements from unrelated sources.

Each subscription carries the knobs that decide behavior under congestion. Priority and
order belong to each range, so one subscription can ask for the live edge urgently and
backfill history in the background:

| Knob | Effect |
| --- | --- |
| **Priority** (-128..127, default 0) | Higher-priority ranges get bandwidth first. Audio above video, base layer above enhancement. The publisher's track priority breaks ties between subscriptions at one priority. From a native Rust publisher over QUIC or WebTransport, subscriptions at the same priority and publisher priority split bandwidth evenly by bytes, streams and datagrams alike, however many groups each has queued. A browser publisher and other transports without send groups are not covered. |
| **Order** | Which group to send first when several are pending. Newest first for live (the default), oldest first for catch-up. Needs moq-lite 07 or moq-transport; older moq-lite always sends newest first. |
| **Ranges** | Which groups to ask for, through the group containing an end time or onward with the live edge. A range starts at the oldest group or at the group containing a time, which asks for history, or as a live join (the default joins at the live edge). Each group arrives whole, from its first frame. |
| **Max** (live joins) | How long a non-latest group a live join reaches stays worth delivering before it is skipped. Zero means "live edge only", the default. Unset leaves the publisher's max age alone to bound it. History has none: its groups arrive in full, bounded only by the publisher's max age. |

Only moq-lite 07 carries a priority and order per range. Older moq-lite and
moq-transport carry one per subscription: the highest priority among its ranges,
newest first if any range at that priority asks for it.

A **metadata subscription** (`broadcast.meta(name)` in Rust and TypeScript) takes
the same knobs and delivers a track's groups with each frame's timestamp and
declared size, without the payload, to watch a track's progress cheaply. It is a
subscription of its own beside any payload subscription, and never switches mode.
On moq-lite 07 each relay hop forwards it as a metadata subscription, so no
payload crosses a hop for it. Over an older version or moq-transport it rides the
track's one payload subscription and drops the payload, so a track is never
downloaded twice. A reader that wants the payload after all subscribes to the
track from the group it reached and skips the frames it already saw.

A live join is a range starting a lag behind the live edge: the playout buffer
the subscriber holds. It starts at the group containing the live edge minus the
lag, so the buffer arrives full instead of filling in real time, and no fuller;
a zero lag starts at the latest group. Each range joins on its own, so one
subscription can join at several lags. The hop answering measures by media time
on a timed track and by arrival on an untimed one, and fills from history to
reach that group like any time start. A moq-lite 07 relay copy asks its upstream
for each join, with the same lag and end, and places a reader's join only from
an answer that arrived after the reader asked, so a join reads the same groups
through a relay as from the publisher. Readers joining alike share one answer.
Each range is answered on its own, so a relay serves at once a range its copy
placed where its upstream would, while a join in the same subscription waits on
its upstream; a range reaching back to the oldest group waits for where the
copy's feed starts.
Over older moq-lite and moq-transport, a relay places the join from the groups
it holds instead.

A join never starts at a group its max expired, so a max below its lag starts
past the oldest groups the lag reaches, and a relay copy reuses an answer only
for readers no wider than the max it was asked under. A group several ranges cover takes the largest max, and none if
a history range covers it. Over moq-lite 07 an update changes a join's max in
place, keeping where it started. Older moq-lite and moq-transport carry one
subscriber max age instead, so a subscription sends its widest, or none while it
asks for history.

On moq-lite 05 and later, a publisher explicitly reports groups that will not arrive.
This lets a finished subscription settle without waiting for missing groups.
A reader of a reported group, including one still arriving, ends with the publisher's error, such as `OLD` or `NOT_FOUND`.
Before moq-lite 07 that code is application-specific, so every reported group ends with `OLD`.
Datagram delivery stays best effort.

A peer that grants fewer streams than its subscriptions have groups in flight, as a
browser does at about 100, gets them for the highest-priority groups. To make room
the publisher resets the lowest-priority group's stream with `TOO_FAR_BEHIND`, on
every moq-lite version, and from moq-lite 05 also reports that group as dropped.
Its reader ends with that error after the frames that arrived, and no later
stream carries the rest, even for a reader of history. A reader that still
wants the group fetches it, which returns it whole on a stream the subscriber
opens, outside the cap.

Ranges are both a request and a filter. A subscriber reads only the groups its
own ranges cover, resolved again whenever it replaces them, even when another
subscriber of the same track asked for more. Widening them, including lowering
the start, also serves the cached groups they newly cover, never one already
served or reported. A report is final: raising a join's max afterwards does not
bring back a group reported `OLD`, though a relay whose own reader allows more
asks upstream for it again. A reader that has accounted for every group its bounded ranges cover
ends, and repeated reads end immediately too. Widening the ranges resumes reading.
A moq-lite-07 publisher ends a bounded subscription once it covered it, and the
copy stays cached for the same ranges until an update asks for groups it did not
cover, wider or shifted. A group they cover below the live feed that the cache lacks (evicted
before the subscriber first looked for it, skipped, or below where the feed
starts) is asked of the track's dynamic handler
as a fill, which a relay serves by fetching upstream one group at a time, skipping
a group once every subscriber that asked for it narrows past it; with
no handler it is reported as dropped once the feed stops waiting for it. One
evicted after the subscriber saw it cached, but before it read it, is asked the same
way, once: evicted again before it is read, it is reported as dropped. A relay merges its subscribers' ranges into one upstream request, which
moq-lite-07 carries range by range and older wire versions as the single span
covering them.

A group is addressed by its start: where it starts in the track's timescale,
or a bare number that grows with each group on a track without one. A time
start is the group containing that time, the newest starting at or before it,
and a time end includes the group containing it, so neither waits on a frame.
A group that ended whole at a declared end contains no time past it: a time
start in the pause before the next group starts at that next group.
Older moq-lite versions and moq-transport carry a group ID equal to the start,
so a time goes upstream as that ID, where a start selects the first group at or
after it rather than the one containing it. A live join goes to a moq-transport
publisher as a join at its latest group. A moq-transport subscriber's relative
filter or joining FETCH is served for the current or next group and refused
`NOT_SUPPORTED` for one counting further back, since group IDs here are starts
and no arithmetic on them counts groups. moq-lite 01 and 02 carry no start, so every
subscription there starts at the live group.

A group ends one of three ways: finished at the time it ends, so it is whole;
closed, so every frame written arrives but it may have held more, as when a
relay stops sending a group that went stale; or aborted with an error. A
finished group's end also bounds how long it stays fresh, so the last group of
a paused track still ages out. Over the work-in-progress moq-lite 07 groups
never overlap: a group runs from its start to its end, and the time
between one group's end and the next group's start belongs to no group and is
reported as skipped, so a reader sees a gap at once. Older versions and
moq-transport carry no end.

Order only decides what goes first, never what is skipped, so catching up oldest
first needs a history range, or a live join whose max covers the backlog. When a relay serves subscribers
asking for the same groups at different priorities or orders from one upstream
subscription, it asks upstream at the highest priority, newest first if any of those
asks it, and still sends each subscriber its groups in that subscriber's own order. Over
moq-transport the upstream order is fixed by whichever subscriber arrived first.

A live join's max starts counting when a group's successor arrives, on two clocks, and
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
moq-transport without an `ID_TIMESCALE` (or `TIMESCALE` on pre-draft-23), arrive
untimed. moq-lite 05 and 06 cannot declare an untimed track, so there it goes
out as milliseconds, each frame stamped with its group's start; the
work-in-progress 07 carries it without a timescale, with the same stamps.

A publisher refuses a group that starts before a lower one, and so does a
moq-lite 07 receiver. A group's timestamps never decrease either: writing a frame
below its predecessor fails, so a stream with B-frames stamps decode time and
carries presentation time in its payload, as hang's `cts` container does.
moq-lite 07 sends each after a stream's first as an unsigned delta. A receiver passes on the timestamps it
gets over moq-lite 05 and 06 without checking them, since an older publisher
stamps an untimed track there with send times, which look like media time, and an
older group fetched later would look like a rewind. A relay serves them on to
moq-lite 05 and 06, while a moq-lite 07 or moq-transport draft-23 subscriber gets
an error for a group whose timestamps decrease. On moq-transport draft-23 and later, a timed frame's
timestamp is `Group ID + Object ID - TIMESTAMP_TIE` in the track's `ID_TIMESCALE`;
earlier drafts use `TIMESCALE`, with every frame at its Group ID.

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

The publisher may declare a max age per track: how long a superseded group stays
fresh. On the wire, omission sets no limit; zero makes a group stale as soon as
its successor arrives. Relays preserve that declaration across moq-lite hops, and
may still cap it lower under their own ceiling. The Rust and JS libraries give an authored track
zero, the live edge only, unless its publisher asks for more or for no limit;
media tracks declare 30 seconds, so a segmented egress can still find its
segments. A relay bounds what it keeps with its own cache, not the publisher's
default. Max age ages a group by the
same two clocks as a live join's max, and makes the newest group stale only once it
finished whole and its own end stands in for a successor. A fetch
obeys the same bound and names none of its own.
moq-transport drops it: its `MAX_CACHE_DURATION` would also expire the newest group,
so a hop past one keeps superseded groups only as long as its own cache allows.

Put together, a conference might use:

| Track | Priority | Order | Live join max |
| --- | --- | --- | --- |
| audio | 100 | ascending | 500 ms |
| video | 50 | descending | 2 s |

Under light congestion video drops the tail of a group; under heavy congestion
video stops and audio lags by at most 500 ms. No protocol change, just knobs.

A long group is paced by its fastest reader instead. Each hop receives at most
its stream receive window of a group past its fastest reader, capped at 16 MB
(half the group's tail), then stops reading the group's stream, so the hop
holds about what its upstream peer can have in flight on that stream and
stream flow control pushes back hop by hop to the publisher. A reader
that keeps up loses nothing. A slower reader is never waited on: one that trails
the fastest by more than the group's 32 MB tail reaches a shed frame, and asks
for a new group on tracks that opt into [refresh](#refresh) or ends with
`Evicted`. A group a subscriber has yet to pick up is paced from its start, where
that reader begins. A hop with no reader keeps receiving and shedding, so pacing
never stalls an unwatched track, and the publisher's own writes never wait unless
it asks to. A stopped stream keeps its receive credit, so a hop stops at most as
many streams per connection as fit in half its connection window, and the rest
keep receiving and shedding.

### Refresh

A publisher can opt a track into refresh, so a reader can ask it for a fresh
group: a new keyframe or complete snapshot. Two settings decide when.

A **byte ratio** judges a new subscriber: it asks only when the current group's
bytes after its first frame exceed that ratio times the first frame's size. A
ratio of 2, for example, permits two snapshots' worth of deltas before a join
asks for a new snapshot. Equality does not trigger it.

An **interval** caps how often anyone asks. A request takes effect only once the
newest group arrived at least that long ago; one made sooner waits until then,
and every request made meanwhile is answered by the same group. A track with an
interval and no ratio, such as a one-frame thumbnail, gets a fresh group when a
reader arrives and never more often than the interval, however many readers
ask behind however many relays.

A reader can also ask outright with `refresh()` on its track subscriber
(`Subscriber::refresh` in Rust, `subscriber.refresh()` in TypeScript). On a
track with an interval only the interval judges it; on a track with only a
ratio, the ratio does, like a join. It resolves once the request is sent: after
any interval wait, when whatever serves the reader's copy reads it. On a relay
or a remote reader that is the session forwarding it upstream; on a local
broadcast it is the publisher waiting on its demand. Concurrent requests share
one, and all of them resolve when it is sent. Dropping the future (Rust) or
aborting its signal (TypeScript) before then withdraws the request. It resolves
at once when there is nothing to send, such as a track that does not refresh or
a group still under the ratio, and when a newer group lands first. The answer is
the first group the reader receives that starts after the newest it held, which
may be a group already on its way, so a reader that must see a frame made after
its request marks the frame itself.

A request does not hold back cached groups: the reader receives the old group
before the publisher answers. The publisher waits on the track's
`Demand::refresh` (`demand.refresh` in TypeScript) and opens a new group each
time it fires. A Rust moq-json snapshot producer answers it by driving its
`refresh()` alongside updates. It restates the current value in a new group at
its newest source timestamp. Existing readers continue into that group too.

Each relay judges a request against its own cache and clock and forwards it
upstream when needed, all the way to the publisher. Carrying refresh across
sessions requires opt-in moq-lite 07 (`moq-lite-07-wip`) at every hop. Older
moq-lite versions do not carry refresh; moq-transport's dynamic-groups feature
carries it with a ratio of zero, so any join after a delta asks for a new group.
Tracks that opt out, JSON append streams, and importers that cannot force
keyframes do not use this mechanism.

## Datagrams

A track is either a stream track, made of groups, or a datagram track, decided
when the publisher creates it. A datagram track is a series of QUIC datagrams:
unreliable, unordered, under about 1200 bytes, and never retransmitted. Each
has the datagram track's own sequence number and a timestamp, and no group. It
suits real-time audio and sensor data. There is no stream fallback, so a
datagram that doesn't fit isn't delivered.

Datagram tracks have a namespace apart from stream tracks, so `audio` as
datagrams and `audio` as streams are two independent tracks. A broadcast's
track listing says which kind each track is, and lists datagram tracks only
over the work-in-progress moq-lite 07; elsewhere the application knows the kind
from its catalog or by convention. In
Rust, `broadcast.create_datagrams(name, info)` publishes one and
`broadcast.datagrams(name)` reads it. In JavaScript use
`broadcast.createDatagrams(name, info)` and `broadcast.datagrams(name)`.

A datagram subscription is live from when it starts: it has no ranges, order,
or max, only a priority, which a subscriber can change. A datagram still
waits behind higher-priority traffic instead of jumping the queue. Nothing
caches a datagram, and `FETCH` never returns one, so use a stream track for
anything a late joiner needs.

Datagram tracks need a transport with datagrams, so not WebSocket or TCP. The
draft's [Datagrams](/draft/moq-lite#datagrams) section specifies them, and each
library's API docs say which versions carry them.

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
