---
title: Authentication
description: Authentication for the moq-relay
---

# Relay Authentication

The MoQ Relay authenticates via JWT-based tokens. Generally there are two different approaches you can choose from:
- asymmetric keys: using a public and private key to separate signing and verifying keys for more security
- symmetric key: using a single secret key for signing and verifying, less secure

## Symmetric key

1. Generate a secret key:
```bash
moq-token --key root.jwk generate --algorithm HS256
```
:::details You can also choose a different algorithm
- HS256
- HS384
- HS512
:::

2. Configure relay:
:::code-group
```toml [relay.toml]
[auth]
# public = "anon"     # Optional: allow anonymous access to anon/**
key = "root.jwk"    # JWT key for authenticated paths
```
:::

3. Generate tokens:
```bash
moq-token --key root.jwk sign \
  --root "rooms/123" \
  --publish "alice" \
  --subscribe "" \
  --expires 1735689600 > alice.jwt
```

## Asymmetric keys

Generally asymmetric keys can be more secure because you don't need to distribute the signing key to every relay instance, the relays only need to verifying (public) key.

1. Generate a public and private key:
```bash
moq-token --key private.jwk generate --public public.jwk --algorithm RS256
```
:::details You can also choose a different algorithm
- RS256
- RS384
- RS512
- PS256
- PS384
- PS512
- EC256
- EC384
- EdDSA
:::

2. Now the relay only requires the public key:
:::code-group
```toml [relay.toml]
[auth]
# public = "anon"     # Optional: allow anonymous access to anon/**
key = "public.jwk"    # JWT key for authenticated paths
```
:::

3. Generate tokens using the private key:
```bash
moq-token --key private.jwk sign \
  --root "rooms/123" \
  --publish "alice" \
  --subscribe "" \
  --expires 1735689600 > alice.jwt
```

## JWK set authentication

Instead of storing a public key locally in a file, it may also be retrieved from a server hosting a JWK set. This can be a simple static site serving a JSON file, or a fully OIDC compliant Identity Provider. That way you can easily implement automatic key rotation.

::: info
This approach only works with asymmetric authentication.
:::

To set this up, you need to have an HTTPS server hosting a JWK set that looks like this:
```json
{
  "keys": [
    {
      "kid": "2026-01-01",
      "alg": "RS256",
      "key_ops": [
        "verify"
      ],
      "kty": "RSA",
      "n": "zMsjX1oDV2SMQKZFTx4_qCaD3iIek9s1lvVaymr8bEGzO4pe6syCwBwLmFwaixRv7MMsuZ0nIpoR3Slpo-ZVyRxOc8yc3DcBZx49S_UQcM76E4MYbH6oInrEP8QL2bsstHrYTqTyPPjGwQJVp_sZdkjKlF5N-v5ohpn36sI8PXELvfRY3O3bad-RmSZ8ZOG8CYnJvMj_g2lYtGMMThnddnJ49560ahUNqAbH6ru---sHtdYHcjTIaWX4HYP6Y_KjA6siDZTGTThpaEW45LKcDQWM9sYvx_eAstaC-1rz8Z_6fDgKFWr7qcP5U2NmJ0c-IGSu_8OkftgRH4--Z5mzBQ",
      "e": "AQAB"
    },
    {
      "kid": "2025-12-01",
      "alg": "EdDSA",
      "key_ops": [
        "verify"
      ],
      "kty": "OKP",
      "crv": "Ed25519",
      "x": "2FSK2q_o_d5ernBmNQLNMFxiA4-ypBSa4LsN30ZjUeU"
    }
  ]
}
```

:::tip The following must be considered:
- Every JWK MUST be public and contain no private key information
- If your JWK set contains more than one key:
  1. Every JWK MUST have a `kid` so they can be identified on verification
  2. Your JWT tokens MUST contain a `kid` in their header
  3. `kid` can be an arbitrary string
:::

Configure the relay:
:::code-group
```toml [relay.toml]
[auth]
# public = "anon"                                               # Optional: allow anonymous access to anon/**

key = "https://auth.example.com/keys.json"                      # JWK set URL for authenticated paths
refresh_interval = 86400                                   # Optional: refresh the JWK set every N seconds, no refreshing if omitted
```
:::

## Anonymous Access

If you don't care about security, anonymous access is supported.
The relay can be configured with a single public prefix, usually "anon".
This is obviously not recommended in production especially because broadcast paths are not unique and can be hijacked.

**Example URL**: `https://cdn.moq.dev/anon`

**Example Configuration:**
```toml
# relay.toml
[auth]
public = "anon"  # Allow anonymous access to anon/**
key = "root.jwk" # Require a token for all other paths
```

If you really, really just don't care, then you can allow all paths.

**Fully Unauthenticated**
```toml
# relay.toml
[auth]
public = ""  # Allow anonymous access to everything
```

And if you want to require an auth token, you can omit the `public` field entirely.
**Fully Authenticated**
```toml
# relay.toml
[auth]
key = "root.jwk" # Require a token for all paths
```

## Token Claims

An token can be passed via the `?jwt=` query parameter in the connection URL:

**Example URL**: `https://cdn.moq.dev/demo?jwt=<base64-jwt-token>`

**WARNING**: These tokens are only as secure as the delivery.
Make sure that any secrets are securely transmitted (ex. via HTTPS) and stored (ex. secrets manager).
Avoid logging this query parameter if possible; we'll switch to an `Authentication` header once WebTransport supports it.

The token contains permissions that apply to the session.
It can also be used to prevent publishing (read-only) or subscribing (write-only) on a per-path basis.

**Example Token (unsigned)**
```json
{
  "root": "room/123",  // Root path for all operations
  "pub": "alice",      // Publishing permissions (optional)
  "sub": "",           // Subscription permissions (optional)
  "cluster": false,    // Cluster node flag
  "exp": 1703980800,   // Expiration (unix timestamp)
  "iat": 1703977200    // Issued at (unix timestamp)
}
```

This token allows:
- Connect to `https://cdn.moq.dev/room/123`
- Cannot connect to: `https://cdn.moq.dev/secret` (wrong root)
- Publish to `alice/camera`
- Cannot publish to: `bob/camera` (only alice)
- Subscribe to `bob/screen`
- Cannot subscribe to: `../secret` (scope enforced)

A token may omit either the `pub` or `sub` field to make a read-only or write-only token respectively.
An empty string means no restrictions.

Note that there are implicit `/` delimiters added when joining paths (except for empty strings).
Leading and trailing slashes are ignored within a token.

All subscriptions and announcements are relative to the connection URL.
These would all resolves to the same broadcast:
- `CONNECT https://cdn.moq.dev/room/123` could `SUBSCRIBE alice`.
- `CONNECT https://cdn.moq.dev/room` could `SUBSCRIBE 123/alice`.
- `CONNECT https://cdn.moq.dev` could `SUBSCRIBE room/123/alice`.


The connection URL must contain the root path within the token.
It's possible use a more specific path, potentially losing permissions in the process.

## Next Steps

- Configure [Clustering](/relay/cluster)
- Deploy to [Production](/relay/production)
- Learn about [Authentication concepts](/concepts/authentication)
