---
title: moq-auth
description: The authorization contract, the lease a session holds, and the JWT that rides it
---

# moq-auth

[![crates.io](https://img.shields.io/crates/v/moq-auth)](https://crates.io/crates/moq-auth)
[![docs.rs](https://docs.rs/moq-auth/badge.svg)](https://docs.rs/moq-auth)

Everything a party needs to ask for or answer an authorization on
[moq-relay](/bin/relay/auth), as a library. Use it in an auth server that
answers the relay, in a service that mints tokens for clients, or in your own
accept loop that decides in process.

- **Request and grant.** `Request` is the JSON a relay POSTs on each session event (`connect`, `revalidate`, `end`), carrying everything it knows about the session. `Grant` is the answer: publish and subscribe [`Pattern`](https://docs.rs/moq-pattern) unions, an expiry, a revalidation cadence, and whether the session is a cluster peer or an [upstream link](/bin/relay/cluster#upstream-links). `Grant::validate` refuses a grant that cannot be enforced. Expiry is exact, with no allowance for clock skew.
- **Lease.** `lease::Producer` and `lease::Consumer` are the handle a session holds for its grant: read it, wait for a change, revoke it, and learn why it ended. Whoever runs the accept loop builds the producer, so an embedder can decide in process with no HTTP.
- **Client.** `Client` drives a lease against an auth server over `https://`, `unix://`, or loopback `http://`. It revalidates on cadence and retries through an outage until the grant expires, revokes on a 401, 403, or invalid grant, and reports `end` with the session's byte totals.
- **Server** (feature `serve`). `serve::Server` is the reference auth server behind `moq auth serve`: JWTs from the query or the moq-transport SETUP token, certificate grants, anonymous rules, tiers, and session caps. `Server::router` mounts it in your own axum service.
- **Keys and claims.** Generate HMAC, RSA, ECDSA, or EdDSA keys as JWKs, sign `Claims` (`root`, `publish`, `subscribe`, `exp`), and verify them. `Claims::authorize(path)` scopes verified claims to the path a client dialed, exactly as `moq auth serve` does.

```bash
cargo add moq-auth
```

For command-line use, install the [moq CLI](/setup/install) and run
`moq auth generate|sign|verify`, `moq auth serve` for the
[auth server](/bin/relay/auth#auth-server), or `moq auth sessions|revalidate`
to list live sessions and push a re-check through the relay's internal
listener. Examples:
[`basic.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-auth/examples/basic.rs)
and
[`asymmetric.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-auth/examples/asymmetric.rs).
The TypeScript twin, [`@moq/auth`](/lib/js/auth), reads the same request,
builds the same grant, and mints identical tokens.
API: [docs.rs/moq-auth](https://docs.rs/moq-auth).
