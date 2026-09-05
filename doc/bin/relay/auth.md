---
title: Authentication
description: JWT, anonymous, mTLS, and API-driven access control for moq-relay
---

# Authentication

Access is decided per connection from the URL path the client dialed. A token
(or an anonymous rule) grants publish and subscribe rights under path
prefixes, and the session can only see that part of the tree.

| Method | When to use it |
| --- | --- |
| **JWT** in `?jwt=` | Normal clients. Path-scoped, expiring, signed by a key the relay can verify. |
| **Anonymous prefixes** | Public rooms, demos, viewer input channels. |
| **mTLS** | Relay-to-relay clustering and trusted services. Full access under the dialed path. |
| **Auth API** | A service that decides everything per connection: key, public access, path alias, billing tier. |

## Tokens

Generate a key, sign a token, hand it to the client. `moq token` inside
[moq-cli](/bin/cli) and the standalone `moq-token` are the same tool.

```bash
# Asymmetric: the relay only needs public.jwk.
moq token generate --algorithm ES256 --out private.jwk --public public.jwk

# Let the bearer publish rooms/123/alice and subscribe to anything in rooms/123.
moq token sign --key private.jwk --root rooms/123 --publish alice --subscribe "" --expires "$(( $(date +%s) + 3600 ))" > alice.jwt

moq token verify --key public.jwk --in alice.jwt
```

```toml
[auth]
key = "public.jwk"          # or key_dir = "/etc/moq/keys/" for {kid}.jwk rotation
```

The client dials `https://relay.example.com/rooms/123?jwt=<token>`. HMAC
(HS256/384/512), RSA (RS/PS), ECDSA (ES256/384), and EdDSA keys all work. A key
can itself be **scoped** at generation (`--root`, `--publish`, `--subscribe`),
after which it can never sign a broader token.

### Claims

| Claim | Meaning |
| --- | --- |
| `root` | Base path. Optional. |
| `put` | Publish suffixes under `root`. `""` means everything; omitted means no publishing. |
| `get` | Subscribe suffixes under `root`. Same rules. |
| `exp`, `iat` | Expiry and issue time. `exp` is enforced for the whole session, not just at connect. |

### Path matching

Grants are `root/suffix`, matched on path boundaries (`foo` covers `foo/bar`
but not `foobar`). The connection path may equal the root, extend it (which
narrows the grant), or be a parent of it (the grant still applies at the
root). An unrelated path is rejected.

| root | put | get | Publish | Subscribe |
| --- | --- | --- | --- | --- |
| `demo` | `my-stream` | `""` | `demo/my-stream` | `demo/*` |
| `demo` | (none) | `""` | nothing | `demo/*` |
| `""` | `""` | `""` | everything | everything |

Libraries: [`moq-token`](/lib/rs/moq-token) (Rust) and [`@moq/token`](/lib/js/token) (TypeScript) sign and verify the same tokens.

## Anonymous access

```toml
[auth]
key = "public.jwk"
public = "anon"             # anyone may publish and subscribe under anon/

# or asymmetric rules:
[auth.public]
subscribe = ["anon", "demo"]
publish = ["anon"]
```

`public = ""` opens everything and is for development only.

## mTLS

Clients presenting a certificate signed by a trusted CA get full access under
the path they dialed. A cluster peer dialing `/` gets everything, which is how
relays authenticate to each other without long-lived JWTs.

```toml
[server.tls]
root = ["/etc/moq/peer-ca.pem"]

[client.tls]
cert = "/etc/moq/relay.pem"    # presented on outbound dials and to the auth API
key = "/etc/moq/relay.key"
```

## Auth API

One HTTP call per connection replaces `key_dir`, `public`, and the rest:

```toml
[auth]
auth_api = "https://api.example.com/auth"
```

The relay issues `GET <url>?root=<path>&kid=<kid>&mtls=true&transport=<quic|websocket|tcp|unix|iroh>`
and expects JSON with optional fields:

| Field | Purpose |
| --- | --- |
| `key` | The verifying JWK for this `kid`. |
| `public` | `{ "subscribe": [...], "publish": [...] }` anonymous prefixes under the root. |
| `alias` | Rewrite the root, so a vanity name and a stable id map to one broadcast tree. |
| `tier` | The label this session's [stats](/bin/relay/config#stats) record under, for billing. |

It fails closed: a network error or non-2xx rejects the connection. If the
response carries `Cache-Control: max-age`, the relay re-asks on that cadence
and closes sessions whose grant is withdrawn, so revoking a key or banning a
tenant takes effect on live sessions rather than only new ones.
`stale-if-error` says how long to keep serving through an outage (default one
hour).

## Stream listeners

The plaintext TCP and Unix-socket listeners authenticate exactly like QUIC:
the JWT rides the `SETUP` path (`tcp://127.0.0.1:4444/room?jwt=...`), and
tokenless connections fall back to the public rules. A Unix socket can also
require a specific uid, gid, or pid:

```toml
[server.unix]
bind = "/run/moq/internal.sock"
allow.uid = [1001]
```

Bind TCP to loopback or a private interface; it carries no peer identity.
These are native-only paths for gateways and stats publishers on the same host.
