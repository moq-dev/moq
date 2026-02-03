---
title: Authentication
description: JWT-based access control for moq-relay
---

# Authentication

moq-relay uses JWT (JSON Web Tokens) for authentication and authorization. Tokens control who can publish or subscribe to which paths.

## Overview

The authentication flow:
1. Generate a signing key (shared secret or public/private keypair)
2. Configure the relay with the verification key
3. Issue tokens to clients with their allowed paths
4. Clients connect with `?jwt=<token>` query parameter

## Quick Start

### Generate a Key

Using the Rust CLI:
```bash
# Symmetric key (simpler, key must stay secret)
moq-token --key root.jwk generate

# Asymmetric key (private signs, public verifies)
moq-token --key private.jwk generate --algorithm RS256 --public public.jwk
```

Using the JavaScript CLI:
```bash
bunx @moq/token generate --key root.jwk
```

### Configure the Relay

```toml
[auth]
# Path to the verification key
# - Symmetric: the shared secret (e.g., "root.jwk")
# - Asymmetric: the public key (e.g., "public.jwk")
key = "root.jwk"

# Optional: allow anonymous access to a path prefix
public = "anon"
```

### Issue a Token

```bash
# Allow publishing to demo/my-stream and subscribing to anything under demo/
moq-token --key root.jwk sign --root demo --publish my-stream --subscribe ""
```

The client connects with the token:
```text
https://relay.example.com/demo/my-stream?jwt=eyJhbGciOiJIUzI1NiIs...
```

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
key = "root.jwk"
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
key = "root.jwk"
public = "streams"  # Anyone can subscribe to streams/*
# Publishing requires a token
```

### Fully authenticated
```toml
[auth]
key = "public.jwk"  # Asymmetric, public key only
# Everything requires a token
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
moq-token --key root.jwk sign \
  --root demo \
  --publish my-stream \
  --subscribe "" \
  --expires 3600
```

## See Also

- [moq-token (Rust)](/rs/crate/moq-token) - Rust library and CLI
- [@moq/token](/js/@moq/token) - TypeScript library and CLI
- [Relay Configuration](/app/relay/config) - Full config reference
