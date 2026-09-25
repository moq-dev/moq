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
`name` (first SAN DNS name, else CN, else the fingerprint; a server cannot tell which), `fingerprint`
(SHA-256 of the leaf), `expires`, `issuer`. Nothing is parsed on the server's
behalf: the `jwt` query parameter is a convention of `moq auth serve`, not of
the relay. An `end` adds `reason`, `duration` in seconds, and `bytes` sent and
received.

**Grant.** `publish` and `subscribe` as pattern unions (`foo/**` is a subtree,
`**` is everything, an empty list is nothing), `root` (optional; replaces the
dialed path, which is how a slug aliases to a canonical id), `expires`
(optional unix seconds; the session closes then), `revalidate` (optional
seconds until the relay asks again), `tier` (optional label handed to
[stats](/bin/relay/config#stats)), and `peer` (optional; `true` marks another
relay, so what it announces counts as entering the cluster elsewhere, not as
ingest here). A 2xx with a grant admits. A 401 or 403 refuses.
Anything else at connect, a timeout, a 5xx, or an unparseable body, refuses and
logs an error; nothing is admitted because the server was down. A grant that
names nothing refuses, and one with `revalidate` but no `expires` is refused
as invalid. A few seconds of clock skew are tolerated on `expires`.

**Revalidate and outage.** On the cadence the relay POSTs `revalidate` with the
same request. A grant applies: a changed `root` or one that no longer covers
what the session holds closes it with `Unauthorized` (the live session is not
resized in place), and so does a flipped `peer`; a changed `tier` keeps the
session and moves its stats: its presence counts under the new tier from then
on, as does each group and subscription it starts afterwards, while one already
in flight finishes where it began. A 401 or 403 closes the session now, as does a 2xx
whose grant names nothing. A 2xx that fails validation otherwise (already
expired, `revalidate` without `expires`, or a zero cadence) closes it as
`invalid`. Anything else (408, 429, 404, 400, 5xx, a timeout, a transport
error) retries with jittered backoff and the session lives until `expires`, so
an outage always has the bound the server chose. There is no `Cache-Control`
and no cache on the relay.

**End.** Every close reports `end` with the reason: `expired`, `refused`,
`invalid`, the session's own close classification, or `dropped`. The byte
totals are what the transport reports; QUIC reports them, the qmux stream
transports do not yet.

**Push.** An operator or the auth server can ask this node to re-check now,
instead of waiting for the cadence, so a kick, a gate, or a retier lands in
one round trip. The internal listener takes the same filter on
`GET /sessions` and `POST /sessions/revalidate`: any subset of the fields the
server already saw (`id`, `path` as a pattern, `remote` as an IP or CIDR,
`transport`, `tls.name`, ...). Every given field must match. An empty filter
is every session on this node, so a node under load is nudged through a
selector. A push does not decide anything: the relay re-POSTs `revalidate`
and the server's reply kicks, narrows, retiers, or keeps the session exactly
as a scheduled re-check would. Closing a session with no server in the loop
is not a route.

```bash
# Kick one session.
curl -X POST 'http://127.0.0.1:9101/sessions/revalidate?id=00ff'

# Re-check everyone under a path, then see who is still there.
curl 'http://127.0.0.1:9101/sessions?path=demo/**'
curl -X POST 'http://127.0.0.1:9101/sessions/revalidate?path=demo/**'

moq auth revalidate --internal-url http://127.0.0.1:9101 --id 00ff
moq auth sessions --internal-url http://127.0.0.1:9101 --path 'demo/**'
```

`GET /sessions` is the dry run: the same matches, each request plus start
time, with `query` omitted so a jwt on the plane cannot be replayed. A
matching POST returns 202 and the ids; no match is 200 with an empty list.
An unknown field, including `query`, is 400. One node, no cluster fan-out:
the server already knows each session's `node` from `connect` and calls that
relay.

**The link.** `https://` presents the relay's `connect.tls` client certificate
when one is configured, so a remote server can tell which relay is asking;
`unix://` speaks HTTP over a socket; `http://` is accepted for a loopback host
only. There is no shared secret.

**Patterns.** A grant is any pattern union and means exactly what it says:
`foo/**` is a subtree, a bare `foo` is that one broadcast, `live/*` is every
broadcast one segment under `live`, and `room/*/chat` is each room's chat and
nothing beside it. The relay announces, publishes, and resolves only the paths
inside the grant. Announcements travel by literal head on the wire, so a
non-prefix grant is filtered locally on each side while the announced route
stays a prefix on every protocol version.

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

Tokens and key scopes from the older `moq-token` format still work: each `put`
and `get` prefix `p` reads as the subtree `p/**`, and `""` as `**`. When every
grant is a subtree, signing writes that older form so relays and auth servers
that predate patterns accept it too. A grant only a pattern can express (an
exact `foo`, or `*/chat`) is written as `publish`/`subscribe`, which an older
verifier refuses.

### Path matching

Grants are [patterns](https://docs.rs/moq-pattern) relative to `root`: `foo`
is exactly `foo`, `foo/**` is `foo` and everything beneath it, and `**` is
everything. Matching is on path boundaries (`foo/**` covers `foo/bar` but not
`foobar`). The relay scopes a session by the exact union, including literals,
segment wildcards, suffixes, and subtrees. `moq auth serve` authorizes the token
at the dialed path: the path may equal the root, extend it (which narrows the
grant), or be a parent of it (the grant still applies at the root). An unrelated
path is rejected. The relay forwards the raw path and enforces the grant it gets.

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
   (by `kid`, read per request so rotation needs no restart). Its claims
   are authorized at the dialed path (`Claims::authorize`): the path may
   equal the root, extend it (which narrows the grant), or be a parent of
   it (the grant stays anchored at the root). Residuals become the grant.
   An unrelated path, or one the token grants nothing at, is refused. A
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
                tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            reason = producer.closed() => break reason,
                            () = producer.revalidate_requested() => {
                                // Re-run the decision now: update, revoke, or keep.
                            }
                        }
                    }
                });
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
