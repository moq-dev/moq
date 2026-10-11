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
- **Follow restarts.** `consumer.follow(path)` reduces the routes covering a path to the one a request resolves (the most specific) and yields its start, update, restart, and end. A restart says another publisher instance now serves the path: request it again. A broadcast already resolved stays on the old instance until dropped or its route goes.
- **One track per name.** Concurrent subscriptions share one request and producer. A producer that replaces an ended one continues the name's group and datagram sequences; only a new broadcast restarts them. Readers of an aborted track resume onto the replacement only when the broadcast announces an epoch; otherwise their subscription ends with the error.
- **Publish only while someone is watching.** `demand()` on a track, group, or broadcast says whether a subscriber is attached, which is how capture and transcode skip work nobody asked for. Holding a broadcast consumer is not demand. Holding a track consumer is, so drop one you are not reading. A shared fetch stays up until its last reader leaves.
- **You drive the session, or `moq-tokio` does.** `connect` and `accept` return a session plus a driver that never reads the clock itself. `moq_net::time::run` polls it on tokio or in the browser. `moq-tokio` and `moq-wasm` hide that. A custom transport implements `moq_net::transport::poll`.
- **A session caps what its peer can hold.** `session::Limits` (via `Client::with_limits` and `Server::with_limits`) bounds announces and subscriptions per session; the defaults suit a relay mesh, so lower them for untrusted peers. Past either, the session closes with `TOO_MANY_REQUESTS`. On moq-transport drafts 14 to 16 they also size the request-ID window, which every request counts against, so very low limits can starve it. Peer-declared lengths are capped before they are buffered.
- **Authorization is in band.** On moq-lite 07 (`moq-lite-07-wip`, opt-in) and on moq-transport draft-17+ with the [MoQ Auth extension](/draft/moq-auth), `session.auth()` presents tokens without reconnecting and watches the grant the peer sent back. A server verifies tokens itself by taking `handshake.auth().requests()` before `ok()`. `authorize(&grant)` narrows or widens a live session on any version: what falls outside resets with `Unauthorized` and the session stays up. A client that publishes outside its grant closes the session, naming the path.
- **A graceful close waits.** `session.close().await` withdraws announcements and gives finished tracks up to one second to deliver. `abort` ends immediately. IETF drafts 14 through 16 send withdrawals without waiting.

```bash
cargo add moq-net moq-tokio
```

See the [Rust quick start](/lib/rs/#quick-start) and
[docs.rs/moq-net](https://docs.rs/moq-net). The TypeScript twin is
[`@moq/net`](/lib/js/net). Path patterns are on the
[concept page](/concept/moq-lite#path-patterns).
