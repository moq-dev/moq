---
title: "@moq/token"
description: JWT token library for browsers
---

# @moq/token

JWT token generation and verification for MoQ in browsers.

## Overview

`@moq/token` provides:

- Generate signing keys (HMAC, RSA, ECDSA, EdDSA)
- Sign and verify JWT tokens
- Compatible with moq-relay authentication and `moq-token-cli`

## Installation

```bash
bun add @moq/token
```

## Usage

For a complete working example covering key loading, signing, and verification, see [`js/token/examples/sign-and-verify.ts`](https://github.com/moq-dev/moq/blob/main/js/token/examples/sign-and-verify.ts).

## Token Claims

| Claim | Type | Description |
|-------|------|-------------|
| `root` | string | Root path for operations |
| `put` | `string \| string[]?` | Publishing permission paths |
| `get` | `string \| string[]?` | Subscription permission paths |
| `cluster` | boolean? | Cluster node flag |
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
bun run @moq/token verify --key root.jwk --root "rooms/123" < token.jwt
```

## Security Considerations

- **Never expose secret keys** in browser code
- Use asymmetric keys when possible
- Generate tokens server-side for production
- Set appropriate expiration times

## Next Steps

- Set up [Relay Authentication](/app/relay/auth)
- Use [@moq/lite](/js/@moq/lite) for connections
