---
title: "@moq/token"
description: JWT token library for browsers
---

# @moq/token

JWT token generation and verification for MoQ in browsers.

## Overview

`@moq/token` provides:

- Generate signing keys (HMAC, RSA, ECDSA, EdDSA), individually or as a JWK Set
- Sign and verify JWT tokens
- Authorize a connection path against a token's claims
- Compatible with moq-relay authentication and `moq-token`

The API mirrors the Rust [`moq-token`](/lib/rs/crate/moq-token) crate: `sign` and `verify`
handle the signature, and `authorize` scopes the verified claims to the path a client
dialed. Tokens mint and validate identically on both sides.

## Installation

```bash
bun add @moq/token
```

## Usage

For a complete working example covering key loading, signing, and verification, see [`js/token/examples/sign-and-verify.ts`](https://github.com/moq-dev/moq/blob/main/js/token/examples/sign-and-verify.ts).

## Token Claims

| Claim | Type | Description |
|-------|------|-------------|
| `root` | string? | Root path for operations, defaulting to the top-level path |
| `put` | `string \| string[]?` | Publishing permission paths, relative to `root` |
| `get` | `string \| string[]?` | Subscription permission paths, relative to `root` |
| `exp` | number? | Expiration timestamp |
| `iat` | number? | Issued at timestamp |

## CLI Usage

The package includes a CLI tool:

```bash
# Generate a key
bun run @moq/token generate --key root.jwk

# Sign a token
bun run @moq/token sign --key root.jwk --root "rooms/123" --publish alice

# Verify a token from stdin
bun run @moq/token verify --key root.jwk < token.jwt
```

## Security Considerations

- **Never expose secret keys** in browser code
- Use asymmetric keys when possible
- Generate tokens server-side for production
- Set appropriate expiration times

## Next Steps

- Set up [Relay Authentication](/bin/relay/auth)
- Use [@moq/net](/lib/js/@moq/net) for connections
