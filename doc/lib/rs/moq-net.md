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
- **Broadcasts** are created unadvertised, then announced as an exact route, or served below a prefix with `dynamic`. Discovery accepts pattern unions; events carry the advertised prefix and captures for a complete match.
- **Patterns** (`Pattern`, `Patterns`) are re-exported from [`moq-pattern`](https://docs.rs/moq-pattern). Literal `Path` stays a coordinate.
- **Tracks** carry groups with a priority, a retention window, and a timescale. Subscribers set their own priority and max age and can change them live.
- **Groups** are written frame by frame and delivered on independent streams. Old groups are cached for fetch-by-sequence; stale groups are skipped per the subscriber's budget.
- **Datagrams** send a single small frame unreliably on moq-lite 05+.
- **Routes** record the relay hops and a cost, which is what the relay [cluster](/bin/relay/cluster) routes on. A hop of 0 marks the chain anonymous: `Route::is_anonymous()` is true, and that route ranks below every fully identified one.
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
`server::Handshake::ok()` return `(Session, Driver)`. The lite-only entry points
return the same pair. `moq-net` never spawns tasks or schedules runtime timers.

```rust
let now = tokio::time::Instant::now().into_std();
let (session, driver) = client.connect(now, transport).await?;
tokio::spawn(moq_tokio::runtime::run(driver));
```

A custom event loop calls `driver.poll(now, waiter)` with a nondecreasing
`moq_net::time::Instant`. After `Pending`, wait for external activity or the
instant returned by `driver.timeout()`, whichever comes first. `None` means
there is no timer to schedule. Poll again with fresh time after waking.
Tests use the same interface with explicitly advanced instants.

Dropping the last session handle requests closure when the driver next runs.
Dropping the driver cancels the session. Keep both alive while using the
connection. `moq-tokio` and `moq-wasm` drive sessions for their callers; the
lite-only path also supports native `!Send` transports on their owning thread.

Origin drivers implement the same `time::Driver` interface.
`origin::Producer::new` returns its lifecycle driver, which also calls
`pool.gc(now)` after polling and includes its next cleanup time in the
origin's deadline. For a standalone pool, call `pool.gc(now)` yourself after
polling and at the returned deadline, even when there is no traffic.
`None` means both cache policies are disabled; call again after enabling a
capacity with `pool.resize`.

Cache reads and writes clear their expiration timestamp without reading the
system clock. A due cleanup pass scans cached groups, dates undated activity,
and expires idle groups except each track's latest. Calls before the cleanup
deadline only advance the sampled clock, so calling after every poll is cheap.
Expiration is approximate: delayed cleanup extends retention. Model read/write
APIs take no wall-clock time. Datagram writes keep their existing signatures:
`append_datagram(timestamp, payload)` and
`insert_datagram(sequence, timestamp, payload)`. Datagram send buffers retain
the newest 64 entries, dropping the oldest at capacity without reading a clock.
This changes Rust APIs, with no wire-format or TypeScript API changes.

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
  reachable by exact path immediately and invisible to discovery until
  advertised.
- `broadcast.announce(route)` / `broadcast.unannounce()` own that
  advertisement. Announcing again re-prices the standing route. The route
  retracts on `unannounce()`, `finish()`, or the last producer dropping.
- `origin.dynamic(prefix, route)` claims `prefix` and every path beneath it
  (`""` claims everything). Hold the returned `origin::Dynamic` while the
  claim should stay advertised; drop it to retract. A request beneath it with
  no local broadcast is a `Request` to `accept` or `reject`; reject what you
  will not serve rather than narrowing the claim, since a route is always a
  prefix on every wire.

A route is a capability, not an inventory. Producer and consumer handles are
scoped by any `Patterns` union. A prefix route is allowed when its subtree
overlaps that scope; exact creates and requests must match it, so a broad
route can advertise the wire-compatible prefix while excluded requests are
refused locally. A disjoint route is `Unauthorized`.

`origin.consume().announced()` yields `announce::Update` values: `path` is the
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
