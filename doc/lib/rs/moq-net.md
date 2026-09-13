---
title: moq-net
description: The pub/sub layer
---

# moq-net

[![crates.io](https://img.shields.io/crates/v/moq-net)](https://crates.io/crates/moq-net)
[![docs.rs](https://docs.rs/moq-net/badge.svg)](https://docs.rs/moq-net)

The networking layer: real-time pub/sub with caching, fan-out, and
prioritization on top of QUIC. It negotiates [moq-lite](/concept/moq-lite) or
IETF moq-transport at setup and presents one API either way. Media is a layer
above ([hang](/lib/rs/hang)); relays and CDNs implement only this.

## What it gives you

- **Origins** scope what a session can see, and merge duplicate subscriptions so a broadcast is pulled upstream once no matter how many local readers.
- **Broadcasts** are created unadvertised, then announced as a route, or claimed as a pattern with `dynamic`. Discovery is still by prefix; the events carry a `Pattern`.
- **Patterns** (`Pattern`, `Patterns`) are re-exported from [`moq-pattern`](https://docs.rs/moq-pattern). Literal `Path` stays a coordinate.
- **Tracks** carry groups with a priority, an ordering preference, a retention window, and a timescale. Subscribers set their own priority and max age and can change them live.
- **Groups** are written frame by frame and delivered on independent streams. Old groups are cached for fetch-by-sequence; stale groups are skipped per the subscriber's budget.
- **Datagrams** send a single small frame unreliably on moq-lite 05+.
- **Routes** record the relay hops and a cost, which is what the relay [cluster](/bin/relay/cluster) routes on.
- **Stats** counters per broadcast and session, drained by [`moq-stats`](https://docs.rs/moq-stats).

It runs over anything implementing `web_transport_trait::Session`: quinn,
quiche, noq, the browser, iroh, or qmux over TCP, Unix sockets, and
WebSockets. [`moq-tokio`](https://docs.rs/moq-tokio) wires those up.

```bash
cargo add moq-net moq-tokio
```

See the [Rust quick start](/lib/rs/#quick-start) and
[docs.rs/moq-net](https://docs.rs/moq-net). The TypeScript twin is
[`@moq/net`](/lib/js/net). The [path pattern](/concept/moq-lite#path-patterns)
grammar lives on the concept page.

## Patterns

`Pattern` describes a set of paths; `Patterns` is a union reduced by
containment. `contains` is the authorization check (every path the other
matches, this one matches too). `overlaps` asks whether they share any path.
`rooted` places a pattern under a literal root; `rebase` is the inverse, and
can return several residuals (`**/a` at `a` is both `""` and `**/a`).

```rust
use moq_net::Pattern;

let scope: Pattern = "room/**".parse()?;
assert!(scope.matches("room/alice"));
assert!(scope.contains(&"room/camera-*".parse()?));
assert!(scope.overlaps(&"*/alice".parse()?));
assert_eq!(
    scope.rebase("room").iter().map(Pattern::as_str).collect::<Vec<_>>(),
    ["**"]
);
assert_eq!(
    "camera-*".parse::<Pattern>()?.rooted("room")?.as_str(),
    "room/camera-*"
);
```

## Advertising

Three operations, on an origin:

- `origin.create_broadcast(path)` returns a producer. The broadcast is
  reachable by exact path immediately and invisible to discovery until
  advertised.
- `broadcast.announce(route)` / `broadcast.unannounce()` own that
  advertisement. Announcing again re-prices the standing route. The route
  retracts on `unannounce()`, `finish()`, or the last producer dropping.
- `origin.dynamic(pattern, route)` claims every matching path. A prefix is
  `foo/**`. Hold the returned `origin::Dynamic` while the claim should stay
  advertised; drop it to retract. A request under a prefix-shaped claim with
  no local broadcast is a `Request` to `accept` or `reject`.

A wildcard is a capability, not an inventory. The advertised pattern must sit
inside one of the producer's `prefix/**` scopes; an over-wide claim is
`Unauthorized`, not clamped. Token grants stay prefixes.

`origin.consume().announced()` yields `announce::Update` values. `pattern` is
the claim relative to the consumer's root, `active` is false on a retraction,
and `route` carries hops and cost. A subtree is `room/**`; `room/*` is one
child segment. Use `pattern.as_prefix()` when you need a prefix-shaped claim;
an arbitrary pattern is not a broadcast name. Resolving a non-prefix pattern
into a subscription is not implemented yet.

## Limiting reads

Use `Subscription::default().with_groups(2..=5)` to request only groups 2
through 5. `2..5` excludes group 5, and `..` leaves both ends unbounded.
The range limits the data eligible under the subscription's max-age budget;
it does not fetch historical data by itself.

A reader's `with_groups(2..=5)` applies a local limit. Use `set_groups(...)`
to update an existing reader. Both preserve read progress: a lower start does
not rewind the reader, and an omitted start keeps its current floor. An
omitted end removes the cap, making unread buffered groups available again.
Local limits do not update the subscription's upstream request.

Inside a group, `with_frames(...)` and `set_frames(...)` apply the same range
syntax to frame indices.
