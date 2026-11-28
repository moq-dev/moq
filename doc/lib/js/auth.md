---
title: "@moq/auth"
description: Answer a relay and mint its JWTs in TypeScript
---

# @moq/auth

[![npm](https://img.shields.io/npm/v/@moq/auth)](https://www.npmjs.com/package/@moq/auth)
[![API](https://img.shields.io/badge/API-reference-blue)](/lib/js/api/auth)

The TypeScript twin of [`moq-auth`](/lib/rs/moq-auth). `RequestSchema` and
`GrantSchema` validate what [moq-relay](/bin/relay/auth) POSTs per session and
what an auth server answers, so a Worker or a Node server implements the
contract with one import. It also generates keys, signs and verifies JWTs, and
checks a connection path against a token's claims exactly as `moq auth serve`
does. A grant can label its session's stats independently of its authorization
root. Tokens are interchangeable with the Rust side. Grants and claims are
`Prefix` sets (from `@moq/net/prefix`, re-exported): `foo` covers `foo`
and everything beneath it, and the empty prefix is everything. `Key.sign` and
the CLI refuse a wildcard such as `foo/**` or `*`. A token, key scope, or grant
spells each prefix as the subtree pattern releases before 0.22 read, `foo/**`
or `**`, and reads that spelling back as the prefix. A claim or grant role is
an `Access`: its prefix array, or `{ path, rate }` with a bitrate cap. A
`Grant` is typed as its wire shape: answer with `GrantSchema.parse(grant)`,
which spells it as every relay reads it, and the relay refuses a field it does
not know. A parsed key spells its scope the same way, and so does `Key.public`,
so write a key you scope by hand from `Key.parse` of it or from `Key.public`. `Claims` spell a token's
`exp`, `iat`, `nbf`, and `jti` as `expires`, `issued`, `notBefore`, and `id`, as
the Rust crate does. Key scopes bound paths only, and `Claims.authorize` checks
paths only.

```bash
bun add @moq/auth
bunx @moq/auth generate --out root.jwk
bunx @moq/auth sign --key root.jwk --root "rooms/123" --publish alice
```

Mint tokens on a server, never in the browser, and prefer asymmetric keys so
the relay holds only the public half. Example:
[`sign-and-verify.ts`](https://github.com/moq-dev/moq/blob/main/js/auth/examples/sign-and-verify.ts).
