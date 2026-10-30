---
title: Identity and Recovery
description: Choose broadcast identities so restarts, failover, and on-demand work never mix content from different publisher instances
---

# Identity and recovery

A MoQ name always means the same content. Relays cache groups by broadcast
path, track name, and group sequence, and a subscriber that loses its route
resumes by asking for the first frame it lacks. Both are only safe while every
source of a name serves the same bytes. This page is the contract publishers,
players, and relays share to keep that true across restarts, failover, and work
started on demand. Wire details live in the [moq-lite](/draft/moq-lite) and
[epoch](/draft/moq-epoch) drafts.

## Identity is a path and an epoch

A broadcast is its path plus a [publisher epoch](/concept/moq-lite#publisher-epochs):
a UUIDv7 that a publisher instance mints when it starts producing.

- **An epoch names a run, not a configuration.** Two encoders with identical
  settings fed the same source still number and cut their groups
  independently, so each mints its own. Never derive an epoch from a hash of
  settings, or restore one saved by an earlier run.
- **Replicas share it.** One instance's broadcast carried through several
  relays or CDNs keeps its epoch on every route, which is what lets a
  subscription move between them.
- **A new run takes a new one.** A restarted process, or anything that starts
  its group numbers over, mints a fresh epoch. Keep it across reconnects of the
  same run, since the numbering continues. Replaying a recording is a new run
  too: [`moq import archive`](/bin/cli#archive) republishes under a new epoch.
- **Reusing a name is a bug.** Publishing different content under a path and
  epoch, a track name, or a group sequence that already named something else
  corrupts every cache that holds the old content, and MoQ has no ETag-style
  invalidation. Mint a new epoch for a whole new run, a new track name for one
  rendition whose decoder configuration changes (see
  [hang](/concept/hang#catalog)), and continue past the old sequence when a
  track is replaced within one run.

Rust and JavaScript publishers announce with `Route::default().with_epoch(Epoch::mint())`
and `publication.announce({ epoch: Epoch.mint() })`. `moq`, `@moq/publish`, and
the ingest gateways do it for you; see [publisher runs](/bin/cli#publisher-runs).

| Situation | Epoch | What readers see |
| --- | --- | --- |
| One publisher run, reconnecting | Mint once per run | The same broadcast; subscriptions resume |
| One run carried by several relays or CDNs | Shared by every route | Failover resumes from the first missing frame |
| A restarted or replaced publisher | A fresh one | A restart, then the new broadcast from its start |
| Independent publishers of one source | One each | The newest wins the path as a restart; see [redundant ingest](/setup/redundancy) |
| A worker producing paths on demand | None on the claim, a fresh one per job | See [work on demand](#work-on-demand) |

## Routes with and without an epoch

An epoch reaches readers only over hops that carry it: moq-lite 07
(`moq-lite-07-wip`, opt-in) and moq-transport with the
[epoch extension](/draft/moq-epoch), which is on by default. Older moq-lite
versions and moq-transport without the extension drop it. The
[moq-lite](/draft/moq-lite) and [epoch](/draft/moq-epoch) drafts say how each
version signals another instance taking the path.

A relay forwards an epoch unchanged and never invents one. Without one, no
route can prove it serves the same bytes as another, so any other route
winning the path is a restart, even a reconnect of the same run, and a
subscription stays on its route until that route goes. Do not rely on
continuity through such a hop.

An end followed by a start is not a cache flush. A third-party moq-transport
cache that ignores them may keep the old run's objects under the same names, so
only a fresh epoch, carried end to end, keeps a restart from serving stale
groups. Relays built on `moq-net` or `@moq/net` drop their copy of a replaced
instance, so a new subscriber never sees its cached groups.

## Readers: follow or hold

A resolved broadcast handle is sticky: it stays on the instance it resolved.
A route with the same epoch replacing its route is invisible to it. Another
instance winning the path never substitutes content into it: subscriptions
already open stay on the old instance until its last route goes, then end with
an error, and without an epoch the handle opens no new ones once the restart
arrives. New requests resolve the winner on a fresh broadcast.
Choose between two kinds of reader.

**Follow announcements** when the reader should outlive any one publisher
run, as a player does. Start on each instance, drop all decoder and buffer
state on a restart, and wait on an end. Rust uses
`consumer.follow(path, None)` (or `moq_mux::follow::Instances`), and
JavaScript `origin.follow(path)`. `@moq/watch`, `moq play`,
and `moqsrc` follow this way, and `moq export ts --stitch` can splice a
replacement as a marked program switch. A request failing is never the signal
to restart; the announcement is.

**Hold one instance** when the output must not splice two runs, as an exporter
or recorder does. Rust and JavaScript
`request(path)` and `moq_mux::Source` keep the first broadcast they
resolve for their whole life, then end when it does. Request the path again
for the next run.

A catalog can reference renditions in a sibling broadcast. Followers restart
that rendition on each instance of the sibling, and holders pin the first one
they resolve. See [renditions in another broadcast](/concept/hang#catalog).

### Expecting a particular instance

| Expectation | Over a route with an epoch | Over a route without one |
| --- | --- | --- |
| Request pinned to an epoch (`request(path, Some(epoch))`, `request(path, { epoch })`) | Only that instance; another is refused as unroutable | Refused: nothing proves the instance |
| Catalog reference with an `epoch` | Only that instance; another ends the rendition | Best effort: reads whatever resolved |
| No expectation | Whichever instance wins | Whichever route wins |

The epoch also travels on requests: where the route carries one, SUBSCRIBE
and FETCH name the instance their route announced,
pinned or not, and a publisher or relay that now serves another refuses them
rather than answer with the wrong one. A catalog's `epoch` is weaker. It never
proves a subscription may resume across routes, and over an older wire it
cannot be checked at all.
Set it only when you know the instance; omit it for a broadcast produced on
demand, which has none until a worker answers.

## Work on demand

A worker can advertise a prefix it serves on demand, such as a transcoder
claiming `transcode`. A claim promises capability, not content, so it carries
no epoch. Each requested path is a separate job, announced at its exact path
under its own fresh epoch.

```rust
let claim = origin.requests("transcode")?;
while let Ok(request) = claim.requested_broadcast().await {
    // A fresh epoch for this job, never one shared with another path or run.
    let prepared = request.prepare(Epoch::mint())?;
    let video = prepared.broadcast().create_track("video", info.clone())?;
    // Announces the exact path under that epoch and answers the waiting request.
    let output = prepared.publish()?;
    spawn(run_job(video, output));
}
```

JavaScript mirrors this with `request.prepare(epoch)` and `prepared.publish()`;
see [`@moq/net`](/lib/js/net) and [`moq-net`](/lib/rs/moq-net).
A prepared answer is unannounced until published, and dropping it unpublished
refuses the request.

A lazy job's lifecycle, as a viewer of `transcode/alice` sees it through a relay:

1. **Request.** The relay resolves the path through the claim and forwards the
   request. The worker starts job A and publishes `transcode/alice` under
   epoch A.
2. **Takeover.** Publishing the answer through the claim proves it was job A,
   so job A's route takes over the claim: what the viewer already resolved
   through the claim beneath `transcode/alice` keeps playing, now as epoch A.
   A Rust follower sees a takeover rather than a restart (JavaScript still
   reports a start or restart). A different worker, or a
   job restarted under the same claim, never inherits it. Only moq-lite 07
   carries the takeover across relays. Older wires, and paths where a relay
   cannot vouch for it, send an ordinary announcement instead, so the first
   subscription through the claim is reset there: a follower sees a restart
   and requests again, and a holder's track ends with an error, and it
   requests again. Either way the second request reaches job A.
3. **Steady state.** Requests resolve epoch A by its exact route. Routes
   carrying epoch A through other relays fail over without a seam.
4. **Job restart.** The worker closes job A's publication. The path falls back
   to the claim, the next request asks the worker again, and job B publishes
   under a fresh epoch. Followers see each change as a restart; relays drop job
   A's cached groups, so a viewer returning within the cache window never gets
   them. Other paths under the claim, such as `transcode/bob`, are untouched.
5. **Done.** Closing the last job leaves only the claim, which serves the next
   request as in step 1.

## Recovering content

Within one instance, recovery is about which groups are still worth having.

- **Subscriptions and fetches.** A subscription follows the live track and,
  after a same-epoch route change, continues from the first frame it lacks. A
  fetch asks for one group by sequence, which a relay fills upstream when its
  cache misses. Both are bound to the instance they resolved.
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

- Ranks the newest epoch first among routes for one path, ahead of cost, and
  an epochless route last. Epochs order by creation time, so a host with a
  skewed clock can lose a restart until the old instance retracts.
- Moves a subscription only between routes with the same epoch, never between
  epochless routes or onto another instance.
- Refuses a request naming another instance than the route it would serve from.
- Drops its copy of an instance once another replaces it at the path.

See [clustering](/bin/relay/cluster) for how `moq-relay` applies this across a
mesh, and [redundant ingest](/setup/redundancy) for choosing between publisher
switching, subscriber selection, and several routes to one publisher.
