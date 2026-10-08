# [M] An unread front ends after its linger

## Goal

A relay ends an origin front once nothing has read it for `track::IDLE_LINGER`,
and the per-path state it holds goes with it, so a standing prefix claim
doesn't keep a front, its driver task, and a session placeholder source for
every path ever requested under it. Today a front lives until its route
leaves. Done when, after the linger, the relay holds no front for an unread
path P and the session no source for it.

Found with a pool of transcode workers claiming one prefix with
`origin::Producer::dynamic`: a drained worker kept serving a path to new
viewers through a relay. That routing half is
[Restart](/quest/m0/broadcast-epoch/restart.md)'s (re-scoped 2026-10-07): a
request joins a front only while its route still wins, so a drained claim's
front takes no newcomers. A replaced front still keeps serving its
subscribers until its route goes, so this quest reclaims it once unread.

Non-goal: a worker that closes an output and serves the same path again
restarting at group 0. That reuses a name for different content, which is the
publisher's bug (see Plan).

## Plan

Facts from `main` (`rs/moq-net/src/model/origin.rs`, `front.rs`,
`lite/subscriber.rs`):

- A front resolved without an epoch stays on its first route (`pick`, #4942).
  `request()` joins any live front whose epoch matches the best route's until
  [Restart](/quest/m0/broadcast-epoch/restart.md) lands; after it, a front
  whose route lost takes no newcomers but stays for its subscribers.
- Under a claim, the session answers a request with a placeholder source it
  creates on the spot (`poll_serve`), kept in the announced route's sources
  until the claim is withdrawn or the session closes. moq-lite has no message
  for "the broadcast served under this prefix closed", so the relay never sees
  `SourceClosed` when the worker closes its output. In-process (no relay) the
  front's source is the worker's broadcast, `SourceClosed` fires, and the next
  request re-resolves; that's why the bug needs a relay.
- Nothing ends a front for having no readers. Its unread tracks park and are
  forgotten after `IDLE_LINGER`, but the front and its driver task stay.
- Each distinct path requested under a claim therefore leaves a placeholder
  source and serve state on the session, and a front on the relay, for as
  long as the claim stands.

Decisions (2026-10-07):

- End every front, epoch or not, once all its tracks are forgotten and no
  consumer holds its broadcast. Ending only after the linger keeps the cache
  window for a returning viewer, needs no wire change, and doesn't conflict
  with the draft's Resume text, which pins subscriptions, not idle fronts.
  A front serving a local source forgets an unread track outright, so it ends
  as soon as it goes unread; reaching a local source again is free.
- A request must never join a front that is ending: the front leaves the
  origin's front table under the lock before it ends (or `request()` treats an
  ending front as gone), so a newcomer gets a fresh front rather than a
  broadcast that closes at once.
- A front still waiting for coverage
  ([Front parking](/quest/m1/origin-front-parks.md)) is not idle and stays.
- The session drops a placeholder source once it has no consumers left, so
  per-path state under a claim goes with its fronts instead of piling up until
  the claim leaves. The route's served cache hands one source to every front
  for the path (a plain front and a peer's filtered front can share it), so
  the trigger is the source losing its last consumer, not one front ending.
  Withdrawing the claim or closing the session still closes the source at
  once, as today, and in-flight consumers drain on their own. The session
  keeps that state in more than one place; all of it goes.
- This also reclaims the filtered front a peer session leaves behind (moved
  here from [Front parking](/quest/m1/origin-front-parks.md)): a hop in a
  covering route chain gets its own filtered front, which today lives as long
  as the route, one per peer session that both publishes and subscribes or
  reconnects with a fresh hop.
- A claim worker that re-serves a path after closing it is a new instance.
  Document that in `doc/concept/moq-lite.md` Resume: such a worker keeps its
  group sequence going, across its own source restarting too (mirroring an
  upstream sequence that goes back to 0 isn't enough), or gives each output its own epoch once
  [Claim-served epochs](/quest/m0/broadcast-epoch/claim-epochs.md) lands.
  Without that, a viewer returning within the
  linger gets the old instance's cached latest group and then nothing until
  the new instance's sequence passes it.

Verification: a relay integration test with two `dynamic` claims on their own
sessions (mocked time): read P, leave, close the output, drain the serving
claim, advance past the linger, and check that the relay holds no front for P
and the first session no source for P. A `front.rs`
unit test for the end condition, including a parked front that must stay,
and an origin test of a request racing a front's end (like `front.rs`'s
`a_reader_racing_the_forget_keeps_the_track`, one level up). The front count
returns to the plain front after a peer session closes.

Public API: none expected. Wire: none.

## Related

- [Restart](/quest/m0/broadcast-epoch/restart.md) - a request joins a front only while its route still wins, the routing half of the same report
- [Claim-served epochs](/quest/m0/broadcast-epoch/claim-epochs.md) - on lite-07, a worker's restarted output is a new instance and never spliced
- [Upstream position regression](/quest/m0/largest-regression.md) - fails loud on the stale splice where the answer shows it
- [Front parking](/quest/m1/origin-front-parks.md) - a front waiting for coverage must survive this; the filtered-front leak moved here from it
- [Route wakes](/quest/m1/route-wakes.md) - indexes fronts per route, so an ended front must drop its entries
- [Front deadline index](/quest/m1/front-deadline-index.md) - indexes a front's per-track linger deadlines in the same `front.rs`; it rebases onto this
- [Broadcast epochs](/quest/m0/broadcast-epoch/README.md) - restarted publishers mint a fresh epoch, the documented fix for a re-served path
