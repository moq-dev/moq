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

The client connects with the token. The connection path can be the root or any parent:

```text
# Connect at the token's root
https://relay.example.com/demo?jwt=eyJhbGciOiJIUzI1NiIs...

# Connect at the server root (permissions still scoped to demo/)
https://relay.example.com/?jwt=eyJhbGciOiJIUzI1NiIs...
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
4. Checks the token's permissions cover the connection path

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

### Connection Path

The client's connection URL path does **not** need to match the token's `root` exactly. The connection path determines the scope of the session — all publish/subscribe operations are relative to it.

- If the connection path **extends** the root (e.g., token root=`demo`, connect to `/demo/room`), permissions are narrowed to only paths under `/demo/room`.
- If the connection path is a **parent** of the root (e.g., token root=`demo`, connect to `/`), permissions still apply but are scoped to the token's root. You can only access paths under `demo/`.
- If the connection path is **unrelated** to the root (e.g., token root=`demo`, connect to `/other`), the connection is rejected.

The connection is also rejected if the resulting permissions are empty (no publish or subscribe paths remain after scoping).

### Unified Auth API (`--auth-api`)

Instead of wiring `--auth-key-dir` (URL form) and `--auth-public-api` separately, a relay can resolve **everything it needs to authorize a connection in one call** with `--auth-api <url>` (env `MOQ_AUTH_API`, or `auth_api` under `[auth]` in TOML). It is mutually exclusive with `--auth-key`, `--auth-key-dir`, `--auth-public`, and `--auth-public-api` (configuring both is a startup error); `--auth-domain` still applies.

Per connection the relay issues `GET <base>?root=<path>&kid=<kid>&mtls=true` over the same cached, mTLS-gated HTTP client used by the other auth fetches. All three are query params (the base URL is used verbatim): `root` is the connection path (slashes preserved); `kid` is sent only when the connection carries a JWT (value taken from its header); `mtls=true` is sent only when the peer presented a verified client cert. The JSON response has four **optional** fields:

- `alias` — the canonical full root to scope this connection to: the path with its first segment (a stable id, current vanity, or recently-changed vanity) resolved to the project's canonical id, the rest of the path preserved (e.g. `demo/room/cam` → `x7k2qp/room/cam`). The relay uses it verbatim, so the server owns the entire mapping. Absent → the request path is used unchanged.
- `public` — `{ "subscribe": [...], "publish": [...] }` anonymous access prefixes (relative to the root), used when there is no JWT. Absent → no public access.
- `key` — the verifying JWK (a JSON object, deserialized directly) for the requested `kid`. Absent → key-not-found, and the JWT is rejected.
- `internal` — the billing tier. The relay forwards `mtls=true` and lets the API decide; absent defaults to internal for mTLS peers and external for JWT/public connections. So the API can promote a first-party token to internal or demote a cert-verified connection to external.

This lets a project stay reachable by both its stable id and its current/old vanity path, all mapping to the same broadcast tree: with the API resolving `demo` → `x7k2qp`, both `cdn.moq.dev/demo/foo` and `cdn.moq.dev/x7k2qp/foo` scope to `/x7k2qp/foo`.

```toml
[auth]
auth_api = "https://api.moq.dev/cluster/auth"
```

Unlike the standalone flags, the unified call **fails closed**: any network error, non-2xx status, or unparseable response rejects the connection. The verifying key itself comes from this call, so there is no safe fallback; the endpoint's `Cache-Control` softens transient failures. This applies to mTLS peers as well, including root (`/`) connections such as cluster peers: when an auth API is configured it is the source of truth for every connection (so it can alias and tier the root too), and a failed lookup rejects the connection so the peer reconnects and self-heals once the API recovers. The only fail-open case is when **no** auth API is configured, where the client certificate is the sole credential and the path is used unchanged.

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

## mTLS Peer Authentication

In addition to JWT auth, the relay can authenticate peers via mutual TLS. When
the server is configured with a trusted root CA, any client that presents a
certificate chaining to that CA is granted **full publish and subscribe access
within the connection URL path**. The URL path scopes the grant exactly like a
JWT's `root` claim, so a peer dialing `/demo` can only publish and subscribe
under `demo/`. A peer dialing `/` (as cluster nodes do) gets an empty root and
unscoped, cluster-wide access. The token is also flagged as internal, which only
selects the stats tier used for billing; it grants no extra permissions.

This is primarily intended for relay-to-relay (clustering) authentication, as a
simpler alternative to distributing long-lived JWTs.

Client certificate presentation is **optional**: connections without a
certificate fall through to the normal JWT path unchanged.

```toml
[tls]
cert = ["/etc/moq/server.pem"]
key  = ["/etc/moq/server.key"]
# One or more PEM files containing the CAs trusted to sign peer certificates.
root = ["/etc/moq/peer-ca.pem"]
```

The certificate is used only to authenticate the peer: the relay verifies the
chain against the configured CA and reads nothing else from it. A node
advertises its own identity by setting `--cluster-mesh` to its
externally-reachable URL, which it publishes on the cluster origin for other
peers to discover and dial.

Only the `quinn` QUIC backend supports mTLS; configuring `tls.root` with any
other backend is a startup error.

## Example Configurations

See the [`demo/relay/`](https://github.com/moq-dev/moq/tree/main/demo/relay) directory for complete working configuration files, including authentication setup:

- **Development** - [`demo/relay/root.toml`](https://github.com/moq-dev/moq/blob/main/demo/relay/root.toml) (single key with anonymous access)
- **Production** - [`demo/relay/prod.toml`](https://github.com/moq-dev/moq/blob/main/demo/relay/prod.toml) (key and key directory options)

## Library Usage

### Rust

- [`rs/moq-token/examples/basic.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-token/examples/basic.rs) - Symmetric key generation, signing, and verification
- [`rs/moq-token/examples/asymmetric.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-token/examples/asymmetric.rs) - Asymmetric key pair with public key extraction

### TypeScript

See [`js/token/examples/sign-and-verify.ts`](https://github.com/moq-dev/moq/blob/main/js/token/examples/sign-and-verify.ts) for a complete working example of signing and verifying tokens.

## See Also

- [moq-token (Rust)](/lib/rs/crate/moq-token) - Rust library and CLI
- [@moq/token](/lib/js/@moq/token) - TypeScript library and CLI
- [Relay Configuration](/bin/relay/config) - Full config reference
