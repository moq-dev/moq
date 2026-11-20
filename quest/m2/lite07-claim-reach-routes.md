# [L] A Rust lite-07 answer shows a claim only where a route of it can serve

## Goal

A Rust lite-07 publisher shows a peer a claim only where some route of the claim's instance
can serve a path the peer may ask for, as the JS answer already does, and re-judges the claim
the moment what those routes may serve changes. So a peer is never shown a claim no route can
serve, nor left without one some route can.

Today `Answer::shows` (`rs/moq-net/src/lite/publisher/routed.rs`) judges the handle's scope and
the permit (`Consumer::reaches_any`, `Permit::beneath`) but never the route's reach, since the
node tells a claim without it. A route announced through a producer scoped to `a/*/x` at the
prefix `a` is shown at `a/q/y` (Above 2) to a peer that asks for `a/q/y`, and at `a` to a peer
confined to `a/*`, though the route serves neither: a request there fails.

## Plan

Decided 2026-11-20 (maintainer, on [moq-dev/moq#9229](https://github.com/moq-dev/moq/pull/9229)):
✅ the lite-07 cut waits on this quest, and #9229 lands without it (rejected: fixing it after the
cut; holding #9229 until it is fixed). #9229 first carried an origin lookup of the reach and
then dropped it here (see below), so until this lands Rust shows claims as before.

- Recommended: carry the reach with the claim inside the node (on its `Attr`, never on the
  wire), so the node tells every answer when it changes and `shows` intersects it with the
  permit beneath the path, lock-free.
  - The origin's own claims take it from the feeder cursor (`OwnClaims` in
    `rs/moq-net/src/model/origin.rs`), which then reports the union reach of the winner's
    instance at each prefix (`sync_route`, `TableCursor::update`). The origin's cursors report
    only the winning route, so without this a route of the same instance at the same prefix
    that does not win (appears, leaves, or changes scope) reaches no answer.
  - A neighbour's claims take it from the session that gives them (`lite/subscriber/routed.rs`,
    `admits`: its producer's scope intersected with the claim's prefix). That session files the
    route at the path the claim entered (`serve`, with `Lane::above` from the claim), not at the
    claim's own prefix, and may tell the claim before it serves it into the origin. Carrying the
    reach from the giver covers both.
  - Each route's reach keeps its producer's exclusions (`RouteEntry::excluded`, which
    `RouteEntry::authorizes` applies but `RouteEntry::reach` omits), applied before routes are
    combined, so an exclusion on one route never hides what another serves.
  - Through a mount, the answer names the reach as the handle names it member by member,
    dropping only a member with no path within the wire limits: `Patterns::rooted` fails a
    whole union when one member is too deep.
  - Only the feeder's cursors compute the union, since every cursor touching a prefix visits
    `TableCursor::update`; bench that over routes per prefix and cursors, both swept, and the
    answer's judgment over sessions and claim churn.
- Rejected, tried in #9229: looking the reach up in the origin at each judgment
  (`Consumer::reach` over `routes.at`). It took the origin's whole-table lock and allocated per
  claim per session on the announce fan-out; it looked up the claim's own prefix (its path less
  `above`) while a neighbour's route is filed where the claim entered, so a relayed claim missed
  and the check failed open; it ignored exclusions; and a mount translation failed whole.
- Rejected: a watch per claim the reach hides, which costs origin state per claim.
- Any judgment change between a grant change and its SYNCED goes through `Answer::sync`, and
  #9229 holds every prefix's SYNCED until a grant change's resync, so a reach change that
  arrives through the node needs nothing more for SYNCED.
- The tests: a producer scoped to `a/*/x` announcing at `a`, asked for `a/q/y` with no grant and
  for the root under `a/*` or `a/q/y/**`, shows nothing; asked for `a/q/x`, shows it; a mount
  names the reach beneath its point. A route of the claim's instance serving `c/public/**`
  announced beside a winner serving only `c/private/**` shows the claim to a peer granted
  `c/public/**` at once. A scoped neighbour granted `a/*/x` gives a claim at `a` presented at
  `a/q` (Above 1), and a peer granted `a/q/y/**` is never shown it. A producer scoped to
  `t/x/live` and `t/y/chat` that excludes `t/x` is never shown under `t/*/live`. The property
  test in `lite/publisher/routed/tests.rs` (`synced_follows_what_the_grant_shows`) already
  announces through scoped producers: its oracle then covers reach as well.

Public API: none. Wire: none.
