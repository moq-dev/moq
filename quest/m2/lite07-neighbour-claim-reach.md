# [S] A lite-07 answer judges a neighbour's claim by its reach

## Goal

A Rust lite-07 answer shows a claim a scoped neighbour gave only where that claim's route
can serve some path the peer may ask for, as it already does for the origin's own claims.
Today the answer looks a claim's reach up in the origin when the node tells it
(`Answer::announce` in `rs/moq-net/src/lite/publisher/routed.rs`), and a neighbour's claim
can be told before the session that gave it serves it into the origin, so the answer finds
no route and shows it as before: under a grant it shares no path with, where a request fails.

## Plan

Found by [moq-dev/moq#9229](https://github.com/moq-dev/moq/pull/9229), which carried the
reach to `Answer::shows` for claims the origin holds when told.

- Recommended: the giving session already knows the reach when it files the claim
  (`lite/subscriber/routed.rs`, `admits`: its producer's scope intersected with the claim's
  prefix). Carry it with the claim inside the node, never on the wire, so every answer the
  node tells judges it at once, and drop the origin lookup.
- Rejected: looking the reach up again later, since nothing tells the answer when the
  serving session catches up.
- The test: a scoped neighbour whose grant is `a/*/x` gives a claim at `a`; another peer
  under the grant `a/q/y/**` is not shown it, even before the giving session serves it.

Public API: none. Wire: none.
