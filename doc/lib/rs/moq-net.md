---
title: moq-net
description: The pub/sub layer
---

# moq-net

[![crates.io](https://img.shields.io/crates/v/moq-net)](https://crates.io/crates/moq-net)
[![docs.rs](https://docs.rs/moq-net/badge.svg)](https://docs.rs/moq-net)

The networking layer: real-time pub/sub with caching, fan-out, and
prioritization. It negotiates [moq-lite](/concept/moq-lite) or IETF
moq-transport at setup and presents one API either way. Media is a layer above
([hang](/lib/rs/hang)); relays and CDNs implement only this.

What you use it for, beyond what the concept page already describes:

- **An origin outlives the session.** Broadcasts are created on the origin, and a reconnect announces them again. Closing the session does not delete them.
- **Requests can pin an epoch.** `consumer.request_broadcast(path, Some(epoch))` resolves only through a route announcing that [publisher epoch](/concept/moq-lite#publisher-epochs); `None` takes whichever route wins. The resolved consumer names its epoch in `info().epoch`, pinned or not.
- **One track per name.** Concurrent subscriptions share one request and producer. A producer that replaces an ended one continues the name's group and datagram sequences; only a new broadcast restarts them.
- **Publish only while someone is watching.** `demand()` on a track, group, or broadcast says whether a subscriber is attached, which is how capture and transcode skip work nobody asked for. Holding a broadcast consumer is not demand. A shared fetch stays up until its last reader leaves.
- **You drive the session, or `moq-tokio` does.** `connect` and `accept` return a session plus a driver that never reads the clock itself. `moq_net::time::run` polls it on tokio or in the browser. `moq-tokio` and `moq-wasm` hide that. A custom transport implements `moq_net::transport::poll`.
- **A session caps what its peer can hold.** `session::Limits` (via `Client::with_limits` and `Server::with_limits`) bounds announces and subscriptions per session; the defaults suit a relay mesh, so lower them for untrusted peers. Past either, the session closes with `TOO_MANY_REQUESTS`. On moq-transport drafts 14 to 16 they also size the request-ID window, which every request counts against, so very low limits can starve it. Peer-declared lengths are capped before they are buffered.
- **A graceful close waits.** `session.close().await` withdraws announcements and gives finished tracks up to one second to deliver. `abort` ends immediately. IETF drafts 14 through 16 send withdrawals without waiting.

```bash
cargo add moq-net moq-tokio
```

See the [Rust quick start](/lib/rs/#quick-start) and
[docs.rs/moq-net](https://docs.rs/moq-net). The TypeScript twin is
[`@moq/net`](/lib/js/net). Path patterns are on the
[concept page](/concept/moq-lite#path-patterns).
