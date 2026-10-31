---
title: Authentication
description: "One auth contract for moq-relay: an auth server per session, or a static anonymous grant"
---

# Authentication

A relay admits a session in exactly one of two ways:

| Flag | What admits |
| --- | --- |
| `--auth-url` | An auth server, asked once per session event with everything the relay knows. `moq auth serve` is the reference server; a Worker or a service of your own answers the same contract. |
| `--auth-public` | A static grant for anonymous sessions: patterns rooted at `/`, like a token with an empty root. No server. |

Setting both, or neither, fails at startup; an application that embeds the
relay may leave both unset and decide [in process](#in-process) instead.
Nothing else admits anyone: a verified client certificate is a fact the server
weighs, never a grant on its own. A grant names publish and subscribe rights as
path patterns under a root, and the session can only see that part of the tree.

## The contract

The relay POSTs one JSON body per session event (`connect`, `revalidate`,
`end`) to `--auth-url` and reads a grant back. The request carries everything
the relay knows: a session id, its own `node` name, the transport, addresses,
SNI and ALPN, the path and query exactly as dialed, a moq-transport SETUP
token, the declared role and implementation, and the verified client
certificate. Nothing is parsed on the server's behalf: the `jwt` query parameter is a convention of
`moq auth serve`, not of the relay. The grant answers with `publish` and
`subscribe` pattern unions and optionally a `root` alias, read-only `mounts`,
`expires`, a `revalidate` cadence, a stats `tier` and `label`, and `peer` / `upstream` to
mark another relay and an [upstream link](/bin/relay/cluster#upstream-links).
A `tier` is `/`-joined segments of ASCII letters, digits, `.`, `_`, and `-`, such as `region/sjc`,
because it becomes a metrics label: a grant naming any other tier is an auth server error, and a refusal naming one reads as unattributed.
The fields are documented on `moq_auth::Request` and `moq_auth::Grant`
([Rust](/lib/rs/moq-auth), [TypeScript](/lib/js/auth)).

`server_name` is the SNI that selected the relay's certificate. It is absent
over plaintext, on the `tls://` TCP listener, and for an IP-literal dial, and
never filled from a `Host` header or a WebTransport authority. The client still picks it among the names
the relay serves, so a hostname-scoped server binds the tenant to the
credential, not to `server_name` alone.

**Connect.** A 2xx with a grant admits. A 403 can carry JSON such as
`{"reason":"expired","root":"rooms/123","tier":"edge"}`. The reason is
`refused`, `forbidden`, `expired`, or `malformed` (the token does not parse);
unknown reason strings read as `refused`.
A legacy plain-text 401 or 403 still reads as `refused`. Optional `root` and
`tier` attribute refusal counters, with an absent tier using the default.
Only name a root from a verified credential, including an expired token whose
signature and key scope were verified. Never copy the unverified dialed path
into `root`: that would let callers create unbounded stats entries.
Anything else,
including a timeout or an unparseable body, refuses and logs an error, so
nothing is admitted because the server was down. A grant that names nothing
refuses, and so does one already expired: expiry is exact, with no grace for
clock skew, so keep the auth server's clock in sync. WebSocket admission
happens after SETUP, just like QUIC, so auth never fails the upgrade: rejected
credentials (401 or 403) close the session as `Unauthorized`, or as `Expired`
or `MalformedAuthToken` for those reasons, which stops the client's reconnect
loop, and an unreachable auth server closes it with application code 502.

**Revalidate.** On the grant's cadence the relay asks again. A grant with the
same `root` and `mounts` resizes the live session in place, narrower or wider,
so a moderation decision lands on the session it targets and can be lifted the
same way: what falls outside a narrower grant resets with `Unauthorized`, the
broadcasts the session published outside it abort, and everything else keeps
flowing. A wider grant brings those paths back within the admitted root and
SETUP role. A one-shot HTTP `/fetch` ends on a narrower grant instead. A
changed `root`, `mounts`, `peer`, or `upstream`, or a 401 or 403, ends that
credential's grant, which closes the session unless an
[in-band](#in-band-renewal) credential still holds it. A changed `tier` or
`label` keeps the session and moves its later stats. Any other failure retries with backoff and the session lives
until `expires`, so an outage always has the bound the server chose. A grant
with `revalidate` must set `expires` for that reason. A session that
reaches `expires` closes as `Expired` rather than `Unauthorized`, which also
stops the reconnect loop: refresh the token, then reconnect with the new URL.

**End.** Every close reports `end` with the reason, the duration, and the
bytes sent and received. A shutdown waits for these reports before exiting.

**The link.** `https://` presents the relay's `connect.tls` client certificate
when one is configured, so a remote server can tell which relay is asking;
`unix://` speaks HTTP over a socket; `http://` is accepted for a loopback host
only. HTTPS verifies the URL's hostname with system roots. For a private CA,
set `auth.root` to PEM paths (or repeat `--auth-root`); these replace system
trust for auth only and reload for new connections when rotated. Mesh roots,
fingerprints, insecure mode, and hostname overrides never apply to auth.
There is no shared secret. Redirects are not followed, since each request
carries a credential: point `url` at the final address.

```toml
[auth]
url = "http://127.0.0.1:4440/"
```

### Push

An operator or the auth server can ask a node to re-check sessions now instead
of waiting for the cadence, so a kick, a gate, or a retier lands in one round
trip. A push decides nothing: the relay asks the server again and applies its
answer exactly as a scheduled re-check would. The internal listener selects
sessions by the fields the server already saw; see
[`/sessions`](/bin/relay/http#get-sessions).

```bash
# Kick one session.
curl -X POST 'http://127.0.0.1:9101/sessions/revalidate?id=00ff'

# Re-check everyone under a path.
curl -X POST 'http://127.0.0.1:9101/sessions/revalidate?path=demo/**'

moq auth revalidate --internal-url http://127.0.0.1:9101 --id 00ff
```

A push reaches one node. The server already knows each session's `node` from
`connect` and calls that relay.

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
moq auth serve --key public.jwk   # or --key-dir /etc/moq/keys/ or --key-set /etc/moq/keys.jwks
```

A native client passes it as `--connect-token <token>` (or `MOQ_CONNECT_TOKEN`)
and dials `https://relay.example.com/rooms/123`. The first token is the
connection credential, sent in SETUP: moq-lite-07-wip's Token parameter, or
moq-transport's `AUTHORIZATION TOKEN` option with Token Type 0. While the client
also offers a version that cannot carry it (moq-lite before 07), it sends the
same value as `?jwt=`, which `moq auth serve` admits since they match; two
different values are refused. A URL token can land in server and proxy access
logs, so to keep it out, offer only versions that carry it in SETUP, such as
`--connect-version moq-lite-07-wip`; a relay that predates SETUP tokens then
cannot receive it. Each further token is presented
[in band](#in-band-renewal) and widens the session's grant. A URL that already
carries a `?jwt=`, such as a cluster peer's, keeps it as the connection
credential, and every configured token then rides in band. Dialing
`?jwt=<token>` directly still works, and reaches relays that predate SETUP
tokens; a query carrying two `jwt` parameters is refused. HMAC, RSA, ECDSA, and EdDSA keys all work. A private RSA key must be a complete two-prime JWK (see [moq-auth](/lib/rs/moq-auth) for its fields); export one from your provider, or generate one with `moq auth generate --algorithm RS256`. A
key can itself be **scoped** at generation (`--root`, `--publish`,
`--subscribe`), after which it can never sign a broader token.

### In-band renewal

On moq-lite-07-wip or moq-transport draft-18 and later with MoQ Auth negotiated,
a client can present another JWT without reconnecting. Present its replacement
before the current grant expires and keep the returned token handle alive.
The auth handle a reconnecting connection owns (native `Connection::auth()`,
JavaScript `connection.auth`) presents its tokens again on every reconnect.
A native client can instead replace its configured tokens through its
connection's tokens (`Connection::tokens()`, `session.tokens()` in the
bindings): the next dial presents them, a live session presents them in band and
withdraws each old one once its replacement is answered, `answered()` reports
the relay's verdict, and a refusal redials only once the tokens have changed.
Older versions refuse in-band tokens.

Each accepted token adds its publish and subscribe patterns to the session's
union, within its admitted root and SETUP role. A publish-only connection cannot
become a subscriber. A replacement must retain the root, mounts, and routing
classification. Dropping a token handle, expiry, or refusal at revalidation
removes that token's grant: streams that lose permission reset, and other grants
keep the connection alive. A refused new token leaves existing grants alone.
The grant reports its real expiry so the client can renew in time.

The relay verifies a presented JWT through the same admission path, replacing
`jwt` in the original query and preserving the connection's identity. The auth
server sees another `connect` for the same session id. Attached grants revalidate
independently but never report `end`; only the connection reports its end, even
if its first credential expired earlier, after its attached grants stop. Each
attached JWT counts against its own `--limit-token` cap until the connection
ends. The initial credential remains responsible for the connection's stats tier.
Certificate sessions cannot renew a certificate in band, and public rules
cannot verify a presented token; both refuse it as unsupported. A moq-transport
`AUTHORIZATION TOKEN` on a `SUBSCRIBE` or `PUBLISH_NAMESPACE` is verified the
same way on any draft, but its grant authorizes only that request and ends with
it; it never joins the union. A `PUBLISH_NAMESPACE` grant must cover the whole
namespace: `room/**`, not just `room`. A session verifies at most
128 tokens at once, counting
checks still running past the admission timeout. Later tokens wait their turn
rather than being refused. Each HTTP request is admitted on its own, so an
HTTP/2 connection carries at most as many requests at once, and the client
queues the rest. A waiting token the peer withdraws never reaches the
auth server. The ten-second admission timeout counts from a token's arrival, so
one that times out while waiting is refused without being sent. A sent check
always runs to completion, so the connection's `end` stays its last request and
`--limit-token` counts stay exact. On moq-lite, an
auth server that cannot answer, by timeout or outage, refuses a token as a
transient error, and the client presents it again after a backoff of 1s
doubling to 30s. On moq-transport the refusal is final and the token ends. Retrying
never extends an accepted grant, so a session whose last grant expires during an
outage still closes.

### Claims

| Claim | Meaning |
| --- | --- |
| `root` | Base path. Optional. |
| `publish` | Patterns the bearer may publish under `root`. `**` means everything; omitted means no publishing. |
| `subscribe` | Patterns the bearer may subscribe to under `root`. Same rules. |
| `exp`, `iat`, `nbf` | Expiry, issue time, and not-before. `exp` ends that credential's grant, including after connect; another accepted token can keep the session alive. A token is refused from its `exp` on and before its `nbf`. |
| `iss`, `sub`, `jti` | Read and ignored. |

Any other claim refuses the token with its name, `aud` included: an unknown
claim may narrow the grant, and a misspelled `root` would otherwise widen it to
everything. Put application data somewhere other than the token.

Tokens and key scopes from the older `moq-token` format still work: each `put`
and `get` prefix `p` reads as the subtree `p/**`.

Roles are pattern arrays. Object roles, including bitrate caps such as
`{"path":["alice/**"],"rate":8000000}`, are refused in full by token and
auth-server grant parsing. No transport currently enforces bitrate caps.

### Path matching

Grants are [patterns](https://docs.rs/moq-pattern) relative to `root`: `foo`
is exactly `foo`, `foo/**` is `foo` and everything beneath it, `live/*` is
every broadcast one segment under `live`, and `**` is everything. Matching is
on path boundaries (`foo/**` covers `foo/bar` but not `foobar`), and the relay
announces, publishes, and resolves only paths inside the grant. `moq auth
serve` authorizes the token at the dialed path: the path may equal the root,
extend it (which narrows the grant), or be a parent of it (the grant still
applies at the root). An unrelated path is rejected.

The session's root is always the path it dialed, and what it sees is named
relative to that. For a grant of `demo/**` from the root:

| Dialed | Session root | Granted |
| --- | --- | --- |
| `/demo/bar` | `demo/bar` | `**` |
| `/demo` | `demo` | `**` |
| `/` | \`\` | `demo/**` |
| `/other` | refused | |

Anonymous and mTLS rules, on the relay and in `moq auth serve`, are authorized
the same way, as a token with an empty root.

| root | publish | subscribe | Publish | Subscribe |
| --- | --- | --- | --- | --- |
| `demo` | `my-stream/**` | `**` | `demo/my-stream/**` | `demo/**` |
| `demo` | (none) | `**` | nothing | `demo/**` |
| `""` | `**` | `**` | everything | everything |

Libraries: [`moq-auth`](/lib/rs/moq-auth) (Rust) and [`@moq/auth`](/lib/js/auth) (TypeScript) sign and verify the same tokens.

### Peer grants

A peer grant is the relay's short-lived statement of one session's publish and
subscribe patterns, bound to a public key the client holds (`cnf`). Another
client verifies it with the relay's public keys and serves the presenter only
those paths on a direct connection. It is not a token: presented as `?jwt=`
it is refused, and a token is never a peer grant.

Peer grants need an asymmetric key, ES256 or EdDSA. A relay configured with
only HMAC keys (HS256 and friends) issues none, since giving a client an HMAC
key would let it forge grants, and its sessions serve nothing on a direct
connection. The set of public keys that verify peer grants is not a secret: it
holds only ES256 and EdDSA public keys, never HMAC material.

[`moq-auth`](/lib/rs/moq-auth) signs and verifies peer grants
(`KeySet::sign_peer`, `KeySet::verify_peer`, `KeySet::to_peer_set`). The relay
does not issue them on the AUTH stream yet.

## Anonymous access

```toml
[auth]
public = "anon/**"                    # anyone may publish and subscribe under anon/

# or asymmetric rules:
public_subscribe = ["anon/**", "demo/**"]
public_publish = ["anon/**"]
```

A static grant with no expiry and no re-check. Nothing here verifies a token,
so a session presenting one is refused rather than admitted on the public
grant. The patterns are rooted at `/`, like a token with an empty root (see
[Path matching](#path-matching)): `anon/**` admits a session dialed at `/`,
`/anon`, or `/anon/room`, and refuses one dialed anywhere else. A pattern with
no wildcard, such as `anon`, refuses to start; write `anon/**` for the subtree.
`public = "**"` opens everything and is for development only. With
`--auth-url` the anonymous rules live on the server instead
(`moq auth serve --public-*`).

## mTLS

`listen.tls.root` verifies a client certificate chain at the handshake, and a
bad chain fails there. What the certificate admits is the server's decision:
the relay reports it in the request and enforces the grant it gets back.
`moq auth serve` grants a certificate only what `--mtls-publish` and
`--mtls-subscribe` name, nothing by default. Public rules ignore certificates,
so `--auth-public` with a client CA refuses to start. Only the QUIC listener
verifies client certificates, so a relay without `listen.bind` refuses
`listen.tls.root` too.

Cluster peers are admitted the same way, so a mesh runs
`moq auth serve --mtls-publish '**' --mtls-subscribe '**' --mtls-link peer` (or a
server granting its cluster CA everything with `peer: true`); see
[Clustering](/bin/relay/cluster). `--mtls-link peer` marks every certificate as
another relay; leave it off when certificates identify clients. Without
`peer`, a certificate is a client's: it routes like one and sees
[hidden](/concept/moq-lite#hidden-broadcasts) routes only by asking.
`--mtls-link upstream` also marks them
[upstream](/bin/relay/cluster#upstream-links). Changing the link role ends each
live mTLS session at its next re-check (the `--revalidate` cadence or a
`moq auth revalidate` push), so the mesh redials once.

```toml
[listen.tls]
root = ["/etc/moq/peer-ca.pem"]

[connect.tls]
cert = "/etc/moq/relay.pem"    # presented on outbound dials and to an https:// auth server
key = "/etc/moq/relay.key"
```

## Auth server

Upgrade auth servers before relays when adopting the `webtransport` transport value.
WebTransport sessions report `webtransport`; native QUIC sessions report `quic`.
Older auth servers reject the new value. Updated Rust and JavaScript auth parsers
map future transport names to `unknown`, which your policy should handle explicitly.

`moq auth serve` is the reference auth server, answering the contract above.

```bash
moq auth serve --listen 127.0.0.1:4440 \
  --key-dir /etc/moq/keys \
  --public-subscribe 'anon/**' --public-publish 'anon/**' \
  --mtls-publish '**' --mtls-subscribe '**' --mtls-link peer \
  --tier edge --expires 1d --revalidate 1m --limit-remote 64
```

What a session presents decides what is checked. Every credential is either
evaluated or refused: nothing is ignored, and no two are combined.

| Presented | `--auth-public` | `--auth-url` to `moq auth serve` |
| --- | --- | --- |
| nothing | the public rules | `--public-*`, or refused |
| a JWT | refused | verified, or refused |
| a certificate | never requested: a client CA refuses to start | `--mtls-*`, or refused |
| a JWT and a certificate | never requested | refused |

A JWT is verified against `--key FILE`, `--key-dir DIR`, or `--key-set FILE`
(by `kid`, read per request so rotation needs no restart) and authorized at the
dialed path, as in [Path matching](#path-matching). A bad token is refused; it
never falls through to the anonymous rules. A JWT presented with a certificate
is refused too, because neither can safely win, so a peer presents
`cluster.token` or a certificate, not both.

Nothing is re-checked or closed by default: a session lives until its token's
`exp` or its certificate's notAfter. `--expires D` bounds a grant with no bound
of its own. `--revalidate D` has the relay re-check each grant on that cadence,
and needs `--expires` so an outage still has a bound. Without `--revalidate`,
rotating or deleting a key does not close live sessions.

`--limit-token N` and `--limit-remote N` cap live sessions per token and per
remote address. They need `--revalidate`, and are a nuisance limit, not a
security boundary: approximate across relay crashes and server restarts, and
they gate admission without revoking.

`--listen unix:/run/moq-auth.sock` serves a socket. Binding anything but a
loopback address needs `--listen-public`: the server has no authentication of
its own. See `moq auth serve --help` for the rest.

### Migrating from the relay flags

The 0.14 relay's auth flags moved to `moq auth serve`, and the relay points
`--auth-url` at it. A relay without a server keeps `--auth-public`, as
patterns: `--auth-public 'PREFIX/**'`.

| 0.14 relay flag | `moq auth serve` |
| --- | --- |
| `--auth-key FILE` | `--key FILE` |
| `--auth-key-dir DIR` | `--key-dir DIR` |
| `--auth-public PREFIX` | `--public-publish 'PREFIX/**' --public-subscribe 'PREFIX/**'` |
| `--auth-public-publish` / `--auth-public-subscribe` | `--public-publish` / `--public-subscribe`, as patterns |
| `--auth-public-api URL` | your own server answering the contract |
| `--auth-mtls-tier LABEL` | `--tier LABEL` (one tier per server) |
| `listen.tls.root` alone admitting a peer unscoped | `--mtls-publish '**' --mtls-subscribe '**' --mtls-link peer` |
| `--auth-api` (token or proxy mode), `Cache-Control` | `--auth-url` pointed at any server answering the contract; `revalidate` and `expires` in the grant |
| `--auth-domain` | your server reads `server_name` and decides |

Both on one host:

```bash
moq auth serve --listen 127.0.0.1:4440 --key-dir /etc/moq/keys --public-subscribe 'anon/**'
moq-relay --auth-url http://127.0.0.1:4440/
```

### In process

An application that [embeds](/bin/relay/#embed) the relay can decide
admissions itself, without the HTTP: leave `[auth]` empty and take
`relay.admissions()` before `run`. Each `Admission` carries the same
`moq_auth::Request` a server would read, and is answered with a lease or a
refusal. Pass the shared `moq_auth::Refusal` in `auth::Error::Refused` to
`Admission::refuse`, using the same verified-root rule as an HTTP server.
A fixed lease never changes; a `lease::Producer` the application keeps
re-checks, updates, and revokes on its own clock. The relay still closes the
session at the grant's `expires`, and a connection whose admission is still
unanswered at its `listen.timeout` handshake deadline is closed; a token
presented in band is refused after ten seconds. `Admission::attached()` marks a token presented
[in band](#in-band-renewal) for a session already admitted under the same id,
so count sessions only on the others. See
[docs.rs/moq-relay](https://docs.rs/moq-relay) and
[moq-auth](/lib/rs/moq-auth).

## Stream listeners

The plaintext TCP and Unix-socket listeners are admitted exactly like QUIC:
the path and query ride the `SETUP` (`tcp://127.0.0.1:4444/room?jwt=...`) and
reach the auth server with `transport` set to `tcp` or `unix`. A Unix socket
can also require a specific uid, gid, or pid:

```toml
[listen.unix]
bind = "/run/moq/internal.sock"
allow.uid = [1001]
```

Bind plaintext TCP to loopback or a private interface; it carries no peer
identity. The Unix socket is created mode `0666`, so gate it with a restrictive
parent directory or an allowlist.

`listen.tcp.tls = true` serves the TCP listener over TLS instead, for `tls://`
dials such as [cluster links](/bin/relay/cluster#tls-links). It asks for no
client certificate, so a peer on it presents a token, not mTLS.
