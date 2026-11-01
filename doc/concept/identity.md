---
title: Identity and Recovery
description: Choose stream instances so restarts, failover, and on-demand work never mix content from different streams
---

# Identity and recovery

A MoQ name always means the same content. Relays cache groups by broadcast
path, track name, and group sequence, and a subscriber that loses its route
resumes by asking for the first frame it lacks. Both are only safe while every
source of a name serves the same bytes. This page is the contract publishers,
players, and relays share to keep that true across restarts, failover, and work
started on demand. Wire details live in the [moq-lite](/draft/moq-lite) draft.

## Identity is a path and an instance

A broadcast's identity is its path plus its **instance**. Every route in an
origin carries an instance: any non-empty text naming one continuous run of
group numbers at its path. Within one instance, group sequences never restart
and never name two different groups, so a reader stitches across every route
and incarnation that shares it. Two instances are two independent streams,
never stitched together. An instance names a run, not who publishes it: it is
no credential, and auth decides who may publish the path.

- **A new stream takes a new instance.** Every broadcast mints a random one
  when it is created and keeps it while it is withdrawn and announced again or
  reconnects, since its numbering continues. A restarted process, or anything
  that starts its group numbers over, is a new broadcast with a new instance.
  Replaying a recording is a new stream too: [`moq import archive`](/bin/cli#archive)
  republishes under a new instance.
- **Replicas share it.** One stream carried through several relays or CDNs
  keeps its instance on every route, which is what lets a subscription move
  between them.
- **A stable instance is a promise.** A publisher may name its own instance
  (`Route::with_instance(instance, epoch)` in Rust, `announce({ instance, epoch })`
  in JavaScript) only if it continues the sequence space across restarts and
  announces a higher **epoch** each time, for example a counter persisted on
  disk. Breaking the promise splices unrelated groups into every reader's
  tracks. Two encoders with identical settings fed the same source still
  number and cut their groups independently, so never derive an instance from
  a hash of settings.
- **Reusing a name is a bug.** Publishing different content under an instance,
  a track name, or a group sequence that already named something else
  corrupts every cache that holds the old content, and MoQ has no ETag-style
  invalidation. Use a new instance for a whole new stream, a new track name for
  one rendition whose decoder configuration changes (see
  [hang](/concept/hang#catalog)), and continue past the old sequence when a
  track is replaced within one stream.

Think of a cloud instance ID that survives a reboot: the instance stays the
same, and the epoch counts the boots. The epoch is a counter within one
instance, never compared across instances and never shown to readers. It only
picks the live incarnation of a stable instance while a stale route lingers:
within one instance, only its highest epoch is eligible, however cheap an older
one is, so it fences out an older run. Nothing assumes it is a clock.

| Situation | Instance | What readers see |
| --- | --- | --- |
| One broadcast, reconnecting | The one it minted | The same stream; subscriptions resume |
| One stream carried by several relays or CDNs | Shared by every route | Failover resumes from the first missing frame |
| A restarted or replaced publisher | A fresh one | A restart, then the new stream from its start |
| A publisher that continues its sequences across restarts | Stable, with a higher epoch each run | The same stream; readers continue without a seam |
| Independent publishers of one source | One each | Two streams; see [redundant ingest](/setup/redundancy) |
| A pool of hosts producing paths on demand | One per host and one per job | See [work on demand](#work-on-demand) |

## Choosing a route

A route resolving a path is chosen in this order:

1. The most specific prefix wins.
2. Within one instance, only its highest epoch is eligible, even over a
   cheaper route. Readers continue across an epoch change without a seam,
   because the sequence space continues.
3. Among different instances: the preferred link, then an identified chain
   over an anonymous one, then the lowest cost, then a broadcast published on
   this origin, then the shortest hop chain, then a hash of the instance and
   the full requested path. Every relay agrees on the hash, and it spreads a
   pool's paths over its hosts. A route that entered a node for exactly the
   requested path is not hashed: the newest one wins, so a restarted publisher
   takes over from its lingering session at once.

`Consumer::request` in Rust and `origin.request` in JavaScript resolve by this
order, and so does every relay deciding what to advertise.

## Routes over older wires

Instances and epochs travel only over moq-lite 07 (`moq-lite-07-wip`,
opt-in). Older moq-lite versions and moq-transport carry neither, so the
receiving session mints an instance for each announcement it receives, with
epoch 0. A minted instance starts with `~`. It names the announcement, not the
publisher's stream, so:

- An announcement ending and starting again, such as a job re-announced on one
  session, is a new stream, never spliced to its predecessor.
- Continuity does not survive such a hop: a reconnect of the same publisher
  through it reads as another stream. Do not rely on seamless failover through
  an older hop.
- A catalog reference naming an instance cannot be checked against a minted
  one, so there it is best effort.
- A pool reached through an older-lite relay hop can pick different winners
  per relay, since each relay mints its own instances. Use moq-lite 07
  between relays.

An end followed by a start is not a cache flush. A third-party moq-transport
cache that ignores them may keep the old run's objects under the same names.
Relays built on `moq-net` or `@moq/net` drop their copy of a replaced stream,
so a new subscriber never sees its cached groups.

## Readers: follow or hold

`request(path)` resolves one instance and returns its broadcast. Another route
of the same instance replacing its route, including a higher epoch, is
invisible to it: the broadcast moves there and subscriptions continue from the
first frame they lack. Another instance winning the path never substitutes
content into it: subscriptions already open stay on the old stream until its
last route goes, then end with an error. New requests resolve the winner on a
fresh broadcast. Choose between two kinds of reader.

**Follow announcements** when the reader should outlive any one stream, as a
player does. Start on each broadcast, drop all decoder and buffer state on a
restart, and wait on an end. Rust uses `consumer.follow(path, None)` (or
`moq_mux::follow::Instances`), and JavaScript `origin.follow(path)`.
`@moq/watch`, `moq play`, and `moqsrc` follow this way, and
`moq export ts --stitch` can splice a replacement as a marked program switch.
A request failing is never the signal to restart; the announcement is.

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
| Request pinned to an instance (`request(path, Some(instance))`, `request(path, { instance })`) | Only that instance, over any of its routes; another is refused as unroutable | Only the instance this session minted |
| Catalog reference with an `instance` | Only that instance; another ends the rendition | Best effort: reads whatever resolved |
| No expectation | Whichever instance wins | Whichever announcement wins |

The instance also travels on requests: on moq-lite 07, TRACK, TRACKS,
SUBSCRIBE and FETCH name the instance their route announced, and a publisher
or relay now serving another refuses them rather than answer with the wrong
stream. A catalog's `instance` is weaker. It never proves a subscription may
resume across routes, and against a minted instance it cannot be checked at
all. Set it only when you know the stream; omit it for a broadcast produced on
demand, which has none until a worker answers.

## Work on demand

A worker can advertise a prefix it serves on demand, such as a transcoder
claiming `transcode`. A claim promises capability, not content. Each requested
path is a separate job, announced at its exact path under a fresh instance of
its own.

```rust
let claim = origin.requests("transcode")?;
while let Ok(request) = claim.requested_broadcast().await {
    // A fresh instance for this job, never one shared with another path or run.
    let prepared = request.prepare()?;
    let video = prepared.broadcast().create_track("video", info.clone())?;
    // Announces the exact path under that instance and answers the waiting request.
    let output = prepared.publish()?;
    spawn(run_job(video, output));
}
```

JavaScript mirrors this with `request.prepare()` and `prepared.publish()`;
see [`@moq/net`](/lib/js/net) and [`moq-net`](/lib/rs/moq-net).
A prepared answer is unannounced until published, and dropping it unpublished
refuses the request.

A lazy job's lifecycle, as a viewer of `transcode/alice` sees it through a relay:

1. **Request.** The relay resolves the path through the claim and forwards the
   request. The worker starts job A and publishes `transcode/alice` under
   instance A.
2. **Takeover.** Publishing the answer through the claim proves it was job A,
   so job A's route takes over the claim: what the viewer already resolved
   through the claim beneath `transcode/alice` keeps playing, now as instance
   A. A Rust follower sees a takeover rather than a restart (JavaScript still
   reports a start or restart). A different worker, or a job restarted under
   the same claim, never inherits it. Only moq-lite 07 carries the takeover
   across relays. Older wires, and paths where a relay cannot vouch for it,
   send an ordinary announcement instead, so the first subscription through
   the claim is reset there: a follower sees a restart and requests again, and
   a holder's track ends with an error, and it requests again. Either way the
   second request reaches job A.
3. **Steady state.** Requests resolve instance A by its exact route. Routes
   carrying instance A through other relays fail over without a seam.
4. **Job restart.** The worker closes job A's publication. The path falls back
   to the claim, the next request asks the worker again, and job B publishes
   under a fresh instance. Followers see each change as a restart; relays drop
   job A's cached groups, so a viewer returning within the cache window never
   gets them. Other paths under the claim, such as `transcode/bob`, are
   untouched.
5. **Done.** Closing the last job leaves only the claim, which serves the next
   request as in step 1.

### Pools

Several hosts can claim one prefix, such as a set of transcoders or control
nodes. Each claim has its own instance, so step 3 of [choosing a
route](#choosing-a-route) spreads the paths under the prefix over the hosts,
and every relay picks the same host for a path. A host that is full withdraws
its claim, like a leaky bucket: the paths it served through the claim move to
another host's instance, so their broadcasts end and `follow` yields the other
host's, while paths another host won stay where they are. A crashed host's
claim goes the same way when its session closes or drains.

## Recovering content

Within one stream, recovery is about which groups are still worth having.

- **Subscriptions and fetches.** A subscription follows the live track and,
  after a route change within its instance, continues from the first frame it
  lacks. A fetch asks for one group by sequence, which a relay fills upstream
  when its cache misses. Both are bound to the instance they resolved.
- **Publisher max age bounds retention; reader max delay bounds interest.** A
  track's `max_age` (`maxAge` in JavaScript, `--max-age` on `moq import`) is the
  longest any hop keeps a non-latest group for late joiners, fetches, and
  recovery; a relay may evict sooner. A reader's `max_delay` (`maxDelay`) is how long a non-latest group
  stays worth delivering to that reader. It can tighten retention, never relax
  it. Both age a group from its successor's arrival and never expire the newest
  group. Not every wire carries a reader's max delay on every request; see
  [subscriptions](/concept/moq-lite#subscriptions).
- **Arrivals are unordered.** Each group is its own stream, so a lower group
  can land after a higher one, and is still delivered if the reader's range and
  budget cover it. Group order chooses what is sent first, not what is skipped.
- **Duplicates are dropped.** A reader receives each group sequence once,
  however many routes deliver it or however often a producer re-creates it. A
  sequence names one group, so resending one cannot correct it.
- **A drop ends the subscription's copy of a group.** Where the wire reports
  drops, the publisher names each group a subscription will not get, and every
  reader of the live copy, even one still arriving, ends with the publisher's
  error. A fetched copy of that group may still complete, so a drop is no
  reason to discard one. Elsewhere a reader gives a missing group up after a
  grace. The
  [moq-lite draft](/draft/moq-lite) lists which versions report drops and why.

moq-transport's FETCH `FILL_TIMEOUT` bounds how long a hop waits on upstream,
not how old the content it returns may be, so it is no substitute for max age
or max delay. Support differs by library; see
[moq-transport compatibility](/concept/standard).

## Relays

A relay that keeps this contract:

- Chooses routes in the [order above](#choosing-a-route), so every relay
  resolving a path agrees on its stream.
- Moves a subscription only between routes of the same instance, never onto
  another stream.
- Refuses a request naming another instance than the route it would serve from.
- Mints an instance for every announcement it receives without one, so it
  holds no route without an instance.
- Drops its copy of a stream once another replaces it at the path.

See [clustering](/bin/relay/cluster) for how `moq-relay` applies this across a
mesh, and [redundant ingest](/setup/redundancy) for choosing between publisher
switching, subscriber selection, and several routes to one publisher.
