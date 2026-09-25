# [S] Skip unchanged announce updates

## Goal

A publisher sends an announce update only when what the peer would decode
differs from what it last sent for that announcement. A local change the wire
cannot express (the route's source session, `served`, captures) sends nothing.
This applies on every version, lite and IETF, in Rust and JS, and it is wire
compatible.

## Plan

Each announce cursor dedupes its best route on `(hops, cost, source)` plus
`served` and captures (`rs/moq-net/src/model/origin.rs`, the `Updated` kind),
but the lite publisher only remembers each suffix's Announce ID
(`rs/moq-net/src/lite/publisher.rs`, the `self.live` branch) and re-sends
`Restart` for every `Updated` it sees. Only the hops and cost reach the wire,
so a flip of any other field sends an identical update, to every peer, for
every covered broadcast. This comes from reading the code, not from a test.

- Reproduce first: a test that flips a route's source session with the same
  hops and cost, and asserts the peer receives no update.
- Keep the last-sent `(hops, cost)` beside the Announce ID and skip equal
  updates. Check the IETF publisher's re-pricing path and the JS publisher for
  the same pattern.
- Fix the stale comments on the way: `lite/announce.rs` says restarts are only
  ever received, and the `restart_announce` doc in `lite/subscriber.rs` says it
  compares the first hop.

## Related

- [Announce counters](/quest/m0/announce-counters.md) - shows the saving on a live fleet
- [Babel routing](/quest/m0/babel/README.md) - removes the other big source of updates, reroutes that change only the hop chain
