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
[`Pattern`](https://www.npmjs.com/package/@moq/pattern) unions: `foo` is one
broadcast, `foo/**` is a subtree, `**` is everything. Parsed claim and grant roles are
pattern string arrays; `authorize` returns reduced `Patterns` for each role.
Object roles, including bitrate caps, are refused during parsing.

```bash
bun add @moq/auth
bunx @moq/auth generate --out root.jwk
bunx @moq/auth sign --key root.jwk --root "rooms/123" --publish 'alice/**'
```

Mint tokens on a server, never in the browser, and prefer asymmetric keys so
the relay holds only the public half. Example:
[`sign-and-verify.ts`](https://github.com/moq-dev/moq/blob/main/js/auth/examples/sign-and-verify.ts).
