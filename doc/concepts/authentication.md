---
title: Authentication
description: Authentication concepts for MoQ
---

# Authentication

[moq-relay](/relay/) uses JWT tokens in the URL for authentication and authorization.
This scopes sessions to a selected root path with additional rules for publishing and subscribing.

Note that this authentication only applies when using the relay.
The application is responsible for authentication when using [moq-lite](/rust/lite) directly.


## Overview

The authentication system supports:
- **JWT-based authentication** with query parameter tokens
- **Path-based authorization** with hierarchical permissions
- **Symmetric key cryptography** (HMAC-SHA256/384/512)
- **Asymmetric key cryptography** (RSASSA-PKCS1-SHA256/384/512, RSASSA-PSS-SHA256/384/512, ECDSA-SHA256/384, EdDSA)
- **Anonymous access** for public content
- **Cluster authentication** for relay-to-relay communication

## How It Works

### Token-Based Access

Clients pass a JWT token via the `?jwt=` query parameter:

```
https://cdn.moq.dev/room/123?jwt=<base64-jwt-token>
```

The token contains claims that define:
- **Root path** - The base path for all operations
- **Publish permissions** - What paths can be published to
- **Subscribe permissions** - What paths can be subscribed to
- **Expiration** - When the token becomes invalid

### Path Scoping

All operations are relative to the connection URL path:

```
Connection URL: https://cdn.moq.dev/room/123
Token root: "room/123"
```

The client can then:
- `SUBSCRIBE alice` → subscribes to `room/123/alice`
- `ANNOUNCE bob/video` → announces `room/123/bob/video`

### Token Claims

```json
{
  "root": "room/123",     // Base path
  "pub": "alice",         // Can publish under alice/**
  "sub": "",              // Can subscribe to anything
  "cluster": false,       // Not a cluster node
  "exp": 1703980800,      // Expiration timestamp
  "iat": 1703977200       // Issued at timestamp
}
```

## Anonymous Access

For development or public content, the relay can allow anonymous access:

```toml
[auth]
public = "anon"  # Allow unauthenticated access to anon/**
```

This is useful for:
- Development and testing
- Public broadcasts
- Demo applications

::: warning
Anonymous paths can be hijacked by anyone. Don't use for sensitive content.
:::

## Security Considerations

### Token Delivery

- Always transmit tokens over HTTPS
- Avoid logging the `jwt` query parameter
- Use short expiration times
- Consider using asymmetric keys

### Key Management

- **Symmetric keys**: Simpler but requires secure distribution
- **Asymmetric keys**: More secure, relay only needs public key
- **JWK sets**: Enable key rotation without relay restart

### Path Design

- Use unique, unpredictable paths for private content
- Scope tokens to minimum necessary permissions
- Separate publish and subscribe tokens when possible

## Implementation

For practical setup, see:
- [Relay Authentication](/relay/auth) - Configuration guide
- [moq-token library](/rust/token) - Rust implementation
- [@moq/token library](/ts/token) - TypeScript implementation

## Next Steps

- Configure [Relay Authentication](/relay/auth)
- Understand [Protocol concepts](/concepts/protocol)
- Deploy a [Relay server](/relay/)
