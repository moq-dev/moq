# [S] A follower is told when the serving instance's route leaves

## Goal

When the route serving a broadcast is withdrawn, or its session dies, while a
route of another instance remains for the path, a follower downstream of the
relay is announced the restart its version carries, such as an end then a
start on lite-06 and older and `ANNOUNCE_RESTART` on lite-07-wip. A re-request
then resumes on the survivor. Integration tests in the merge gate pin this
over the wire on lite-04 to 06, lite-07-wip, and moq-transport 18 and 22.

## Plan

[Identity and recovery](/doc/concept/identity.md) promises that a route of
another instance winning the path is a restart, and players restart only on
the announcement, never on a request error. If a follower is not told, a
player's track subscriptions end while the path stays announced, and playback
freezes until something else changes the announcement. The release line
before [moq-dev/moq#5087](https://github.com/moq-dev/moq/pull/5087) behaves
this way: two publishers without an epoch on one path, and the serving one
stops. Main implements the restart in the origin's announce cursor (the
"another instance" case), but only
`an_equal_cost_peer_replacement_waits_for_the_old_route` asserts it, at the
model level with no hold and no wire.

Decided in quest-plan (2026-10-30), restated for instances when
[moq-dev/moq#7489](https://github.com/moq-dev/moq/pull/7489) replaced epochs:

- Regression coverage on main, not a fix or a release backport. Reading the code
  shows main already behaves correctly. If a test shows otherwise, fix it in
  this PR, at the relay's announcement, never by splicing the survivor into
  the front or by having players retry on track errors.
- Rust assertion only. `route_dies_across_instances` in
  `rs/moq-net/tests/integration/route_change.rs` already covers the
  `Disconnect` and `Unannounce` triggers over moq-transport 18 and 22 and
  lite-04 to 06 under mock time (`pinned_tests!`). Its `Topology::new` names
  one instance, which those versions drop: each session mints its own
  instance per announcement, so routes `A` and `B` are different instances
  there but one instance on lite-07-wip. It checks that the subscription
  ends and a re-request reaches the survivor.
  Extend it, or a sibling sharing its topology, so a follower one wire hop
  past the relay observes the restart for both triggers. Watching
  `announced()` on the relay's own origin only sees its local model event,
  which `better_route_restarts` already covers; the follower must see what the
  relay sends over the wire.
  Moq-transport 18 and 22 get the follower assertion from the shared
  topology. Lite-07-wip is required too, through a topology whose two routes
  carry different instances, such as two publications at the path, since
  lite-07-wip carries one publication's instance on every route.
  Player-level resume is already
  covered on a graceful leave by `just test native-play` (lite-06 publishers) and
  `just test media --media-cases replacement`. A crash differs only in when
  the relay notices, which the `Disconnect` trigger already exercises, so no
  new browser or native harness case.
- Assert the event kind the version can carry, not just "something arrived".
  The `best_route_wins_and_fails_over` helper accepts Update or Start, which
  would hide a regression to an in-place update.

Public API: none. Wire: none.
