# [L] A stalled subscriber holds bounded publisher state

## Goal

What a downstream peer can make a publisher hold is bounded by the protocol's
own shape, not by how long it stalls. A subscriber that withholds stream
credit, or drains slower than the media, across as many tracks as its session
limits admit, makes a relay (or any moq-net publisher) hold group state and
bytes that plateau at what the peer's credit and the latency budget allow,
on both moq-lite and moq-transport. A healthy viewer behind a congested link
still gets the freshest groups.

Non-goals: capping sessions or connections (see Plan), ingress memory
([Bound incomplete messages](/quest/m1/quic/receive-memory.md) owns it), and
new limit constants that only refuse input.

## Plan

Planned 2026-10-19 after the egress audit in #5958, under the rule that
network input is untrusted and its cost is bounded by design (#5961). Facts
as of 83805955a; re-verify, they move.

Where it stands:

- Each group a subscription reaches becomes its own serve machine at once,
  and waits in `Open` for stream credit with no in-flight bound: lite
  `TrackRun::serve` (`rs/moq-net/src/lite/publisher.rs`, around 3722), IETF
  `TrackServe` ("one machine per group, with unlimited concurrency",
  `rs/moq-net/src/ietf/publisher.rs`, around 4002).
- A parked machine leaves only when its group expires. An aborted group never
  does: `expiry_pending` is false once aborted (`model/group.rs`, around 1924
  and 2174), and the `Open` arms check only expiry. The pool's idle window
  (30 s by default) evicts the unread group's bytes, so the machine outlives
  its content and parks forever. When credit finally arrives, it may be spent
  on a group that can no longer be served.
- The serving budget is effectively unbounded. A peer whose wire carries no
  max delay (lite-01/02, every moq-transport draft) is served at about 2^53 ms
  (`MAX_SAFE_DELAY_MS` in both publishers), and a lite-03+ peer names any
  value it likes. Locally only the origin's `cache_duration` (`Duration::MAX`
  unless the relay sets `--cache-duration`) or the publisher's `max_age`
  clamps it.
- Session limits cap subscriptions and fetches (`rs/moq-relay/src/limits.rs`:
  1000 subscriptions and 10000 fetches per client session), never groups in
  flight per subscription.

Abuse scenarios to cover:

1. A subscriber at its subscription cap never grants uni-stream credit. Held
   machines grow with every group published, forever, even after the pool
   frees their bytes.
2. The same peer with an inert pool (no capacity, no idle window, as a library
   user may configure). The pool keeps every group by policy, so judge only
   what the subscriber adds: held state beyond a no-subscriber baseline.
3. A peer grants stream credit but withholds byte credit, so opened streams
   pin their groups past the point they are useful.
4. A lite-03+ peer asks for an enormous max delay so nothing ever expires.
5. The benign twin: a real viewer on a bufferbloated link. The fix must not
   cost it the live edge.

Recommended directions (the implementer settles the shape):

- Make a group's serve state follow credit rather than arrival. One option is
  to open a stream only when the session can grant one and then pick the best
  group still worth sending at that moment, so a parked backlog lives in the
  cache (already billed and evicted) rather than in per-subscriber machines.
  Whatever the shape, cross-subscription priority and per-broadcast fairness
  ([Scope track priority](/quest/m1/track-priority-scope.md)) must still
  decide who gets the next stream.
- A parked group that can no longer be served (aborted, evicted, expired)
  leaves at once and is named to the subscriber where the version can, through
  SUBSCRIBE_DROP.
- The serving budget never exceeds what this hop would retain anyway: clamp
  the requested or defaulted max delay by the local retention (the
  `cache_duration` ceiling, the track's `max_age`, and possibly the pool's idle
  window). Consider whether the relay's default `cache_duration` should follow
  the pool's window instead of `Duration::MAX`; that changes a user-facing
  default, so update `doc/bin/relay/` with it.
- Prefer bounds the protocol already carries (stream credit, max delay,
  `max_age`, the cache pool) over a new per-subscription constant. If a number
  is unavoidable, it must come from one of those.

Sessions: capping them is out of scope. A session costs a handshake, auth, and
the per-session limits above, and a count cap would be a constant that only
throws. Admission control (per-token or per-address session caps) belongs to
the deployment's auth server, which already refuses a connect with a 401 or
403 ([relay auth](/doc/bin/relay/auth.md)); this repo needs nothing more.

Reproduce first. A mock-time sim test (or a `just` drill against a real relay)
with a subscriber that withholds stream credit, or drains below the media rate
behind a deep finite queue, while the publisher writes groups across many
tracks. Measure, over time, live serve machines (pending and opened, as a
gauge) and the bytes they retain outside the pool (batches, chunks, and
unsent transport data), beside pool-billed bytes compared with a
no-subscriber baseline. Only lite counts pending opens today; IETF
`TrackServe` counts streams only after they open, so add the gauge there. On
main they grow with time; done, they plateau. Per the fan-out benchmark rule,
sweep tracks by groups per track, and subscribers by tracks, timing each
stream grant too, so a cost that grows with the table (such as scanning every
subscription when one stream frees up) shows up as a slope. Cover lite and
moq-transport, and scenario 5 still receives the latest group.
Include a benign withholder: a peer bounded by
[Bound incomplete messages](/quest/m1/quic/receive-memory.md) withholds credit
by design under memory pressure, so two relays running both quests must still
plateau and keep the live edge.

Done looks like: those measurements plateau on both protocols and every
scenario above, the regression tests fail on main and run in CI, the JS
publisher in `js/net` is checked for the same shape and fixed or its gap
named in the PR, and the PR reports public API and wire impact (expected:
none on the wire; possibly a relay default).

## Related

- [Bound incomplete messages](/quest/m1/quic/receive-memory.md) - the ingress counterpart: a receiver withholds QUIC credit to bound what it holds, and this quest bounds what the sender holds while it does
- [Rust request credit](/quest/m1/rs-request-credit.md) - requests fail fast without stream credit; publisher group streams are left to this quest
- [Scope track priority](/quest/m1/track-priority-scope.md) - who gets the next stream across subscriptions and tenants
- [Public serving](/quest/m1/public-model/serve.md) - makes group expiry while serving public; coordinate the publisher serve loops with it
- [Treat network input as untrusted](https://github.com/moq-dev/moq/pull/5961) - the rule this quest applies
- [Resumed group head](https://github.com/moq-dev/moq/pull/5958) - the audit that found this, and bounded held heads by the cache pool
