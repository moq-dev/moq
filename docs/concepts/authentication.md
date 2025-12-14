# Authentication

[moq-relay](/rust/moq-relay) uses JWT tokens in the URL for authentication and authorization.
This scopes sessions to a selected root path with additional rules for publishing and subscribing.

Note that this authentication only applies when using the relay.
The application is responsible for authentication when using [moq-lite](/rust/moq-lite) directly,


## Overview

The authentication system supports:
- **JWT-based authentication** with query parameter tokens
- **Path-based authorization** with hierarchical permissions
- **Symmetric key cryptography** (HMAC-SHA256/384/512)
- **Asymmetric key cryptography** (RSASSA-PKCS1-SHA256/384/512, RSASSA-PSS-SHA256/384/512, ECDSA-SHA256/384, EdDSA)
- **Anonymous access** for public content
- **Cluster authentication** for relay-to-relay communication

## Usage

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


### Authenticated Tokens
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
- ✅ Connect to `https://cdn.moq.dev/room/123`
- ❌ Connect to: `https://cdn.moq.dev/secret` (wrong root)
- ✅ Publish to `alice/camera`
- ❌ Publish to: `bob/camera` (only alice)
- ✅ Subscribe to `bob/screen`
- ❌ Subscribe to: `../secret` (scope enforced)

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

Our example token from above:
- 🔴 Connect to `http://cdn.moq.dev/room` (must contain room/123)
- 🟢 Connect to `http://cdn.moq.dev/room/123`
- 🟡 Connect to `http://cdn.moq.dev/room/123/alice` (can't subscribe to `bob`)
- 🟡 Connect to `http://cdn.moq.dev/room/123/bob` (can't publish to `alice`)


### Generating Tokens

`moq-token` is available as a Rust crate ([docs.rs](https://docs.rs/moq-token)), JS library ([@moq/token](https://www.npmjs.com/package/@moq/token)), and CLI.
This documentation focuses on the CLI but the same concepts apply to all.

**Installation**:
```bash
# Install the `moq-token` binary
cargo install moq-token-cli
```

**Generate a key**:
```bash
moq-token --key "root.jwk" generate
```

**Sign a token**:
```bash
moq-token --key "root.jwk" sign \
  --root "rooms/meeting-123" \
  --subscribe "" \
  --publish "alice" \
  --expires 1703980800 > "alice.jwt"
```


And of course, the relay has to be configured with the same key to verify tokens.
We currently only support symmetric keys.

**Example Configuration:**
```toml
# config.toml
[auth]
key = "root.jwk" # Path to the key we generated.
```
