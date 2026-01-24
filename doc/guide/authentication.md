---
title: moq-authentication
description: Authentication for the moq-relay
---

# MoQ authentication

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
