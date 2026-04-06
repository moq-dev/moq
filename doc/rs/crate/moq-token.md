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

The binary is named `moq-token-cli`.

#### Using Nix

```bash
# Run directly
nix run github:moq-dev/moq#moq-token-cli

# Or build and find the binary in ./result/bin/
nix build github:moq-dev/moq#moq-token-cli
```

#### Using Docker

```bash
docker pull kixelated/moq-token-cli
docker run -v "$(pwd):/app" -w /app kixelated/moq-token-cli --key root.jwk generate
```

Multi-arch images (`linux/amd64` and `linux/arm64`) are published to [Docker Hub](https://hub.docker.com/r/kixelated/moq-token-cli).

## CLI Usage

### Generate a Key

```bash
# Symmetric key (HMAC)
moq-token-cli generate --out root.jwk --algorithm HS256

# Asymmetric key pair (RSA)
moq-token-cli generate --algorithm RS256 --out private.jwk --public public.jwk

# Asymmetric key pair (EdDSA)
moq-token-cli generate --algorithm EdDSA --out private.jwk --public public.jwk
```

### Sign a Token

```bash
moq-token-cli sign --key root.jwk \
  --root "rooms/123" \
  --publish "alice" \
  --subscribe "" \
  --expires 1735689600 > alice.jwt
```

### Verify a Token

```bash
moq-token-cli verify --key root.jwk < alice.jwt
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

- [`rs/moq-token/examples/basic.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-token/examples/basic.rs) - Generate a symmetric key, sign a token, verify it, and round-trip the key
- [`rs/moq-token/examples/asymmetric.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-token/examples/asymmetric.rs) - Generate an ECDSA key pair, extract the public key for the relay, sign and verify

For the TypeScript equivalent, see [`js/token/examples/sign-and-verify.ts`](https://github.com/moq-dev/moq/blob/main/js/token/examples/sign-and-verify.ts).

## Token Claims

| Claim | Type | Description |
|-------|------|-------------|
| `root` | string | Root path for all operations |
| `put` | `string \| string[]?` | Publishing permission paths |
| `get` | `string \| string[]?` | Subscription permission paths |
| `cluster` | bool? | Cluster node flag |
| `exp` | number? | Expiration (Unix timestamp) |
| `iat` | number? | Issued at (Unix timestamp) |

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

For key rotation, use the relay's `key_dir` option pointing to a directory or URL. The relay resolves keys on demand by extracting the `kid` (key ID) from the JWT header and fetching the corresponding `{kid}.jwk` file. See [Relay Authentication](/app/relay/auth) for configuration details.

## API Reference

Full API documentation: [docs.rs/moq-token](https://docs.rs/moq-token)

## Next Steps

- Configure [Relay Authentication](/app/relay/auth)
- Deploy a [Relay Server](/app/relay/)
- Learn about [Authentication](/app/relay/auth)
