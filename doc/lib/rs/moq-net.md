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
- **Rank several CDNs.** Dial each with its own subscribe handle, `origin.clone().with_preference(origin::Preference::new(rank))` (higher wins; 0 is neutral and a negative rank demotes), all on one origin. Requests use the preferred link that has a route and fail over to the next; see [multiple CDNs](/concept/moq-lite#multiple-cdns).
- **Follow restarts.** `consumer.announced_broadcast(path, None)` hands out each publisher instance serving a path from `available()`: the first once announced, then each one that takes over or returns after an end, like JS `request(path, { announced: true })`. A broadcast already resolved stays on the old instance until dropped or its route goes. `consumer.follow(path)` yields the underlying start, update, restart, and end of the route a request resolves.
- **Discovery follows what you watch.** On moq-lite, a client session asks the relay for announcements only under the prefixes its subscribe origin's readers watch, with `announced()`, `routed_broadcast`, or `announced_broadcast`, stops asking about a prefix 2s after nothing at or beneath it is watched, and asks again on reconnect. A broadcast resolved through a discovered route keeps its path watched until dropped. `request_broadcast` watches nothing, so a path nobody watches is unroutable. A server session, or a client fed into a `peer()` or `upstream()` origin handle, asks for everything it may, so a relay discovers everything. An HTTP gateway (the moq-hls `Server`, the moq-rtc WHEP router) watches its whole scope while it lives, so the paths its callers name never open a request each. moq-transport sessions, and moq-lite-03/04 sessions (whose announcements mark no initial set), still ask for everything.
- **One track per name.** Concurrent subscriptions share one request and producer. The application owns sequence numbers: a producer that replaces an ended one gets `Error::Duplicate` from an append that would reuse a sequence the name already wrote, so it starts with `create_group` at an explicit sequence. Only a new broadcast starts over. Readers of an aborted track resume onto the replacement only when the broadcast announces an epoch; otherwise their subscription ends with the error.
- **End a broadcast with `finish()`.** When the input ends, finish the tracks, then call `broadcast.finish()`. The returned handle keeps the broadcast announced and serving its finished tracks for `linger()`, their longest max-age, so a subscriber still catching up can drain them. Await `wait()` on a runtime, or drop the handle once your own clock passes `linger()`. Dropping it early is the interrupt, and `close()` on another handle ends it at once.
- **Publish only while someone is watching.** `demand()` on a track, group, or broadcast says whether a subscriber is attached, which is how capture and transcode skip work nobody asked for. Holding a broadcast or track consumer is not demand: a track consumer requests nothing until it queries, subscribes, or fetches, and those count until dropped. A shared fetch stays up until its last reader leaves.
- **Use the model without a runtime.** Disable default features on `moq-net` to use its `poll_*` APIs and origin driver with caller-supplied time. The default `async` feature adds async helpers, `time::run`, and both wire sessions.
- **You drive the session, or `moq-tokio` does.** `connect` and `accept` return a session plus a driver that never reads the clock itself. `moq_net::time::run` polls it on tokio or in the browser. `moq-tokio` and `moq-wasm` hide that. A custom transport implements `moq_net::transport::poll`, and `moq_net::transport::memory::pair` links two sessions in one process with no sockets, for tests or in-process links.
- **A session caps what its peer can hold.** `session::Limits` (via `Client::with_limits` and `Server::with_limits`) bounds announces, subscriptions, and FETCH requests independently per session; the defaults suit a relay mesh, so lower them for untrusted peers. Past a cap, the session closes with `TOO_MANY_REQUESTS`. On moq-transport drafts 14 to 16 they also size the request-ID window, which every request counts against, so very low limits can starve it. Peer-declared lengths are capped before they are buffered.
- **Authorization is in band.** On moq-lite 07 (`moq-lite-07-wip`, opt-in) and on moq-transport draft-17+ with the [MoQ Auth extension](/draft/moq-auth), `session.auth()` presents tokens without reconnecting and watches the grant the peer sent back. A server verifies tokens itself by taking `handshake.auth().requests()` before `ok()`. `authorize(&grant)` narrows or widens a live session on any version: what falls outside resets with `Unauthorized` and the session stays up. A client that publishes outside its grant closes the session, naming the path.
- **A graceful close waits.** `session.close().await` withdraws announcements and drains for up to one second. Application delivery requires explicit `moq-lite-07-wip`; older Lite versions (including default lite-06) and IETF wait only for transport ACKs. `abort` ends immediately. IETF drafts 14 through 16 send withdrawals without waiting.

```bash
cargo add moq-net moq-tokio
```

See the [Rust quick start](/lib/rs/#quick-start) and
[docs.rs/moq-net](https://docs.rs/moq-net). The TypeScript twin is
[`@moq/net`](/lib/js/net). Path patterns are on the
[concept page](/concept/moq-lite#path-patterns).
