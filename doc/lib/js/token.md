---
title: "@moq/token"
description: Mint and verify relay JWTs in TypeScript
---

# @moq/token

[![npm](https://img.shields.io/npm/v/@moq/token)](https://www.npmjs.com/package/@moq/token)

The TypeScript twin of [`moq-token`](/lib/rs/moq-token). Generate keys (HMAC,
RSA, ECDSA, EdDSA, individually or as a JWK set), `sign` and `verify` tokens,
and `authorize` a connection path against the claims exactly as
[moq-relay](/bin/relay/auth) does. Tokens are interchangeable with the Rust
side.

```bash
bun add @moq/token
bun run @moq/token generate --key root.jwk
bun run @moq/token sign --key root.jwk --root "rooms/123" --publish alice
```

Mint tokens on a server, never in the browser, and prefer asymmetric keys so
the relay holds only the public half. Example:
[`sign-and-verify.ts`](https://github.com/moq-dev/moq/blob/main/js/token/examples/sign-and-verify.ts).
