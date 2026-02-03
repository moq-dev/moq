---
title: "@moq/token"
description: JWT token library for browsers
---

# @moq/token

JWT token generation and verification for MoQ in browsers.

## Overview

`@moq/token` provides:

- Generate JWT tokens in the browser
- Verify tokens client-side
- Compatible with moq-relay authentication

## Installation

```bash
bun add @moq/token
# or
npm add @moq/token
```

## Usage

### Generate a Token

```typescript
import { sign } from "@moq/token";

const token = await sign({
    key: secretKey, // Uint8Array or CryptoKey
    claims: {
        root: "rooms/123",
        pub: "alice",
        sub: "",
        exp: Math.floor(Date.now() / 1000) + 3600, // 1 hour
    },
});

// Use in connection URL
const url = `https://relay.example.com/rooms/123?jwt=${token}`;
```

### Verify a Token

```typescript
import { verify } from "@moq/token";

try {
    const claims = await verify({
        token,
        key: publicKey, // Uint8Array or CryptoKey
    });

    console.log("Root:", claims.root);
    console.log("Publish:", claims.pub);
    console.log("Subscribe:", claims.sub);
} catch (error) {
    console.error("Invalid token:", error);
}
```

### Decode Without Verification

```typescript
import { decode } from "@moq/token";

const claims = decode(token);
// WARNING: Does not verify signature
```

## Key Management

### Generate a Key

```typescript
import { generateKey } from "@moq/token";

// HMAC key (symmetric)
const hmacKey = await generateKey({ algorithm: "HS256" });

// ECDSA key pair (asymmetric)
const { privateKey, publicKey } = await generateKey({
    algorithm: "ES256",
});
```

### Import a Key

```typescript
import { importKey } from "@moq/token";

// From JWK
const key = await importKey({
    jwk: {
        kty: "oct",
        k: "...",
        alg: "HS256",
    },
});

// From raw bytes
const key = await importKey({
    raw: secretBytes,
    algorithm: "HS256",
});
```

### Export a Key

```typescript
import { exportKey } from "@moq/token";

const jwk = await exportKey(key);
// Store or transmit JWK
```

## Token Claims

| Claim | Type | Description |
|-------|------|-------------|
| `root` | string | Root path for operations |
| `pub` | string? | Publishing permission |
| `sub` | string? | Subscription permission |
| `cluster` | boolean | Cluster node flag |
| `exp` | number | Expiration timestamp |
| `iat` | number | Issued at timestamp |

## Security Considerations

- **Never expose secret keys** in browser code
- Use asymmetric keys when possible
- Generate tokens server-side for production
- Set appropriate expiration times

## Integration Example

```typescript
import * as Moq from "@moq/lite";
import { sign } from "@moq/token";

async function connectWithAuth(key: CryptoKey, room: string, user: string) {
    const token = await sign({
        key,
        claims: {
            root: `rooms/${room}`,
            pub: user,
            sub: "",
            exp: Math.floor(Date.now() / 1000) + 3600,
        },
    });

    const connection = await Moq.connect(
        `https://relay.example.com/rooms/${room}?jwt=${token}`
    );

    return connection;
}
```

## Next Steps

- Set up [Relay Authentication](/app/relay/auth)
- Learn about [Authentication concepts](/app/relay/auth)
- Use [@moq/lite](/js/@moq/lite) for connections
