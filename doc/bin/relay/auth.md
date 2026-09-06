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
[listen.tls]
root = ["/etc/moq/peer-ca.pem"]

[connect.tls]
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

### Proxy mode

Set `api_mode = "proxy"` in `[auth]` (or `--auth-api-mode proxy`) to let the
endpoint authorize an opaque credential instead of returning a verifying key.
The default mode is `token`.

```http
GET <url>?root=demo&host=live.example.com&transport=quic
Authorization: Bearer <credential>
```

```json
{
  "alias": "x7k2qp",
  "tier": "region/sjc",
  "grant": { "subscribe": ["room"], "publish": ["room/alice"], "exp": 1893456000 }
}
```

The host comes from the URL authority or HTTP/1.1 `Host` header. The credential
arrives in the client's `?jwt=` parameter but need not be a JWT. The endpoint
owns verification and policy; the relay enforces the returned grant. `key` and
`public` fields do not authorize proxy connections. An absent or empty grant
refuses access.

Grant prefixes are relative to `grant.root`, which defaults to the connection
path. The alias may reshape that path: `/` can become `x7k2qp`, or `/room` can
become `x7k2qp/room`, with the granted prefixes following the mapping. Token
mode aliases must preserve path depth. Proxy mode cannot be combined with
`--auth-domain`; either mode requires `--auth-api` when explicitly configured.

`exp` is Unix seconds and bounds the whole session, including in-flight
rechecks. Each successful proxy recheck replaces it, allowing renewal or a
shorter lifetime. Token mode cannot extend a signed JWT's expiry. A response
without a usable positive `max-age` does not enable rechecks, so an omitted
`exp` then leaves the session without an expiry timer.

A `404` or withdrawn grant closes a revalidated session. `401`/`403` also revoke
it when the proxy request carried a credential. For anonymous proxy requests
and token-mode requests, those statuses remain outages subject to
`stale-if-error`, because they cannot identify a rejected viewer credential.

Proxy responses cache per credential, using a SHA-256 cache key even if the
endpoint omits `Vary: Authorization`. Send that header for any intermediate
caches. The proxy client uses private HTTP caching, so plain `max-age` works
with credentials. Token, JWK, and cluster clients retain shared caching and
honor `s-maxage`. In token mode viewers sharing a key share requests; proxy
mode's auth traffic scales with distinct credentials. Anonymous requests
share entries for the same URL.

mTLS peers still use the alias and tier lookup; proxy grants do not restrict
or revalidate them.

## Stream listeners

The plaintext TCP and Unix-socket listeners authenticate exactly like QUIC:
the JWT rides the `SETUP` path (`tcp://127.0.0.1:4444/room?jwt=...`), and
tokenless connections fall back to the public rules. A Unix socket can also
require a specific uid, gid, or pid:

```toml
[listen.unix]
bind = "/run/moq/internal.sock"
allow.uid = [1001]
```

Bind TCP to loopback or a private interface; it carries no peer identity. The
Unix socket is created mode `0666`, so gate it with a restrictive parent
directory or an explicit allowlist.
These are native-only paths for gateways and stats publishers on the same host.
