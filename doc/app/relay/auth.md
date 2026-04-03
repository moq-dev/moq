---
title: Authentication
description: JWT-based access control for moq-relay
---

# Authentication

moq-relay uses JWT (JSON Web Tokens) for authentication and authorization. Tokens control who can publish or subscribe to which paths.

## Overview

There are two authentication modes:

### Single Key (`--auth-key`)

A single JWK file used to verify all tokens. No `kid` header is required in JWTs. Good for development and simple deployments.

### Key Directory (`--auth-key-dir`)

For production use with key rotation. Keys are resolved on demand by extracting the `kid` from the JWT header and fetching the corresponding key file.

1. Generate signing keys (a random key ID is assigned automatically)
2. Store each key as `{kid}.jwk` in a directory or serve via HTTP
3. Configure the relay with the key directory or URL
4. Issue tokens to clients with their allowed paths
5. Clients connect with `?jwt=<token>` query parameter

## Quick Start

### Generate a Key

Using the Rust CLI:

```bash
# Symmetric key (simpler, key must stay secret)
moq-token-cli generate --out my-key.jwk

# Save to a directory as {kid}.jwk
moq-token-cli generate --out-dir ./keys/

# Asymmetric key (private signs, public verifies)
moq-token-cli generate --algorithm ES256 --out private.jwk --public public.jwk

# Asymmetric key, both saved to directories as {kid}.jwk
moq-token-cli generate --algorithm ES256 --out-dir ./private/ --public-dir ./keys/
```

A random key ID is generated if `--id` is not specified.

### Configure the Relay

Single key (simplest):

```toml
[auth]
key = "my-key.jwk"
```

Key directory (for key rotation):

```toml
[auth]
# Point to the public keys directory (from --public-dir).
# For asymmetric algorithms, the relay only needs public keys to verify tokens.
key_dir = "/etc/moq/keys/"
```

Remote key server:

```toml
[auth]
key_dir = "https://api.example.com/keys"
```

### Issue a Token

```bash
# Allow publishing to demo/my-stream and subscribing to anything under demo/
moq-token-cli sign --key my-key.jwk --root demo --publish my-stream --subscribe ""
```

The client connects with the token:

```text
https://relay.example.com/demo/my-stream?jwt=eyJhbGciOiJIUzI1NiIs...
```

## Key Resolution

### Single Key Mode (`--auth-key`)

The relay uses the specified key file to verify all incoming JWTs. No `kid` header is required in the token.

### Key Directory Mode (`--auth-key-dir`)

Key files are stored as JSON by default. Legacy base64url-encoded files are also supported for backwards compatibility. Use `--base64` when generating keys if you prefer the base64url format.

When a client connects with a JWT, the relay:

1. Decodes the JWT header to extract the `kid` (key ID)
2. Looks up the key from the configured source: `{dir}/{kid}.jwk` or `{url}/{kid}.jwk`
3. Verifies the JWT signature with the resolved key
4. Checks the token's `root` claim matches the connection path

Key IDs must contain only alphanumeric characters, hyphens, and underscores.

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
key = "my-key.jwk"
public = "anon"  # Anyone can publish/subscribe to anon/*
```

Set `public = ""` to make everything public (development only).

## Example Configurations

### Development (no auth)

```toml
[auth]
public = ""
```

### Development (single key)

```toml
[auth]
key = "dev.jwk"
public = "anon"
```

### Production (local keys with rotation)

```toml
[auth]
key_dir = "/etc/moq/keys/"
```

### Production (remote key server)

```toml
[auth]
key_dir = "https://api.example.com/keys"
```

## Library Usage

### TypeScript

```typescript
import { generate, load, sign, type Claims } from "@moq/token"

// Generate a key (random kid assigned automatically)
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
moq-token-cli sign --key my-key.jwk \
  --root demo \
  --publish my-stream \
  --subscribe "" \
  --expires 3600
```

## See Also

- [moq-token (Rust)](/rs/crate/moq-token) - Rust library and CLI
- [@moq/token](/js/@moq/token) - TypeScript library and CLI
- [Relay Configuration](/app/relay/config) - Full config reference
