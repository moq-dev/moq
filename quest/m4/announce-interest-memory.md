# [S] Cap the legacy versions' announce interest streams

## Goal

A peer can open only as many announce-interest streams (lite-05/06
ANNOUNCE_PLEASE or moq-transport SUBSCRIBE_NAMESPACE) as a new
`session::Limits` field allows, so a subscriber opening thousands of
root-prefix streams on a relay holding N routes can no longer make it hold
thousands of times N entries. A stream past the cap is refused, and a test
shows the refusal on each of those versions while a stream under the cap is
served as today.

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

Since [one route stream](https://github.com/moq-dev/moq/pull/8835),
lite-07 holds one interest stream per side, so what remains exposed is
lite-05/06 and moq-transport.

Decided 2026-11-15 (maintainer, /quest-complete): ✅ a `session::Limits` cap
on interest streams for lite-05/06 and moq-transport now, counted like
`announces` with a `Slots` acquire where each stream starts, and deleted with
this concern once lite-07 is the oldest version served (rejected: a shared,
refcounted table snapshot that cursors walk lazily, since redesigning
`model/origin.rs` for versions being retired costs more than it saves; and
deleting this later with no cap meanwhile). `Limits` is `#[non_exhaustive]`,
so the field is additive. Report its peak in the stats sessions rows, as
`announces` does, and mirror the field in `js/net` if it serves these
streams.

Public API: additive, a new `session::Limits` field. Wire: none.

## Related

- [Cluster routing](/quest/m3/cluster-routing/README.md) - reshapes the route table this walks
