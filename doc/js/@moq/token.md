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

### Load a Key

```typescript
import { load, loadPublic } from "@moq/token";

// Load a key from JWK JSON string
const key = load(jwkString);

// Load only the public portion of an asymmetric key
const publicKey = loadPublic(jwkString);
```

### Generate a Key

```typescript
import { generate } from "@moq/token";

// HMAC key (symmetric)
const hmacKey = await generate("HS256");

// RSA key pair (asymmetric)
const rsaKey = await generate("RS256");

// EdDSA key pair (asymmetric)
const edKey = await generate("EdDSA");
```

### Extract Public Key

```typescript
import { toPublicKey } from "@moq/token";

const publicKey = toPublicKey(rsaKey);
```

### Sign a Token

```typescript
import { load, sign } from "@moq/token";

const key = load(jwkString);

const token = await sign(key, {
    root: "rooms/123",
    put: ["alice"],
    get: [""],
    exp: Math.floor(Date.now() / 1000) + 3600, // 1 hour
});
```

### Verify a Token

```typescript
import { load, verify } from "@moq/token";

const key = load(jwkString);

try {
    const claims = await verify(key, token, "rooms/123");
    console.log("Root:", claims.root);
    console.log("Publish:", claims.put);
    console.log("Subscribe:", claims.get);
} catch (error) {
    console.error("Invalid token:", error);
}
```

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
