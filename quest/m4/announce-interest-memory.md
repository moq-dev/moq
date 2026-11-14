# [M] Announce interest streams share one route table

## Goal

An announce-interest stream (lite ANNOUNCE_PLEASE or moq-transport
SUBSCRIBE_NAMESPACE) costs memory and CPU proportional to what it has in
flight, not to every route under its prefix. A subscriber opening thousands
of root-prefix streams on a relay holding N routes no longer makes it hold
thousands of times N entries, and a benchmark swept over streams and routes
shows per-stream cost flat in N.

## Plan

From the 2026-10-24 pre-cut audit at 151a529670. Source-inferred, not
measured; inherited (release's `lite/publisher.rs:659` has the same
per-stream `origin.announced()`).

Each stream scopes the origin and opens its own `origin.routes()` cursor
(`rs/moq-net/src/lite/publisher.rs`, the announce stream's `start`, about
line 1058, with no `Slots` acquire), materializing its own `TableCursor`
with `OriginConsumerState` pending/due/delivered entries, and the
`AnnounceRun` live map (`model/origin.rs` `register_cursor` about line 6481,
`Consumer::routes` about 7698). On lite-05/06, `init()` also encodes the
whole initial set before the first flush. Only QUIC stream credit bounds the
count (`moq_tokio::quic::DEFAULT_MAX_STREAMS = 10_000`), so the worst case
is about 10k times N times a few hundred bytes. Incoming announces are
bounded by `session::Limits`; outgoing interest streams are not.

Once [one route stream](/quest/m2/cluster-routes/route-stream.md)
lands, lite-07 holds one interest stream per side, so what remains exposed is
lite-05/06 and moq-transport.

Bound it by design, not with a cap: cursors walk a shared, refcounted table
snapshot lazily, materializing only what is in flight and gated on stream
write credit; lite-05/06 encodes its initial set incrementally under
`poll_flush`, as lite-07 does. The IETF SUBSCRIBE_NAMESPACE handler follows
the same pattern and gets the same fix.

Public API: none. Wire: none.

## Waiting

- Maintainer: how to bound the legacy versions' interest streams: the shared
  snapshot redesign above, a `session::Limits` cap on outgoing interest
  streams (which the Plan rules out today), or deleting this once lite-07 is
  the oldest version served. Recommended: the cap for lite-05/06 and
  moq-transport now, then delete, since redesigning `model/origin.rs` for
  versions being retired costs more than it saves.

## Related

- [Cluster routing](/quest/m3/cluster-routing/README.md) - reshapes the route table this walks
