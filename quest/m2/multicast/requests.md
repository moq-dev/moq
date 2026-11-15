# [L] A request treats routing as a hint

## Goal

No subscription, fetch, or track request depends on routing state being
fresh. A relay confirms a request only once its upstream has, and serves
every request for one Instance and track from one upstream subscription,
whichever origin node the requests named. A request whose route ends, is
refused as unroutable, or loops moves to another route of its Instance
instead of ending the track, hanging, or ending with `Unroutable` while its
relay shows the broadcast available. No group crosses one link twice for one
subscription, and the old and new copies of a move overlap for a bounded
time. Tests on real lite-07 relays with mocked time pin each.

## Plan

Decided 2026-11-15 (maintainer, /quest-plan): this is the data half of
"evidence, not timers" ([epic](/quest/m2/multicast/README.md) item 4). With
no hold and no doubt, selected routes may loop or go stale while news is in
flight, and that is safe only if a request fails closed and follows its
Instance. It started as the request loop quest, which this absorbs.

Decided 2026-11-15 (maintainer, on
[moq-dev/moq#8832](https://github.com/moq-dev/moq/pull/8832)): ✅ m2
(rejected: m3). Relays that show a broadcast available while ending readers'
subscriptions should block the milestone.

Decided 2026-11-13 (maintainer, /quest-plan on the BGP failure modes):

- ✅ A fixture on real relays, with no TLA+ for pending requests and
  aggregation (rejected: adding them to the route model).
- ✅ A relay's pending upstream request follows its route when the selected
  next hop changes, even before the request is answered, so a loop clears
  with the routes (rejected: a hop count on every request that fails one at
  the limit, which adds a field to SUBSCRIBE, FETCH, and the other requests
  and turns a loop that would heal into an error the subscriber sees).

Why a request cannot find its own loop: once relays aggregate requests, one
fed around a loop meets a relay already asking upstream and joins it, so
neither a nonce nor a carried path ever returns to its sender. Nothing
errors and nobody is fed. What holds instead: a pending request confirms
nothing downstream, and every relay re-asks when its selected route changes.

Hazards to reproduce first, from the 2026-11-15 audit and the acceptance
fixtures:

- `route_failover::live_mesh_link_failure_keeps_presence` cuts all but one
  of a publishing relay's links on a 34-relay mesh, and 3 readers on relays
  that stayed available end with `Unroutable`. Flip it once they read on.
- An upstream that loses its own route resets the subscription `Unroutable`
  and sends the route news on another stream. If the reset is handled first,
  the front aborts a track that has delivered and never splices it again
  (`rs/moq-net/src/model/front.rs` `track_ended`), while
  `rs/moq-net/src/model/resume.rs` treats an unroutable copy as waiting for
  another route. Found by reading; reproduce it two hops below a live
  upstream that loses its route, and for a fresh TRACK naming a route that
  just ended.
- During a move the old and new upstream subscriptions overlap with no time
  bound if the old route keeps winning, and the newest group is asked for
  again.
- Two route entries for one claim through one session send two SUBSCRIBEs
  that differ only in depth.

The loop fixture: form the loop the churn sweeps found (two unrelated
failures at once, so two relays briefly route a publisher through each
other), subscribe at both while it lasts, then let the routes settle. Both
subscribers receive the next group and no upstream request is left pending.
Cover a request answered before the loop and one still unanswered when it
forms.

Fix in the layer that survives
[the resume redesign](/quest/m4/resume-redesign.md), and run the
`verify-failover` and `verify-resume` skills on what they cover.

Public API: none. Wire: none expected. If a refusal must tell "this route is
gone" from "no such track", that is a lite-07 wire change to report, which is
why this sits in m2.

## Related

- [moq-dev/moq#8734](https://github.com/moq-dev/moq/pull/8734) - its doubt rule lets a publisher leaving three or more relays at once loop a request until the news arrives
- [Resume as producer replacement](/quest/m4/resume-redesign.md) - rewrites the per-reader path some of these hazards live in
