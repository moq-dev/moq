---
title: moq-token
description: JWT keys, signing, verification, and path authorization
---

# moq-token

[![crates.io](https://img.shields.io/crates/v/moq-token)](https://crates.io/crates/moq-token)
[![docs.rs](https://docs.rs/moq-token/badge.svg)](https://docs.rs/moq-token)

The tokens [moq-relay](/bin/relay/auth) authenticates with, as a library. Use
it in an auth service that mints tokens for clients, or in your own server
that wants the relay's path rules.

- **Keys**: generate HS256/384/512, RS256/384/512, PS256/384/512, ES256/384, or EdDSA keys as JWKs, with a `kid` for rotation and an optional immutable scope that caps every token the key signs.
- **Claims**: `root`, `put`, `get`, `exp`, `iat`. `Key::sign` and `Key::verify` handle the signature and expiry.
- **Authorization**: `Claims::authorize(path)` scopes verified claims to the path a client dialed and returns the publish and subscribe prefixes, exactly as the relay does.

```bash
cargo add moq-token
```

The CLI is `moq token` in [moq-cli](/bin/cli) or the standalone
`moq-token-cli` package; both wrap this crate. Examples:
[`basic.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-token/examples/basic.rs)
and
[`asymmetric.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-token/examples/asymmetric.rs).
The TypeScript twin, [`@moq/token`](/lib/js/token), mints identical tokens.
API: [docs.rs/moq-token](https://docs.rs/moq-token).
