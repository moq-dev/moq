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
- **Epochs** identify publisher instances with a canonical UUIDv7 carried on each route (`Route::epoch`): the newest wins a path, and only routes with the same epoch resume a subscription; see [publisher epochs](/concept/moq-lite#publisher-epochs).
- **Patterns** (`Pattern`, `Patterns`) are re-exported from [`moq-pattern`](https://docs.rs/moq-pattern). Literal `Path` stays a coordinate.
- **Tracks** carry groups with a priority, an optional publisher retention window (`Info::max_age`), and a timescale. Subscribers set their own priority and max delay and can change them live.
- **Groups** are written frame by frame and delivered on independent streams. Old groups are cached for fetch-by-sequence; stale groups are skipped per the subscriber's budget.
- **Track ends**: `finish()` ends a track at its live edge, while `finish_at(n)` declares the exclusive end ahead of it and still accepts the groups below. A subscriber awaits it with `finished()`. A remote track ends only once every group below its end has arrived or was dropped; one reset before its header arrived is skipped after the subscription's max delay on moq-lite (one second without one), or after one second on IETF.
- **Datagrams** send a single small frame unreliably on moq-lite 05+ and moq-transport.
- **Routes** record the relay hops and a cost, which is what the relay [cluster](/bin/relay/cluster) routes on. A hop of 0 marks the chain anonymous: `Route::is_anonymous()` is true, and that route ranks below every fully identified one. `Route::source()` says where a delivered route entered: `Source::Local`, or `Source::Peer(hop)` when a handle marked `origin::Producer::peer()` announced it. `origin::Consumer::local()` sees only the local ones. A handle marked `origin::Producer::upstream()` announces as a peer, and a consumer taken from it never sees a route another upstream handle announced, so a relay never transits between two upstream links.
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
`server::Handshake::ok()` return `(Session, Driver)`. Drivers never spawn
tasks or read the clock: the caller polls the driver and supplies the time.
`moq_net::time::run` does that on tokio or in the browser.

```rust
let now = tokio::time::Instant::now().into_std();
let (session, driver) = client.connect(now, transport).await?;
tokio::spawn(moq_net::time::run(driver));
```

On moq-lite-05+ and moq-transport draft 17+, `Client::connect` returns before
the server's SETUP arrives, so a request made at once may still be
refused along with it. `session.setup().await` waits for the server's SETUP
and returns the close reason if the session is already closed, even after SETUP. A moq-rs
server sends its SETUP only once it admits the client; neither protocol
requires that, so another server may still refuse afterward. Older versions
read that SETUP during the handshake and resolve at once, except moq-lite-03
and -04, which carry none and return `Error::Unsupported`.

A custom event loop calls `driver.poll(now, waiter)` with a nondecreasing
`moq_net::time::Instant`. `Ok(Some(at))` asks to be polled again by `at` or
on external activity, `Ok(None)` only on external activity, and `Err` is the
terminal error (`Error::Closed` for a clean finish): stop polling. Tests drive
the same interface with explicitly advanced instants.

Dropping the last session handle requests closure on the next poll, and
`session.abort(err)` closes with `err`'s code. Either discards stream data the
peer has not acknowledged yet. `session.close().await` first waits, up to one
second, for finished tracks to deliver their last groups and FIN, returning
`Error::Timeout` if it gave up. Finish or abort live tracks before calling it.
Both protocols withdraw the session's announcements and wait for delivery
under that same deadline. IETF drafts 14 through 16 send withdrawals without
waiting. Dropping the driver cancels the session. `moq-tokio` and `moq-wasm`
drive sessions for their callers. A deliberate local session close ends
received tracks cleanly after their delivered groups. A peer close ends
received tracks and open group readers with the session error, preserving its
close code.

`origin::Producer::new` returns a driver with the same `time::Driver`
interface. It calls `cache::Pool::gc(now)` after each poll and folds the next
cleanup time into its returned deadline. A standalone pool needs `gc(now)`
called by its owner, at least by the returned deadline; `None` means expiry is
disabled.

The origin driver finishes once every owner is gone: each `origin::Producer`
clone, published broadcast, and `origin::Dynamic`. Producer-side children pin
their parent where no cycle exists, so a handler can drop its producer and keep
serving. Read handles (`origin::Consumer`, announce cursors) never pin it: they
see `Closed` once the owners are gone.

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
  advertisement. Announcing again replaces the standing route as given, epoch
  included, so re-price from the current one
  (`broadcast.announce(broadcast.route().unwrap_or_default().with_cost(c))`);
  another epoch, or none, announces a new broadcast; `origin::Dynamic` has the
  same `route()` and `update(route)`. The route competes with
  remote routes at the same path: the newest epoch wins, then the cheapest (a
  tie goes to the local broadcast). The route retracts on `unannounce()`, `close()`, or the last
  producer dropping; tracks already in flight carry on to their own end.
- `broadcast.close()` ends the broadcast for good: it retracts, leaves local
  discovery, and answers every later track lookup with `Unroutable`. Tracks
  already subscribed carry on to their own end. It can never be announced
  again. Dropping the last producer does the same.
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

`origin.consume().announced()` yields `announce::Event`s: `Start`,
`Update` (a reprice in place), or `End`, each holding an
`announce::Announce` with `prefix`, the covered prefix relative to the
consumer's root; `captures`, what the most specific matching scope member's
wildcards stood for when the prefix pins them; and `route`, its hops and cost
(on a retraction, its last values). The consumer is also a
`futures::Stream`. A prefix is not a broadcast name;
sessions request each scope member's literal head and filter locally. Routes
with a `.`-prefixed segment below that head are [hidden](/concept/moq-lite#hidden-broadcasts)
unless `with_hidden(true)` opts the consumer in. Sessions always ask the peer
for hidden routes, so each local consumer decides.

## Limiting reads

Use `Subscription::default().with_groups(2..=5)` to request only groups 2
through 5. `2..5` excludes group 5, and `..` leaves both ends unbounded.
The range limits the data eligible under the subscription's max delay budget;
it does not fetch historical data by itself.

A reader's `set_groups(2..=5)` applies a local limit. It preserves read
progress: a lower start does not rewind the reader, and an omitted start
keeps its current floor. An omitted end removes the cap, making unread
buffered groups available again. Local limits do not update the
subscription's upstream request.

Inside a group, `set_frames(...)` applies the same range syntax to frame
indices.

## Subscriber demand

Track producers, pending track requests, and dynamic track handlers return a
`track::Demand` from `demand()`. Group producers and pending group requests
return `group::Demand`; broadcast producers return `broadcast::Demand`.
Watch `used()` / `unused()` (or `poll_used` / `poll_unused`) on these handles,
or take a snapshot with `is_used()`. Demand handles cannot write or abort and
do not keep a producer or request alive. A pending group request counts every
caller sharing its fetch, so one caller leaving does not cancel the others.
The last caller to leave withdraws the fetch, so a later fetch of that group
starts a fresh request; a handler that sees the request unused just drops it.
Use the producer's `abort_unused` to atomically end an idle track; observing
unused demand alone does not protect against a new subscriber arriving.
