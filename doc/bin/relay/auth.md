---
title: Authentication
description: "One auth contract for moq-relay: an auth server per session, or a static anonymous grant"
---

# Authentication

A relay admits a session in exactly one of two ways:

| Flag | What admits |
| --- | --- |
| `--auth-url` | An auth server, asked once per session event with everything the relay knows. `moq auth serve` is the reference server; a Worker or a service of your own answers the same contract. |
| `--auth-public` | A static grant for anonymous sessions: patterns under the dialed path, no server. |

Setting both, or neither, fails at startup; an application that embeds the
relay may leave both unset and decide [in process](#in-process) instead.
Nothing else admits anyone: a verified client certificate is a fact the server
weighs, never a grant on its own. A grant names publish and subscribe rights as path patterns under a root,
and the session can only see that part of the tree.

## The contract

The relay POSTs one JSON body per session event to `--auth-url` and reads a
grant back. The schemas are `moq_auth::Request` and `moq_auth::Grant`
([Rust](/lib/rs/moq-auth), [TypeScript](/lib/js/auth)).

**Request.** `id` (random 128-bit hex, unique per session), `event`
(`connect`, `revalidate`, or `end`), `node` (the relay's `--stats-node`, else
`--cluster-node`), `transport` (`quic`, `websocket`, `tcp`, `unix`, `iroh`, or
`http` for a one-shot `/fetch` or `/announced` request), `remote` and `local`
socket addresses, `server_name` (the SNI or the host the client addressed),
`alpn` (the negotiated moq protocol), `path` exactly as dialed, `query` raw,
`role` the client declared at SETUP (`publisher`, `subscriber`, absent for
both), and `tls` with the verified client certificate when one was presented:
`name` (first SAN DNS name, else CN, else the fingerprint), `fingerprint`
(SHA-256 of the leaf), `expires`, `issuer`. Nothing is parsed on the server's
behalf: the `jwt` query parameter is a convention of `moq auth serve`, not of
the relay. An `end` adds `reason`, `duration` in seconds, and `bytes` sent and
received.

**Grant.** `publish` and `subscribe` as pattern unions (`foo/**` is a subtree,
`**` is everything, an empty list is nothing), `root` (optional; replaces the
dialed path, which is how a slug aliases to a canonical id), `expires`
(optional unix seconds; the session closes then), `revalidate` (optional
seconds until the relay asks again), and `tier` (optional label handed to
[stats](/bin/relay/config#stats)). A 2xx with a grant admits. A 403 refuses.
Anything else at connect, a timeout, a 5xx, or an unparseable body, refuses and
logs an error; nothing is admitted because the server was down. A grant that
names nothing refuses, and one with `revalidate` but no `expires` is refused
as invalid.

**Revalidate and outage.** On the cadence the relay POSTs `revalidate` with the
same request. A grant applies: a changed `root` or one that no longer covers
what the session holds closes it with `Unauthorized` (the origin cannot be
resized in place until pattern scopes land); a changed `tier` is logged and
applies to the session's next connection, since its stats counters were
resolved at admission. A 403 closes the session now. Anything else retries
with jittered backoff and the session lives until `expires`, so an outage always
has the bound the server chose. There is no `Cache-Control` and no cache on the
relay.

**End.** Every close reports `end` with the reason: `expired`, `refused`, the
session's own close classification, or `dropped`. The byte totals are what the
transport reports; QUIC reports them, the qmux stream transports do not yet.

**The link.** `https://` presents the relay's `connect.tls` client certificate
when one is configured, so a remote server can tell which relay is asking;
`unix://` speaks HTTP over a socket; `http://` is accepted for a loopback host
only. There is no shared secret.

**Patterns today.** The relay scopes a session by prefix until pattern scopes
land, so only `foo/**` and `**` admit: a grant naming `live/*`, or a bare
literal `foo`, is refused at connect naming the pattern.

```toml
[auth]
url = "http://127.0.0.1:4440/"
```

## Tokens

With `moq auth serve`, a client presents a JWT in `?jwt=`. Generate a key,
sign a token, hand it to the client. Install the [moq CLI](/setup/install)
and use its `moq auth` subcommand.

```bash
# Asymmetric: the relay only needs public.jwk.
moq auth generate --algorithm ES256 --out private.jwk --public public.jwk

# Let the bearer publish under rooms/123/alice and subscribe to anything in rooms/123.
moq auth sign --key private.jwk --root rooms/123 --publish 'alice/**' --subscribe '**' --expires "$(( $(date +%s) + 3600 ))" > alice.jwt

moq auth verify --key public.jwk --in alice.jwt
```

```bash
moq auth serve --key public.jwk   # or --key-dir /etc/moq/keys/ for {kid}.jwk rotation
```

The client dials `https://relay.example.com/rooms/123?jwt=<token>`. HMAC
(HS256/384/512), RSA (RS/PS), ECDSA (ES256/384), and EdDSA keys all work. A key
can itself be **scoped** at generation (`--root`, `--publish`, `--subscribe`),
after which it can never sign a broader token.

### Claims

| Claim | Meaning |
| --- | --- |
| `root` | Base path. Optional. |
| `publish` | Patterns the bearer may publish under `root`. `**` means everything; omitted means no publishing. |
| `subscribe` | Patterns the bearer may subscribe to under `root`. Same rules. |
| `exp`, `iat` | Expiry and issue time. `exp` is enforced for the whole session, not just at connect. |

A token carrying the retired `put` and `get` prefix lists fails verification.

### Path matching

Grants are [patterns](https://docs.rs/moq-pattern) relative to `root`: `foo`
is exactly `foo`, `foo/**` is `foo` and everything beneath it, and `**` is
everything. Matching is on path boundaries (`foo/**` covers `foo/bar` but not
`foobar`). The relay scopes a session by prefix until origin grants become a
pattern set, so only `foo/**` and `**` admit today: a token naming `live/*`,
or a bare literal, is refused at connect naming the pattern. The connection
path may equal the root, extend it (which narrows the grant), or be a parent
of it (the grant still applies at the root). An unrelated path is rejected.

| root | publish | subscribe | Publish | Subscribe |
| --- | --- | --- | --- | --- |
| `demo` | `my-stream/**` | `**` | `demo/my-stream/**` | `demo/**` |
| `demo` | (none) | `**` | nothing | `demo/**` |
| `""` | `**` | `**` | everything | everything |

Libraries: [`moq-auth`](/lib/rs/moq-auth) (Rust) and [`@moq/auth`](/lib/js/auth) (TypeScript) sign and verify the same tokens.

## Anonymous access

```toml
[auth]
public = "anon/**"                    # anyone may publish and subscribe under anon/

# or asymmetric rules:
public_subscribe = ["anon/**", "demo/**"]
public_publish = ["anon/**"]
```

A static grant with no expiry and no re-check. `public = "**"` opens everything
and is for development only. With `--auth-url` the anonymous rules live on the
server instead (`moq auth serve --public-*`).

## mTLS

`listen.tls.root` verifies a client certificate chain at the handshake, and a
bad chain still fails there. What the certificate admits is the server's
decision: the relay reports its facts in the request's `tls` and enforces the
grant it gets back. `moq auth serve` grants a certificate only what
`--mtls-publish` and `--mtls-subscribe` name, empty by default. A relay on
`--auth-public` grants a certificate what it grants everyone.

Cluster peers are admitted the same way, so a mesh runs
`moq auth serve --mtls-publish '**' --mtls-subscribe '**'` (or a server
granting its cluster CA everything); see [Clustering](/bin/relay/cluster).
The LAN mesh credential on `/.cluster/<credential>` stays relay-internal: it
is a secret the relay minted for itself, checked locally, and never a request
to the server.

```toml
[listen.tls]
root = ["/etc/moq/peer-ca.pem"]

[connect.tls]
cert = "/etc/moq/relay.pem"    # presented on outbound dials and to an https:// auth server
key = "/etc/moq/relay.key"
```

## Auth server

`moq auth serve` is the reference auth server: the policy the relay used to
hold, answered over the contract above.

```bash
moq auth serve --listen 127.0.0.1:4440 \
  --key-dir /etc/moq/keys \
  --public-subscribe 'anon/**' --public-publish 'anon/**' \
  --mtls-publish '**' --mtls-subscribe '**' \
  --tier edge --revalidate 1m --limit-remote 64
```

Policy runs in this order and stops at the first that applies:

1. A `jwt` in the query is verified against `--key FILE` or `--key-dir DIR`
   (by `kid`, read per request so rotation needs no restart). Its
   claims are the grant and its `root` must equal the dialed path. A
   malformed, expired, or unknown-key token is refused; it never falls
   through to the anonymous rules.
2. A verified client certificate gets `--mtls-publish` and
   `--mtls-subscribe`, and nothing when they are empty. Cluster peers are
   admitted this way; a mesh needs `'**'` for both.
3. Anything else gets `--public-publish` and `--public-subscribe`, and is
   refused when they are empty.

Every grant carries `--tier`, a `revalidate` cadence (`--revalidate`, default
one minute), and an `expires`: the token's `exp`, the certificate's notAfter,
or `--expires` (default one day) when neither has one.

`--limit-token N` and `--limit-remote N` cap live sessions per token and
per remote address (port dropped, IPv4-mapped IPv6 folded), counted from
`connect` and `end` by session id. The cap is a nuisance limit, not a security
boundary: it gates admission and never revokes. A relay that dies without an
`end` holds its slots until they miss two cadences, a restart empties the table
until the fleet's next cadence refills it, and a session admitted while a live
one's slot was missing stays over the cap. A refusal names the rule in the 403
body.

`--listen unix:/run/moq-auth.sock` serves a socket. Binding anything but a
loopback address needs `--listen-public`: the server has no authentication of
its own.

### Migrating from the relay flags

The relay flags below were deleted; the policy moves to the server and the
relay gets `--auth-url`.

| Removed relay flag | `moq auth serve` |
| --- | --- |
| `--auth-key FILE` | `--key FILE` |
| `--auth-key-dir DIR` | `--key-dir DIR` |
| `--auth-public PREFIX` | `--public-publish 'PREFIX/**' --public-subscribe 'PREFIX/**'` |
| `--auth-public-publish` / `--auth-public-subscribe` | `--public-publish` / `--public-subscribe`, as patterns |
| `--auth-public-api URL` | your own server answering the contract |
| `--auth-mtls-tier LABEL` | `--tier LABEL` (one tier per server) |
| `listen.tls.root` alone admitting a peer unscoped | `--mtls-publish '**' --mtls-subscribe '**'` |
| `--auth-api` (token or proxy mode), `Cache-Control` | `--auth-url` pointed at any server answering the contract; `revalidate` and `expires` in the grant |
| `--auth-domain` | your server reads `server_name` and decides |

Both on one host:

```bash
moq auth serve --listen 127.0.0.1:4440 --key-dir /etc/moq/keys --public-subscribe 'anon/**'
moq-relay --auth-url http://127.0.0.1:4440/
```

### In process

An application that [embeds](/bin/relay/#embed) the relay can be the auth
server without the HTTP: leave `[auth]` empty and take `relay.admissions()`
before `run`. Each `Admission` carries the same `moq_auth::Request` the server
would have read, and is answered with `grant(lease)` or `refuse(err)`. A
`lease::Consumer::fixed(grant)` never changes; the consumer of a
`lease::Producer` the application keeps is driven by it, which re-checks,
updates, revokes, and learns when the session ends. Either way the relay closes the session at the
grant's `expires`. `run` refuses to start while nobody has taken the
admissions, a dropped `Admissions` fails every later session as unavailable,
and an admission left unanswered for ten seconds (the bound an auth server
gets) is refused the same way.

```rust
let mut relay = Relay::load(config).await?;
let mut admissions = relay.admissions().expect("[auth] is empty");
tokio::spawn(async move {
    while let Some(admission) = admissions.next().await {
        match policy.decide(&admission.request) {
            Ok(grant) => {
                let (producer, consumer) = moq_auth::lease::Producer::new(grant);
                admission.grant(consumer);
                tokio::spawn(revalidate(producer)); // update, revoke, await closed()
            }
            Err(err) => admission.refuse(err),
        }
    }
});
relay.run().await
```

## Stream listeners

The plaintext TCP and Unix-socket listeners are admitted exactly like QUIC:
the path and query ride the `SETUP` (`tcp://127.0.0.1:4444/room?jwt=...`) and
reach the auth server in the same request shape, with `transport` set to `tcp`
or `unix` and no addresses on a socket. A Unix socket can also
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
