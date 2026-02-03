---
title: moq-token
description: JWT authentication library for MoQ
---

# moq-token

[![crates.io](https://img.shields.io/crates/v/moq-token)](https://crates.io/crates/moq-token)
[![docs.rs](https://docs.rs/moq-token/badge.svg)](https://docs.rs/moq-token)

JWT authentication library and CLI tool for MoQ relay authentication.

## Overview

`moq-token` provides:

- **Library** - Generate and verify JWT tokens in Rust
- **CLI** - Command-line tool for key and token management
- **Multiple algorithms** - HMAC, RSA, ECDSA, EdDSA

## Installation

### Library

Add to your `Cargo.toml`:

```toml
[dependencies]
moq-token = "0.1"
```

### CLI

```bash
cargo install moq-token-cli
```

The binary is named `moq-token`.

## CLI Usage

### Generate a Key

```bash
# Symmetric key (HMAC)
moq-token --key root.jwk generate --algorithm HS256

# Asymmetric key pair (RSA)
moq-token --key private.jwk generate --public public.jwk --algorithm RS256

# Asymmetric key pair (EdDSA)
moq-token --key private.jwk generate --public public.jwk --algorithm EdDSA
```

### Sign a Token

```bash
moq-token --key root.jwk sign \
  --root "rooms/123" \
  --publish "alice" \
  --subscribe "" \
  --expires 1735689600 > alice.jwt
```

### Verify a Token

```bash
moq-token --key root.jwk verify < alice.jwt
```

## Supported Algorithms

**Symmetric (HMAC):**
- HS256
- HS384
- HS512

**Asymmetric (RSA):**
- RS256, RS384, RS512
- PS256, PS384, PS512

**Asymmetric (Elliptic Curve):**
- EC256, EC384
- EdDSA

## Library Usage

### Generate a Key

```rust
use moq_token::*;

// Generate HMAC key
let key = Key::generate(Algorithm::HS256)?;
key.save("root.jwk")?;

// Generate RSA key pair
let (private_key, public_key) = Key::generate_pair(Algorithm::RS256)?;
private_key.save("private.jwk")?;
public_key.save("public.jwk")?;
```

### Sign a Token

```rust
use moq_token::*;

let key = Key::load("root.jwk")?;

let claims = Claims {
    root: "rooms/123".to_string(),
    publish: Some("alice".to_string()),
    subscribe: Some("".to_string()),
    cluster: false,
    expires: 1735689600,
};

let token = key.sign(&claims)?;
println!("Token: {}", token);
```

### Verify a Token

```rust
use moq_token::*;

let key = Key::load("root.jwk")?;
let claims = key.verify(&token)?;

println!("Root: {}", claims.root);
println!("Publish: {:?}", claims.publish);
println!("Subscribe: {:?}", claims.subscribe);
```

## Token Claims

| Claim | Type | Description |
|-------|------|-------------|
| `root` | string | Root path for all operations |
| `pub` | string? | Publishing permission (path suffix) |
| `sub` | string? | Subscription permission (path suffix) |
| `cluster` | bool | Cluster node flag |
| `exp` | number | Expiration (Unix timestamp) |
| `iat` | number | Issued at (Unix timestamp) |

## Integration with moq-relay

Configure the relay to use your key:

```toml
[auth]
key = "root.jwk"
public = "anon"  # Optional: anonymous access
```

See [Relay Authentication](/app/relay/auth) for details.

## Security Considerations

- **Symmetric keys** should only be used when the same entity signs and verifies
- **Asymmetric keys** are preferred for distributed systems (relay only needs public key)
- **Token expiration** should be set appropriately for your use case
- **Secure transmission** - Only transmit tokens over HTTPS
- **Secure storage** - Keep private keys secure

## JWK Set Support

For key rotation, you can host a JWK set:

```json
{
  "keys": [
    {
      "kid": "2026-01-01",
      "alg": "RS256",
      "kty": "RSA",
      "n": "...",
      "e": "AQAB"
    }
  ]
}
```

Configure the relay:

```toml
[auth]
key = "https://auth.example.com/keys.json"
refresh_interval = 86400
```

## API Reference

Full API documentation: [docs.rs/moq-token](https://docs.rs/moq-token)

## Next Steps

- Configure [Relay Authentication](/app/relay/auth)
- Deploy a [Relay Server](/app/relay/)
- Learn about [Authentication](/app/relay/auth)
