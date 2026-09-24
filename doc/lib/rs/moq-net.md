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
- **Broadcasts** are created unannounced and invisible to everyone, then announced as an exact route, or served below a prefix with `dynamic`. A consumer of the same origin sees exactly what a peer sees. Discovery accepts pattern unions; events carry the advertised prefix and captures for a complete match.
- **Patterns** (`Pattern`, `Patterns`) are re-exported from [`moq-pattern`](https://docs.rs/moq-pattern). Literal `Path` stays a coordinate.
- **Tracks** carry groups with a priority, a retention window, and a timescale. Subscribers set their own priority and max age and can change them live.
- **Groups** are written frame by frame and delivered on independent streams. Old groups are cached for fetch-by-sequence; stale groups are skipped per the subscriber's budget.
- **Datagrams** send a single small frame unreliably on moq-lite 05+.
- **Routes** record the relay hops and a cost, which is what the relay [cluster](/bin/relay/cluster) routes on. A hop of 0 marks the chain anonymous: `Route::is_anonymous()` is true, and that route ranks below every fully identified one. `Route::source()` says where a delivered route entered: `Source::Local`, or `Source::Peer(hop)` when a handle marked `origin::Producer::peer()` announced it. `origin::Consumer::local()` sees only the local ones.
- **Stats** counters per broadcast and session, drained by [`moq-stats`](https://docs.rs/moq-stats).

It runs over anything implementing `web_transport_trait::poll::Session`: noq, the
browser, iroh, or qmux over TCP, Unix sockets, and
WebSockets. [`moq-tokio`](https://docs.rs/moq-tokio) wires those up.

```bash
cargo add moq-net moq-tokio
```

See the [Rust quick start](/lib/rs/#quick-start) and
[docs.rs/moq-net](https://docs.rs/moq-net). The TypeScript twin is
[`@moq/net`](/lib/js/net). The [path pattern](/concept/moq-lite#path-patterns)
grammar lives on the concept page.

## Driving sessions

`Client::connect(now, transport)`, `Server::accept(now, transport)`, and
`server::Handshake::ok()` return `(Session, Driver)`. `moq-net` never spawns
tasks or reads the clock: the caller polls the driver and supplies the time.
`moq_net::time::run` does that on tokio or in the browser.

```rust
let now = tokio::time::Instant::now().into_std();
let (session, driver) = client.connect(now, transport).await?;
tokio::spawn(moq_net::time::run(driver));
```

A custom event loop calls `driver.poll(now, waiter)` with a nondecreasing
`moq_net::time::Instant`. `Ok(Some(at))` asks to be polled again by `at` or
on external activity, `Ok(None)` only on external activity, and `Err` is the
terminal error (`Error::Closed` for a clean finish): stop polling. Tests drive
the same interface with explicitly advanced instants.

Dropping the last session handle requests closure on the next poll. Dropping
the driver cancels the session. `moq-tokio` and `moq-wasm` drive sessions for
their callers.

`origin::Producer::new` returns a driver with the same `time::Driver`
interface. It calls `cache::Pool::gc(now)` after each poll and folds the next
cleanup time into its returned deadline. A standalone pool needs `gc(now)`
called by its owner, at least by the returned deadline; `None` means expiry is
disabled.

Cache activity is dated lazily: reads and writes mark a group active without
reading a clock, and the next `gc` pass stamps it with the supplied instant.
Expiry is therefore approximate; a late `gc` extends retention.

## Patterns

`Pattern` describes a set of paths; `Patterns` is a union reduced by
containment. `contains` is the authorization check (every path the other
matches, this one matches too). `overlaps` asks whether they share any path.
`rooted` places a pattern under a literal root; `rebase` is the inverse, and
can return several residuals (`**/a` at `a` is both `""` and `**/a`).
`intersect` returns the exact overlap as a union, and `captures` reports what
one pattern's wildcards stand for in a contained pattern.

```rust
use moq_net::Pattern;

let scope: Pattern = "room/**".parse()?;
assert!(scope.matches("room/alice"));
assert!(scope.contains(&"room/camera-*".parse()?));
assert!(scope.overlaps(&"*/alice".parse()?));
assert_eq!(
    scope.intersect(&"*/alice".parse()?)?
        .iter().map(Pattern::as_str).collect::<Vec<_>>(),
    ["room/alice"]
);
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

- `origin.publish(path, route)` creates and advertises a broadcast in one call.
- `origin.create_broadcast(path)` returns a producer. The broadcast is
  invisible and unroutable, for local consumers and peers alike, until
  `broadcast.announce(route)`.
- `broadcast.announce(route)` / `broadcast.unannounce()` own that
  advertisement. Announcing again re-prices the standing route, which competes
  on cost with remote routes at the same path (a tie goes to the local
  broadcast). The route retracts on `unannounce()`, `finish()`, or the last
  producer dropping; tracks already in flight carry on to their own end.
- `origin.dynamic(prefix, route)` claims `prefix` and every path beneath it
  (`""` claims everything). Hold the returned `origin::Dynamic` while the
  claim should stay advertised; drop it to retract. A request beneath it with
  no winning announced local broadcast is a `Request` to `accept` or `reject`; reject what you
  will not serve rather than narrowing the claim, since a route is always a
  prefix on every wire.

A route is a capability, not an inventory. Producer and consumer handles are
scoped by any `Patterns` union. A prefix route is allowed when its subtree
overlaps that scope; exact creates and requests must match it, so a broad
route can advertise the wire-compatible prefix while excluded requests are
refused locally. A disjoint route is `Unauthorized`.

`origin.consume().announced()` yields `announce::Update` values: `prefix` is the
covered prefix relative to the consumer's root, `kind` is `Announced`,
`Updated` (a reprice in place), or `Retracted`, `captures` reports what the
most specific matching scope member's wildcards stood for when the prefix
pins them, and `route` carries hops and cost (on a retraction, its last
values). The consumer is also a `futures::Stream`. A prefix is not a
broadcast name; sessions request each scope member's literal head and filter
locally.

## Limiting reads

Use `Subscription::default().with_groups(2..=5)` to request only groups 2
through 5. `2..5` excludes group 5, and `..` leaves both ends unbounded.
The range limits the data eligible under the subscription's max-age budget;
it does not fetch historical data by itself.

A reader's `set_groups(2..=5)` applies a local limit. It preserves read
progress: a lower start does not rewind the reader, and an omitted start
keeps its current floor. An omitted end removes the cap, making unread
buffered groups available again. Local limits do not update the
subscription's upstream request.

Inside a group, `set_frames(...)` applies the same range syntax to frame
indices.
