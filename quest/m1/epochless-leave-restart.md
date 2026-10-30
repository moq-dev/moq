# [S] A follower is told when the serving epochless route leaves

## Goal

When the route serving an epochless broadcast is withdrawn, or its session
dies, while another route for the path remains, a follower downstream of the
relay is announced the restart its version carries, such as an end then a
start on lite-06 and older and `ANNOUNCE_RESTART` on lite-07-wip. A re-request
then resumes on the survivor. Integration tests in the merge gate pin this
over the wire, with an epochless route, on lite-04 to 06, lite-07-wip, and
moq-transport 18 and 22.

## Plan

[Identity and recovery](/doc/concept/identity.md) promises that without an
epoch, any other route winning the path is a restart, and players restart only
on the announcement, never on a request error. If a follower is not told, a
player's track subscriptions end while the path stays announced, and playback
freezes until something else changes the announcement. The release line
before [moq-dev/moq#5087](https://github.com/moq-dev/moq/pull/5087) behaves
this way: two epochless publishers on one path, and the serving one stops.
Main implements the restart in the origin's announce cursor (the "another
instance" case), but only
`an_equal_cost_peer_replacement_waits_for_the_old_route` asserts it, at the
model level with no hold and no wire.

Decided in quest-plan (2026-10-30):

- Regression coverage on main, not a fix or a release backport. Reading the code
  shows main already behaves correctly. If a test shows otherwise, fix it in
  this PR, at the relay's announcement, never by splicing the survivor into
  the front or by having players retry on track errors.
- Rust assertion only. `route_dies_without_an_epoch` in
  `rs/moq-net/tests/integration/route_change.rs` already covers the
  `Disconnect` and `Unannounce` triggers over lite-04 to 06 under mock time.
  Its `Topology::new` mints an epoch, which only those versions drop, so it
  is epochless there alone. It checks that the subscription ends and a
  re-request reaches the survivor.
  Extend it, or a sibling sharing its topology, so a follower one wire hop
  past the relay observes the restart for both triggers. Watching
  `announced()` on the relay's own origin only sees its local model event,
  which `better_route_restarts` already covers; the follower must see what the
  relay sends over the wire.
  Lite-07-wip and moq-transport 18 and 22 are required too, through a
  topology that announces without an epoch, as `replacement.rs` already does
  for its newcomer cases. Moq-transport routes all become epochless with
  [moq-transport carries no epoch](/quest/m1/ietf-epoch-cut.md), so this
  path must hold there whichever lands first. Player-level resume is already
  covered on a graceful leave by `just test native-play` (lite-06 publishers) and
  `just test media --media-cases replacement`. A crash differs only in when
  the relay notices, which the `Disconnect` trigger already exercises, so no
  new browser or native harness case.
- Assert the event kind the version can carry, not just "something arrived".
  The `best_route_wins_and_fails_over` helper accepts Update or Start, which
  would hide a regression to an in-place update.

Public API: none. Wire: none.
