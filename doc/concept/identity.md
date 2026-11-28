---
title: Identity and Recovery
description: Choose stream identities so restarts, failover, and on-demand work never mix content from different streams
---

# Identity and recovery

A MoQ name always means the same content. Relays cache groups by broadcast
path, track name, and group start, and a subscriber that loses its route
resumes by asking for the first frame it lacks. Both are only safe while every
source of a name serves the same bytes. This page is the contract publishers,
players, and relays share to keep that true across restarts, failover, and work
started on demand. Wire details live in the [moq-lite](/draft/moq-lite) draft.

## A stream is a path and an identity

A broadcast is named by its path plus its **identity**. Every route in an
origin carries an identity: any non-empty text of at most 256 UTF-8 bytes, room
for a public key, naming one continuous timeline of groups at its path. A group
is addressed by its start timestamp, and within
one identity starts never go back and never name two different groups, so a
reader stitches across every route
and incarnation that shares it. Two identities are two independent streams,
never stitched together. An identity names a run, not who publishes it: it is
no credential, and auth decides who may publish the path.

- **A new stream takes a new identity.** A broadcast that names no identity
  generates a random one when it is created and keeps it while it is withdrawn and announced again or
  reconnects, since its timeline continues. A restarted process, or anything
  that starts its timestamps over, is a new broadcast with a new identity.
  Replaying a recording is a new stream too: [`moq import archive`](/bin/cli#archive)
  republishes under a new identity.
- **Replicas share it.** One stream carried through several relays or CDNs
  keeps its identity on every route, which is what lets a subscription move
  between them.
- **A stable identity is a promise.** A publisher may name its own identity
  (`RouteOptions::with_identity(identity, epoch)` in Rust,
  `origin.route(path, { identity: { id, epoch } })` in JavaScript) only if its timestamps keep moving forward across restarts and
  announces a higher **epoch** each time, for example a counter persisted on
  disk. Breaking the promise splices unrelated groups into every reader's
  tracks. Two encoders with identical settings fed the same source still
  cut and stamp their groups independently, so never derive an identity from
  a hash of settings.
- **Reusing a name is a bug.** Publishing different content under an identity,
  a track name, or a group start that already named something else
  corrupts every cache that holds the old content, and MoQ has no ETag-style
  invalidation. Use a new identity for a whole new stream, a new track name for
  one rendition whose decoder configuration changes (see
  [hang](/concept/hang#catalog)), and keep timestamps moving past the old
  track's when a track is replaced within one stream.

Think of a cloud instance ID that survives a reboot: the identity stays the
same, and the epoch counts the boots. The epoch is a counter within one
identity, never compared across identities and never shown to readers. It only
picks the live incarnation of a stable identity while a stale route lingers:
within one identity, only its highest epoch is eligible, however cheap an older
one is, so it fences out an older run. Nothing assumes it is a clock.

| Situation | Identity | What readers see |
| --- | --- | --- |
| One broadcast, reconnecting | The one it generated | The same stream; subscriptions resume |
| One stream carried by several relays or CDNs | Shared by every route | Failover resumes from the first missing frame |
| A restarted or replaced publisher | A fresh one | A restart, then the new stream from its start |
| A publisher whose timestamps continue across restarts | Stable, with a higher epoch each run | The same stream; readers continue without a seam |
| Independent publishers of one source | One each | Two streams; see [redundant ingest](/setup/redundancy) |
| A pool of hosts producing paths on demand | One per host, carried by its jobs; a fresh one per restart | See [work on demand](#work-on-demand) |

## Choosing a route

A route resolving a path is chosen in this order:

1. The most specific prefix wins.
2. Within one identity, only its highest epoch is eligible, even over a
   cheaper route. Readers continue across an epoch change without a seam,
   because the timeline continues.
3. Among eligible routes: a route that still reaches its publisher (an
   unavailable route, held while the network may recover a path, ranks below
   every other), then one from a peer that has not sent GOAWAY (a draining
   peer's routes rank below every other available route), whatever their
   priority or cost. Both serve only as a last resort, so a restarted
   publisher's lingering identity never takes the path back. Then the highest
   priority, which on
   moq-lite 07 is the lowest of any link along the route's path, then an
   identified chain
   over an anonymous one, then the lowest cost, then a broadcast published on
   this origin, then the shortest hop chain, then a hash of the full requested
   path with the identity, which spreads a pool's paths over its hosts. Relays
   agree on the hash only where every hop carries the identity, which takes
   moq-lite 07. A route a relay learned over an older wire, and holds as no
   moq-lite 07 claim, is not hashed, since that relay minted its identity: the
   route in use keeps the tie. A route that entered a node for exactly the
   requested path is not hashed: the newest one wins, so a restarted publisher
   takes over from its lingering session at once.

`Consumer::request` in Rust and `origin.request` in JavaScript resolve by this
order, and so does every relay deciding what to advertise.

## Routes over older wires

Identities and epochs travel only over moq-lite 07 (`moq-lite-07-wip`,
opt-in). Older moq-lite versions and moq-transport carry neither, so the
receiving session mints an identity for each announcement it receives, with
epoch 0. Whether an identity was minted is a flag beside it
(`Identity::is_minted` in Rust, `Identity.isMinted` in JavaScript) that only a
session sets: parsing text always names an identity, whatever the text, and
moq-lite 07 carries the flag, so a relay forwarding a route keeps it. A minted
identity names the announcement, not the publisher's stream, so:

- An announcement ending and starting again, such as a job re-announced on one
  session, is a new stream, never spliced to the earlier one.
- Continuity does not survive such a hop: a reconnect of the same publisher
  through it reads as another stream. Do not rely on seamless failover through
  an older hop.
- To pin a request to a minted identity, pass back the identity the route
  reported. Its text parses to a different, named identity, which never
  resolves to that route.
- A pool reached through an older-lite relay hop can pick different winners
  per relay, since each relay mints its own identities. Use moq-lite 07
  between relays.

An end followed by a start is not a cache flush. A third-party moq-transport
cache that ignores them may keep the old run's objects under the same names.
Relays built on `moq-net` or `@moq/net` drop their copy of a replaced stream,
so a new subscriber never sees its cached groups.

## Readers: follow or hold

`request(path)` resolves one identity and returns its broadcast. Another route
of the same identity replacing its route, including a higher epoch, is
invisible to it: the broadcast moves there and subscriptions continue from the
first frame they lack. Another identity winning the path never substitutes
content into it: subscriptions already open stay on the old stream until its
last route goes, then end with an error. New requests resolve the winner on a
fresh broadcast. Choose between two kinds of reader.

**Follow the path** when the reader should outlive any one stream, as a
player does. A follower hands out a new broadcast each time another identity
takes the path; an epoch change or another route of the same identity hands
out nothing. Start on each broadcast, drop all decoder and buffer state when the
next one arrives, and wait while the path is offline. Rust uses
`consumer.follow(path, None)`, whose `available()` returns `None` once the path
goes offline; JavaScript `origin.follow(path)`, whose `active` is undefined
between identities; the moq-ffi bindings `follow(path)`, whose `available()`
returns null once the path goes offline; and C `moq_origin_follow`, whose
callback reports `MOQ_FOLLOW_EVENT_OFFLINE`.
An announcement listing (`announced()`) only reports a path becoming present or
absent, whether the origin withdrew it or this side let go of it: an identity
change keeps it present, so a reader that needs one holds a follower. `@moq/watch`, `moq play`, and `moqsrc` follow this way, and
`moq export ts --stitch` can splice a replacement as a marked program switch. A
request failing is never the signal to restart; the follower handing out a new
broadcast is. Nor is a track's error kind: a track withdrawn
unfinished ends `Unroutable` over moq-lite but `Internal` over moq-transport,
whose wire has no code for it, and one its publisher releases unfinished ends
`Internal` over both.

**Hold one stream** when the output must not splice two, as an exporter or
recorder does. Rust and JavaScript `request(path)` and `moq_mux::Source` keep
the first broadcast they resolve for their whole life, then end when it does.
Request the path again for the next stream.

A catalog can reference renditions in a sibling broadcast. Followers restart
that rendition on each broadcast of the sibling, and holders pin the first one
they resolve. See [renditions in another broadcast](/concept/hang#catalog).

### Expecting a particular stream

| Expectation | Over moq-lite 07 | Over an older wire |
| --- | --- | --- |
| Request pinned to an identity (`request(path, Some(identity))`, `request(path, { identity })`) | Only that identity, over any of its routes; another is refused as unroutable | Only the identity this session minted |
| No expectation | Whichever identity wins | Whichever announcement wins |

The identity also travels on requests: on moq-lite 07, TRACK, TRACKS,
SUBSCRIBE and FETCH name the identity their route announced, and a publisher
or relay now serving another refuses them rather than answer with the wrong
stream. A catalog names no identity: its references follow or pin whichever
stream the announcements resolve.

## Work on demand

A worker can advertise a prefix it serves on demand and answer with a
broadcast it already holds. This announces no exact route: the answer belongs
to the claim's identity at that path. Every answer there must continue
one timeline with the same bytes. To serve different content at a path
already served, start a new claim. Its first subscription stays on
the claim without a restart.

A job that may restart independently, such as a transcoder claiming
`transcode`, instead creates each requested path's own exact route. A claim
promises capability, not content. Each requested path is a separate job,
announced at its exact path.

```rust
let claim = origin.claim("transcode", RouteOptions::default())?;
while let Some(request) = claim.requested().await {
    // Takes the claim's identity, so readers resolved through the claim continue.
    // Hidden until its tracks exist.
    let job = request.route(RouteOptions::hidden())?;
    let video = job.broadcast().create_track("video", info.clone()).context("video exists")?;
    // Announces the exact path under that identity and answers the waiting request.
    job.announce()?;
    spawn(run_job(video, job));
}
```

JavaScript mirrors this with `request.route({ hidden: true })` and
`job.announce()`; see [`@moq/net`](/lib/js/net) and [`moq-net`](/lib/rs/moq-net).
A hidden job route is unannounced until it announces, and dropping it before
then refuses the request. It takes the claim's identity when it announces, or
earlier when its identity is first read, as a signer built from it does. Read
first, the job announces under it or not at all: if another job under the claim
ends in between, moving the claim to a fresh identity, `announce` fails with
`Unroutable` and the request reaches the worker again. Ending a job's route withdraws its path, and the
claim keeps answering later requests.

A lazy job's lifecycle, as a viewer of `transcode/alice` sees it through a relay:

1. **Request.** The relay resolves the path through the claim, under the
   claim's identity, and forwards the request. The worker starts a job, whose
   route takes the claim's identity and publishes `transcode/alice` under it.
2. **Continue.** The job's exact route carries the identity the viewer
   resolved, so on moq-lite 07 every relay keeps the viewer on it: it keeps
   playing, and a Rust or JavaScript follower is handed nothing new. Older
   wires mint an identity per announcement, so there the first subscription
   through the claim is reset: a follower is handed the new broadcast, and a
   holder's track ends with an error, and it requests again. The same happens
   on any wire under a claim that names a stable identity, whose jobs each take
   a fresh one, since a restarted job could not keep that promise.
3. **Steady state.** Requests resolve the job by its exact route. Routes
   carrying its identity through other relays fail over without a seam.
4. **Job restart.** The worker drops the job's route, and the claim moves to a
   fresh identity at once, so readers see a restart. The next request starts a
   new job under the claim's new identity, a new stream; a request pinned to
   the old identity is refused as unroutable. Relays drop the old job's cached
   groups, so a viewer returning within the cache window never gets them.
   Other running jobs, such as `transcode/bob`, keep their identity, while
   paths the claim answered with a held broadcast resolve afresh under its new
   one.
5. **Done.** Closing the last job leaves only the claim, which serves the next
   request as in step 1.

### Pools

Several hosts can claim one prefix, such as a set of transcoders or control
nodes. Each claim has its own identity, so step 3 of [choosing a
route](#choosing-a-route) spreads the paths under the prefix over the hosts,
and every relay picks the same host for a path. A host that is full withdraws
its claim, like a leaky bucket: the paths it served through the claim move to
another host's identity, so their broadcasts end and `follow` yields the other
host's, while paths another host won stay where they are. A crashed host's
claim goes the same way when its session closes or drains.

## Recovering content

Within one stream, recovery is about which groups are still worth having.

- **Subscriptions and fetches.** A subscription follows the live track and,
  after a route change within its identity, continues from the first frame it
  lacks. A fetch asks for one group by its start, which a relay fills upstream
  when its cache misses. Both are bound to the identity they resolved.
- **Publisher max age bounds freshness; a live join's max bounds interest.** A
  track's `max_age` (`maxAge` in JavaScript, `--max-age` on `moq import`) is the
  longest a non-latest group stays fresh at any hop for late joiners, fetches,
  and recovery; a relay may cap it lower, and its cache may reclaim the bytes
  sooner. A live join's `max` is how long a non-latest group it reaches
  stays worth delivering to that reader. It can tighten the publisher's bound,
  never relax it, and a range asking for history has none. Both age a group from
  its successor's arrival and never make the newest group stale. Not every wire
  carries a join's max; see [subscriptions](/concept/moq-lite#subscriptions).
- **Arrivals are unordered.** Each group is its own stream, so a lower group
  can land after a higher one, and is still delivered if the reader's range and
  budget cover it. Group order chooses what is sent first, not what is skipped.
- **Duplicates are dropped.** A reader receives each group start once,
  however many routes deliver it or however often a producer re-creates it. A
  start names one group, so resending one cannot correct it.
- **A drop ends the subscription's copy of a group.** Where the wire reports
  drops, the publisher names each group a subscription will not get, and every
  reader of the live copy, even one still arriving, ends with the publisher's
  error. A fetched copy of that group may still complete, so a drop is no
  reason to discard one. Elsewhere a reader gives a missing group up after a
  grace. The
  [moq-lite draft](/draft/moq-lite) lists which versions report drops and why.

moq-transport's FETCH `FILL_TIMEOUT` bounds how long a hop waits on upstream,
not how old the content it returns may be, so it is no substitute for max age. Support differs by library; see
[moq-transport compatibility](/concept/standard).

## Relays

A relay that keeps this contract:

- Chooses routes in the [order above](#choosing-a-route), so every relay
  resolving a path agrees on its stream.
- Moves a subscription only between routes of the same identity, never onto
  another stream.
- Refuses a request naming another identity than the route it would serve from.
- Mints an identity for every announcement it receives without one, so it
  holds no route without an identity.
- Drops its copy of a stream once another replaces it at the path.

See [clustering](/bin/relay/cluster) for how `moq-relay` applies this across a
mesh, and [redundant ingest](/setup/redundancy) for choosing between publisher
switching, subscriber selection, and several routes to one publisher.
