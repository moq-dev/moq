---
title: Authentication
description: JWT-based access control for moq-relay
---

# Authentication

moq-relay uses JWT (JSON Web Tokens) for authentication and authorization. Tokens control who can publish or subscribe to which paths.

## Overview

The authentication flow:
1. Generate a signing key with a key ID (`kid`)
2. Store the key file as `{kid}.jwk` in a directory or serve it via HTTP
3. Configure the relay with the key directory or URL
4. Issue tokens to clients with their allowed paths
5. Clients connect with `?jwt=<token>` query parameter

The relay resolves keys on demand by extracting the `kid` from the JWT header and fetching the corresponding key.

## Quick Start

### Generate a Key

Using the Rust CLI:
```bash
# Symmetric key (simpler, key must stay secret)
moq-token-cli --key keys/my-key.jwk generate --id my-key

# Asymmetric key (private signs, public verifies)
moq-token-cli --key private.jwk generate --id my-key --algorithm ES256 --public keys/my-key.jwk
```

The `--id` flag sets the key ID (`kid`), which is used to look up the key later. The key file should be named `{kid}.jwk`.

### Configure the Relay

```toml
[auth]
# Directory containing JWK files named by key ID
keys = "keys"

# Optional: allow anonymous access to a path prefix
public = "anon"
```

Or with a remote key server:
```toml
[auth]
# Base URL for key lookup — fetches {url}/{kid}.jwk
keys = "https://api.example.com/keys"
```

### Issue a Token

```bash
# Allow publishing to demo/my-stream and subscribing to anything under demo/
moq-token-cli --key keys/my-key.jwk sign --root demo --publish my-stream --subscribe ""
```

The client connects with the token:
```text
https://relay.example.com/demo/my-stream?jwt=eyJhbGciOiJIUzI1NiIs...
```

## Key Resolution

When a client connects with a JWT, the relay:
1. Decodes the JWT header to extract the `kid` (key ID)
2. Looks up the key from the configured source
3. Verifies the JWT signature with the resolved key
4. Checks the token's `root` claim matches the connection path

### File Mode

With `keys = "/path/to/keys"`, the relay reads `/path/to/keys/{kid}.jwk` from disk on each request.

### URL Mode

With `keys = "https://api.example.com/keys"`, the relay fetches `https://api.example.com/keys/{kid}.jwk` with HTTP caching:

- Respects `Cache-Control: max-age` and `stale-while-revalidate` headers
- Uses `ETag` / `If-None-Match` for efficient revalidation
- Fresh keys are served from memory without network requests
- Stale keys within the revalidation window are returned immediately while revalidating in the background

## Token Claims

The JWT payload contains these claims:

| Claim | Description |
|-------|-------------|
| `root` | Base path for publish/subscribe permissions |
| `pub` | Suffix appended to root for publish permission |
| `sub` | Suffix appended to root for subscribe permission |
| `exp` | Expiration time (Unix timestamp) |
| `iat` | Issued-at time (Unix timestamp) |

### Path Matching

The `root` claim sets a base path. The `pub` and `sub` claims are suffixes:

```text
Full publish path = root + "/" + pub
Full subscribe path = root + "/" + sub
```

An empty suffix (`""`) allows access to anything under the root.

**Examples:**

| root | pub | sub | Can publish | Can subscribe |
|------|-----|-----|-------------|---------------|
| `demo` | `my-stream` | `""` | `demo/my-stream` | `demo/*` |
| `rooms/123` | `alice` | `""` | `rooms/123/alice` | `rooms/123/*` |
| `""` | `""` | `""` | Everything | Everything |

## Supported Algorithms

### Symmetric (HMAC)
The same key signs and verifies. Simpler setup, but the key must be kept secret everywhere it's used.

- `HS256` - HMAC with SHA-256 (default)
- `HS384` - HMAC with SHA-384
- `HS512` - HMAC with SHA-512

### Asymmetric (RSA/ECDSA)
Private key signs, public key verifies. The relay only needs the public key, so compromise of the relay doesn't leak signing capability.

- `RS256`, `RS384`, `RS512` - RSA PKCS#1 v1.5
- `PS256`, `PS384`, `PS512` - RSA PSS
- `ES256`, `ES384` - ECDSA
- `EdDSA` - Edwards-curve DSA

## Anonymous Access

The `public` setting allows unauthenticated access to a path prefix:

```toml
[auth]
keys = "keys"
public = "anon"  # Anyone can publish/subscribe to anon/*
```

Set `public = ""` to make everything public (development only).

## Example Configurations

### Development (no auth)
```toml
[auth]
public = ""
```

### Public viewing, authenticated publishing
```toml
[auth]
keys = "keys"
public = "streams"  # Anyone can subscribe to streams/*
# Publishing requires a token
```

### Fully authenticated (local keys)
```toml
[auth]
keys = "keys"
# Everything requires a token
```

### Fully authenticated (remote key server)
```toml
[auth]
keys = "https://api.example.com/keys"
```

## Library Usage

### TypeScript
```typescript
import { generate, load, sign, type Claims } from "@moq/token"

// Generate a key
const keyString = await generate('HS256')

// Load and sign
const key = load(keyString)
const claims: Claims = {
  root: "demo",
  pub: "my-stream",
  sub: "",
  exp: Math.floor(Date.now() / 1000) + 3600,
}
const token = await sign(key, claims)
```

### Rust
```bash
moq-token-cli --key keys/my-key.jwk sign \
  --root demo \
  --publish my-stream \
  --subscribe "" \
  --expires 3600
```

## See Also

- [moq-token (Rust)](/rs/crate/moq-token) - Rust library and CLI
- [@moq/token](/js/@moq/token) - TypeScript library and CLI
- [Relay Configuration](/app/relay/config) - Full config reference
