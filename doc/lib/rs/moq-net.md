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
- **Broadcasts** are announced by path and discovered by prefix, or served on demand from a dynamic handler when a path nobody announced is requested.
- **Tracks** carry groups with a priority, an ordering preference, a retention window, and a timescale. Subscribers set their own priority and latency budget and can change them live.
- **Groups** are written frame by frame and delivered on independent streams. Old groups are cached for fetch-by-sequence; stale groups are skipped per the subscriber's budget.
- **Datagrams** send a single small frame unreliably on moq-lite 05+.
- **Routes** record the relay hops and a cost, which is what the relay [cluster](/bin/relay/cluster) routes on.
- **Stats** counters per broadcast and session, drained by [`moq-stats`](https://docs.rs/moq-stats).

It runs over anything implementing `web_transport_trait::Session`: quinn,
quiche, noq, the browser, iroh, or qmux over TCP, Unix sockets, and
WebSockets. [`moq-native`](https://docs.rs/moq-native) wires those up.

```bash
cargo add moq-net moq-native
```

See the [Rust quick start](/lib/rs/#quick-start) and
[docs.rs/moq-net](https://docs.rs/moq-net). The TypeScript twin is
[`@moq/net`](/lib/js/net).
