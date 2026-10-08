# [L] A claim's answer carries its broadcast's epoch

## Goal

A broadcast a worker serves in answer to a prefix claim carries its own
epoch, and a relay learns it from the answer. Two things follow for a pool
of claim workers (transcoders) behind relays:

- A worker that closes an output and serves the path again is a new instance,
  so a relay never splices the closed instance's cached groups into it.
- A per-output epoch costs no cut on the first view.

Today a claim names no instance, a relay's session answers a request under a
claim with a placeholder source before anything goes upstream, and no answer
on the wire names an instance, so the relay's front always resolves without
an epoch. A worker that announces its output's exact path with a fresh epoch
to get one restarts the first viewer: the front resolved through the claim,
and the epoch route wins the path and is announced as a `Restart`
([Restart](/quest/m0/broadcast-epoch/restart.md)). A front on a drained
worker no longer needs this: under Restart a request joins a front only while
its route still wins, on every version (re-scoped 2026-10-07). lite-07 only; older versions
and moq-transport keep today's behavior.

## Plan

Decisions (2026-10-07, proposed for the maintainer):

- TRACK_INFO gains an `Epoch`: the instance that answered, or empty. Each hop
  answers with the epoch of the front it served from, so it carries through
  relay chains. SUBSCRIBE_OK doesn't need it: the relay fills the learned
  epoch into later TRACK, SUBSCRIBE and FETCH, and the publisher's existing
  refusal of another epoch catches a restart.
- The epoch lives on the broadcast, since a broadcast is an instance, and
  `Request::accept` reads it from the accepted broadcast rather than taking
  it as an argument.
- A front adopts the epoch it learns. Routes without an epoch never splice
  (maintainer, 2026-10-08, in [Restart](/quest/m0/broadcast-epoch/restart.md)):
  a route change, including a per-path winner change under a prefix pool,
  reaches downstream as a `Restart` of the route, which unsets the cached
  copy of every broadcast nested under it. What that
  can't see is a worker restarting an output under the same claim route;
  there the learned epoch is what a downstream copy names on re-subscribe,
  and a mismatch is refused.
- A refusal of a front's learned epoch means that instance is gone: the front
  ends, and a request that joined it re-resolves onto a fresh front (a new
  TRACK_INFO exchange) instead of failing. Only readers the old instance was
  serving see a cut.
- A front someone reads stays on its instance even when a cheaper route
  appears, as Restart's sticky subscriptions already do: moving it would
  start a second instance at the path. A worker that wants its viewers off
  closes its outputs, and they re-request.
- No dedicated "broadcast closed" message: with the epoch, the next request
  re-resolves anyway, and an exact announcement already gets ANNOUNCE_END.
- The draft's "the Epoch of the route it resolved" and "the route it would
  serve from has another Epoch" become broadcast-scoped; as written, a
  request naming an epoch is refused while the best route is the claim, even
  though that instance is serving. Update the concept doc's epoch section and
  this line's README to match.
- A worker using this should not also announce its output's exact path with
  the epoch: that announcement and TRACK_INFO race on separate streams, and
  the announcement still wins new requests over the claim front and is
  announced as a `Restart`.
- A worker drains feed by feed by closing the outputs it wants off: their
  readers re-request and land on the cheapest worker as a new instance, one
  short cut per feed instead of withdrawing the claim and cutting everything.
- Every link from the worker to the relays that serve viewers must speak
  lite-07, since the epoch is learned and named hop by hop. A viewer's own
  version should not matter, because the join rule and the refusal live at
  relays and workers; confirm it, including that `@moq/net`'s track cache
  never hands a returning viewer the old instance's group.

This is a lite-07 wire change, so it lands before the version is cut.

Verification: a relay integration test with two claim workers (mocked time).
A worker restarting an output within the linger delivers the new instance from
its first group, with nothing stale. A worker answering with a per-output
epoch serves its first viewer without a cut. A worker closing a path while
it is read sends the re-request to the cheaper worker as a new instance. A
request joining an unread front whose worker restarted its output reaches the
new instance without an error. A lite-06 or IETF viewer behind a lite-07
worker-to-relay path, with the old instance's groups still cached, receives
only the new instance's groups after the worker restarts the output with a
new epoch.
`just test interop --all`.

Public API: Rust and JS, the broadcast's epoch and what `accept` does with
it. Wire: lite-07 TRACK_INFO gains `Epoch`.

## Related

- [Restart](/quest/m0/broadcast-epoch/restart.md) - the join rule and sticky subscriptions this builds on
- [Idle fronts](/quest/m0/idle-fronts.md) - ends an unread front and its per-path state after the linger on every version
- [Upstream position regression](/quest/m0/largest-regression.md) - catches a restart on versions without this field
- [Finalize moq-lite-07](/quest/m1/lite07-finalize.md) - waits on this wire change
