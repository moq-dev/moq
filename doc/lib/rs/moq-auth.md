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

- **Request and grant**: `Request` is the JSON a relay POSTs per session event (`connect`, `revalidate`, `end`) with everything it knows: id, node, transport, addresses, SNI and ALPN, the raw path and query, the declared role, and the verified certificate facts. `Grant` is the answer: `publish` and `subscribe` pattern unions, an optional `root` alias, `expires`, `revalidate`, and `tier`. `Grant::validate` refuses a grant that names nothing, asks to be revalidated without a bound or at no interval, or has already expired.
- **Lease**: `lease::Producer` and `lease::Consumer` are the handle a session holds for its grant. The consumer reads the current grant, waits for a change, and learns why the lease ended; the producer updates and revokes. Whoever runs the accept loop builds the producer, so an embedder decides in process with no trait and no HTTP.
- **Client**: `Client::new(url, tls)` and `Client::connect(request, counters)` drive a lease against an auth server over `https://`, `unix://`, or loopback `http://`: revalidate on cadence with jittered backoff through an outage until `expires`, revoke on a refusal, and POST `end` with the reason, duration, and byte counters when the session closes.
- **Keys**: generate HS256/384/512, RS256/384/512, PS256/384/512, ES256/384, or EdDSA keys as JWKs, with a `kid` for rotation and an optional immutable scope that caps every token the key signs.
- **Claims**: `root`, `publish`, `subscribe`, `exp`, `iat`. Grants are [`Pattern`](https://docs.rs/moq-pattern) unions: `foo` is one broadcast, `foo/**` is a subtree, `**` is everything. `Key::sign` and `Key::verify` handle the signature and expiry; a token carrying the retired `put` and `get` prefix lists fails verification.
- **Authorization**: `Claims::authorize(path)` scopes verified claims to the path a client dialed and returns the publish and subscribe patterns relative to it, exactly as the relay does.

```bash
cargo add moq-auth
```

For command-line use, install the [moq CLI](/setup/install) and run
`moq auth generate|sign|verify`. Examples:
[`basic.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-auth/examples/basic.rs)
and
[`asymmetric.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-auth/examples/asymmetric.rs).
The TypeScript twin, [`@moq/auth`](/lib/js/auth), reads the same request,
builds the same grant, and mints identical tokens.
API: [docs.rs/moq-auth](https://docs.rs/moq-auth).
