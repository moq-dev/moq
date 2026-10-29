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

- **An origin outlives the session.** Creating a broadcast returns an origin publication that controls its announcement and exposes the broadcast to fill. Standalone and Prepared fill producers cannot announce; only publishing the Prepared answer admits its publication. A reconnect announces existing publications again, and closing the session does not delete them.
- **Requests can pin an epoch.** `consumer.request(path, Some(epoch))` resolves only through a route announcing that [publisher epoch](/concept/moq-lite#publisher-epochs); `None` takes whichever route wins. The resolved consumer names its epoch in `info().epoch`, pinned or not.
- **Rank several CDNs.** Dial each with its own subscribe handle, `origin.clone().with_preference(origin::Preference::new(rank))` (higher wins; 0 is neutral and a negative rank demotes), all on one origin. Requests use the preferred link that has a route and fail over to the next; see [multiple CDNs](/concept/moq-lite#multiple-cdns).
- **Request one instance, or follow them all.** `consumer.request(path, None)` resolves the instance serving a path now, and fails `Unroutable` only once discovery has synced with no route for it. `consumer.follow(path, None)` hands out each publisher instance serving a path from `available()`: the first once announced, then each one that takes over or returns after an end, like JS `follow(path)`. A broadcast already resolved stays on the old instance until dropped or its route goes.
- **Discovery follows what you watch.** A client session asks the relay for announcements only under the prefixes its subscribe origin's readers watch, with `announced()`, `request`, or `follow`, stops asking about a prefix 2s after nothing at or beneath it is watched, and asks again on reconnect. A broadcast resolved through a discovered route keeps its path watched until dropped. `request` owns discovery of its path, so no separate watch is needed. A server session, or a client fed into an origin handle marked `with_link(origin::Link::Peer)` or `Link::Upstream`, asks for everything it may, so a relay discovers everything. An HTTP gateway (the moq-hls `Server`, the moq-rtc WHEP router) watches its whole scope while it lives, so the paths its callers name never open a request each. moq-lite-03/04 sessions, whose announcements mark no initial set, still ask for everything.
- **One track per name.** Concurrent subscriptions share one request and producer. The application owns sequence numbers: the broadcast keeps nothing of an ended track, so a producer that replaces it starts at 0. A name always means the same content, so the replacement continues past its predecessor's groups with `create_group` at an explicit sequence. Readers of an aborted track resume onto the replacement only when the broadcast announces an epoch; otherwise their subscription ends with the error.
- **End content by finishing tracks.** A broadcast has no finished state. When the input ends, finish each track; the broadcast stays announced and keeps serving them until you close it or drop its last producer. How long to keep it up for late readers is yours to decide: `moq_mux::Linger` holds a hang broadcast for a fixed window.
- **Publish only while someone is watching.** `demand()` on a track, group, or broadcast says whether a subscriber is attached, which is how capture and transcode skip work nobody asked for. Holding a broadcast or track consumer is not demand: a track consumer requests nothing until it queries, subscribes, or fetches, and those count until dropped. A shared fetch stays up until its last reader leaves. Demand rolls up: a frame being read keeps its group wanted, a group a subscription or fetch handed out keeps its track wanted until read to its end, and any wanted track keeps its broadcast wanted.
- **Use the model without a runtime.** Disable default features on `moq-net` to use its `poll_*` APIs and origin driver with caller-supplied time. The default `async` feature adds async helpers, `time::run`, and both wire sessions.
- **You drive the session, or `moq-tokio` does.** `connect` and `accept` return a session plus a driver that never reads the clock itself. `moq_net::time::run` polls it on tokio or in the browser. `moq-tokio` and `moq-wasm` hide that. A custom transport implements `moq_net::transport::poll`, and `moq_net::transport::memory::pair` links two sessions in one process with no sockets, for tests or in-process links.
- **A session bounds what its peer can hold.** Each request holds a stream, so the transport's stream credit bounds subscriptions and FETCHes. `session::Limits` (via `Client::with_limits` and `Server::with_limits`) bounds the announcements per session. The default suits a relay mesh, so lower it for untrusted peers. Past it, the session closes with `TOO_MANY_REQUESTS`. Peer-declared lengths are capped before they are buffered.
- **Authorization is in band.** On moq-lite 07 (`moq-lite-07-wip`, opt-in) and on moq-transport draft-18+ with the [MoQ Auth extension](/draft/moq-auth), `session.auth()` presents tokens without reconnecting and watches the grant the peer sent back. `add(token)` resolves with the token and how the peer took it: `Answer::Granted` on its AUTH stream, or `Answer::Requests` on moq-transport without AUTH, where only a request that names it carries it. A handle passed to `Client::with_auth` outlives each session, so its tokens, and the routes naming them, carry across your own reconnects. A server verifies tokens itself by taking `handshake.requests()` before `ok()`. `authorize(&grant)` narrows or widens a live session on any version: what falls outside resets with `Unauthorized` and the session stays up. A client that publishes outside its grant closes the session, naming the path.
- **A graceful close waits.** `session.close().await` withdraws announcements and drains for up to one second. Application delivery requires explicit `moq-lite-07-wip`; older Lite versions (including default lite-06) and IETF wait only for transport ACKs. `abort` ends immediately.

```bash
cargo add moq-net moq-tokio
```

See the [Rust quick start](/lib/rs/#quick-start) and
[docs.rs/moq-net](https://docs.rs/moq-net). The TypeScript twin is
[`@moq/net`](/lib/js/net). Path patterns are on the
[concept page](/concept/moq-lite#path-patterns).
