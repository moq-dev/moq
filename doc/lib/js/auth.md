---
title: "@moq/auth"
description: Answer a relay and mint its JWTs in TypeScript
---

# @moq/auth

[![npm](https://img.shields.io/npm/v/@moq/auth)](https://www.npmjs.com/package/@moq/auth)

The TypeScript twin of [`moq-auth`](/lib/rs/moq-auth). `RequestSchema` and
`GrantSchema` validate what [moq-relay](/bin/relay/auth) POSTs per session and
what an auth server answers, so a Worker or a Node server implements the
contract with one import. Generate keys (HMAC, RSA, ECDSA, EdDSA, individually
or as a JWK set), `Key.sign` and `Key.verify` strict claims, `Key.decode` an unvalidated signed payload, and `authorize` a connection path
against the claims exactly as `moq auth serve` does. Tokens are interchangeable with
the Rust side. Grants and claims are
[`Pattern`](https://www.npmjs.com/package/@moq/pattern) unions: `foo` is one
broadcast, `foo/**` is a subtree, `**` is everything.

```bash
bun add @moq/auth
bun run @moq/auth generate --out root.jwk
bun run @moq/auth sign --key root.jwk --root "rooms/123" --publish 'alice/**'
```

Mint tokens on a server, never in the browser, and prefer asymmetric keys so
the relay holds only the public half. Example:
[`sign-and-verify.ts`](https://github.com/moq-dev/moq/blob/main/js/auth/examples/sign-and-verify.ts).
