# [S] A Rust lite-07 answer shows a claim only where its route can serve

## Goal

A Rust lite-07 publisher shows a peer a claim only where the claim's own route can
serve some path the peer may ask for, as the JS answer already does. Today a route
announced through a producer scoped to `a/*/x` at the prefix `a` is shown at `a/q/y`
(Above 2) to a peer that asks for `a/q/y`, and at `a` to a peer confined to `a/*`,
though the route serves neither: a request there fails.

## Plan

- `origin::RouteEntry::reach` already holds what a route may serve (its producer's
  scope intersected with its prefix). The answer (`lite/publisher/routed.rs`
  `Answer::shows`) judges the handle's scope and the permit together
  (`Consumer::reaches_any`, `Permit::beneath`) but never the route's reach, since the
  node tells a claim without it. Carry the reach, or a predicate over it, to the
  answer, and intersect it with what `shows` already judges.
- JS selects among candidates with the same predicate (`presented`'s `shows`), so a
  narrower cheap route gives way to one the peer can use; the Rust node presents one
  claim per instance, so check whether it needs the same.
- The test: a producer scoped to `a/*/x` announcing at `a`, asked for `a/q/y` with no
  grant and for the root under `a/*`, shows nothing; asked for `a/q/x`, shows it.
