# Migrating moq-relay

How to update your code, invocation, and config for each breaking change, newest first.
Rolling a cluster across a release also follows the [upgrade guide](https://moq.dev/setup/upgrade).

## Unreleased

### Paths with an empty segment are refused ([#6648](https://github.com/moq-dev/moq/pull/6648))

A session dialed at a URL path with a trailing or doubled `/`, such as `/room/` or `/room//cam`, is refused as a bad request instead of collapsed; dial `/room`. An auth grant whose `root`, `mounts`, `tier`, or `label` has one is refused as forbidden, and `stats.prefix`, `stats.node`, and `--cluster-tier` with one refuse to start.

### `moq_net::Path` has no lifetime

`auth` token fields that were `PathOwned` are `moq_net::Path`. `auth::Token::new` and `auth::Lease::new` return `Result`, refusing a grant whose root, mount, tier, or label is longer than `moq_net::Path::MAX_BYTES`. See moq-net's migration for the rest.

### Tiers are metric-safe names (#6778)

`--cluster-tier` and the auth server's grant and refusal `tier` must be `/`-joined segments of ASCII letters, digits, `.`, `_`, and `-`.
The relay refuses to start with any other `--cluster-tier`, and treats a grant naming one as an auth server error; rename such tiers, for example `gold tier` to `gold-tier`.

### Unknown `MOQ_*` environment variables stop startup ([#6633](https://github.com/moq-dev/moq/pull/6633))

A `MOQ_*` variable moq-relay does not read, such as a misspelling, a removed name, or one meant for another binary, now stops startup with the variable named instead of being ignored.
Remove or rename what the error lists.

### Auth `server_name` is the SNI only (#6509)

The auth request's `server_name` no longer falls back to the HTTP `Host` header, `:authority`, or the WebTransport CONNECT host: it is absent over plain `http://` and `ws://` and for an IP-literal dial, while HTTPS and `wss://` report the SNI.
An auth server that scopes tenants by hostname must see them over TLS, and still binds the tenant to the credential.

### A client certificate alone lists no hidden routes (#6509)

Only a grant with `peer: true` lists `.`-named routes to a session that did not ask for them.
Have the auth server mark cluster peers as `peer` (`moq auth serve --mtls-peer`) before upgrading, or a peer below moq-lite-07-wip loses those paths.

### `stats.interval` is a duration (#6472)

`stats.interval`, `--stats-interval`, and `MOQ_STATS_INTERVAL` take a duration such as `"1s"` or `"500ms"` instead of a bare number of seconds, and a zero interval refuses to start.
Write `interval = 5` as `interval = "5s"`.
In Rust, `stats::Config::interval` is an `Option<Duration>` (`None` publishes every second), and `stats::Config::build` returns a `Result`.

### `receive_window` has a floor (#5878)

The `[quic]` and `[peer.quic]` `receive_window` must be at least 67108880 bytes (64 MiB plus 16), or the relay refuses to start; the `[quic]` default is now 128 MiB.
Raise an explicit value below that, or remove it.

### WebSocket clients must offer a QMux subprotocol (#6231)

The relay answers a WebSocket upgrade with 400 when its `Sec-WebSocket-Protocol` offers none of the relay's QMux subprotocols, instead of falling back to the legacy `webtransport` format.
Upgrade any client that still speaks only `webtransport` over WebSocket before the relay.

### io\_uring workers start serving separately (#6245)

`uring::Workers::serve(cluster, auth, shutdown, sessions)` is now `spawn` with the same arguments, which sets every worker up without accepting, followed by `serve()`, which starts them accepting.
Install anything that must be in place before the first session (`Relay::run` installs its signal handlers) between the two.

### `Accepted` takes any transport's handshake (#6176)

`Accepted` loses its transport type parameter, since `moq_net::server::Handshake` has none: write `Accepted` instead of `Accepted<S>`.

### `supervise` takes a `Serving` guard (#6144)

`moq_relay::supervise` takes the `shutdown::Serving` from `Observer::serve` in place of the `Observer`.
Take it before the session's SETUP reply, and bound that reply with `Observer::until_deadline`, so the drain waits for the session to report its end but not for a stalled reply:

```rust
let serving = shutdown.serve();
let (session, driver) = shutdown
	.until_deadline(request.ok())
	.await
	.context("the SETUP reply was still pending at the drain deadline")??;
moq_relay::supervise(session, lease, serving, None).await
```

### `cluster.lan.secret` is a file path only (#5910)

`--cluster-lan-secret`, `MOQ_CLUSTER_LAN_SECRET`, and `[cluster.lan] secret` no longer accept 64 hex characters inline.
Write the hex to a file (`openssl rand -hex 32 > cluster.key`) and pass its path; `LanConfig::secret` is now a `PathBuf`.

### Admission runs under the handshake deadline (#5765)

`listen.timeout` now bounds auth admission as part of the handshake, replacing the fixed ten-second admission timeout, so `"0"` also lets admission wait forever.
HTTP reads bound admission by the web listener's timeout.
`Auth::admit` and `Cluster::admit` no longer time out on their own, so a slow embedded decider no longer refuses after ten seconds: the caller owns the deadline.
Bound them with `moq_tokio::server::Request::within` on an accepted session, or wrap them in `tokio::time::timeout`; dropping the future withdraws the admission from the decider.
Tokens attached in band keep their ten-second bound and are refused as `auth::Error::Unavailable` past it.

### Usage types come from usage-rs 7 (#6060)

The relay's `Args` and `Config` derives now implement usage-rs 7 traits.
A CLI that flattens them depends on `usage-rs = "7"` under its own name: the derives emit `::usage_rs` paths and no longer read `Cargo.toml`.

### Typed admission refusals (#5996)

`auth::Error::Refused` now carries the shared `moq_auth::Refusal`.
Pass `auth::Error::Refused(moq_auth::Refusal::default())` to `Admission::refuse` for an unattributed refusal, or set its `reason`, optional verified `root`, and `tier`.
Match `auth::Error::Refused(refusal)` to inspect it; never use the dialed path as refusal attribution.

### Key logging is opt-in (#5907)

`SSLKEYLOGFILE` alone no longer writes TLS session secrets.
Add `--quic-keylog` (`MOQ_QUIC_KEYLOG`, `[quic] keylog = true`) to capture them; without `SSLKEYLOGFILE` it stops startup.

### Cluster dials build from a template (#5674)

`Relay::client()` and `Cluster::with_client` are removed.
Give the cluster `with_connect(connect, quic)`, plus `with_iroh(endpoint)` for `iroh://` peers, and build your own `moq_tokio::Client` for other outbound dials.

### `max_streams = 0` is refused (#5871)

A `[quic]` `max_streams` of 0 stops startup, as `[peer.quic]` already did, since zero streams wedge the connection.
Remove it to keep the default.

### Auth roles are patterns

`auth::Token::publish` and `subscribe` are `moq_auth::Patterns`, matching release.
Read and construct each pattern union directly instead of using `moq_auth::Access` and `.path`.
Token and auth-server grant parsers refuse object roles, including bitrate caps, before relay admission; no transport currently enforces a cap.

### Workers default to unpinned (#5688)

`runtime.pin` defaults to off, so two worker groups on one host no longer crowd onto the first cores.
Set it to keep the old placement.

```toml
[runtime]
pin = true
```

### iroh is opt-in (#5223)

The default build has no iroh listener and no `--iroh-*` flags or `[iroh]` table.
Build with `--features iroh` to keep it.

### The `noq` cargo feature is `quic`

Rename it wherever you enable it; the QUIC stack is MoQ's own `moq-quic`.
A qlog directory gets one file per endpoint instead of one per connection.

### A QUIC load-balancer nonce needs its server id (#5114)

`listen.lb_nonce` (`--listen-quic-lb-nonce`, `MOQ_LISTEN_QUIC_LB_NONCE`) without `lb_id` stops startup.
Setting `lb_id` or `lb_nonce` together with `load_balancer` also stops; `lb_id` used to win.
Set `load_balancer`, or `lb_id` alone (nonce 8), or `lb_id` with `lb_nonce`.

### Settings no listener reads stop startup (#5006)

A stream-only relay (TCP or Unix, no `--listen`) refuses the QUIC-only `--listen-preferred-v4` / `-v6`, `--listen-quic-lb-id`, and pinned `tls.peers`.
It also refuses `--listen-tls-cert`, `-key`, or `-generate` unless `--listen-tcp-tls` serves it, and `listen.tls.root`, since only QUIC verifies a client CA.
`--listen-unix-allow-*` needs `--listen-unix-bind`, and `web.https.cert`, `key`, and `root` need `web.https.listen`.
Each used to be ignored: drop it, or add the listener it configures.

### `--cluster-mesh` and `--cluster-linger` are unknown flags (#4818)

Remove the flags, TOML `mesh` and `linger`, and `MOQ_CLUSTER_MESH` / `MOQ_CLUSTER_LINGER`.
The removed environment variables stop startup rather than being ignored.
List every peer with `--cluster-connect` or `--cluster-connect-api`.
In Rust, `cluster::Config` has no `mesh` or `linger` field.

### A route without a publisher keeps hop 0 (#4969)

A route that names no publisher keeps its 0 instead of gaining a random hop.

### Embedding: auth takes the client-CA answer (#4912)

`auth::Config::validate` and `init` take `client_ca: bool`, whether any listener verifies client certificates, and `validate_client_ca` is gone.
Pass `client_ca` to `validate` and delete the `validate_client_ca` call.

### Embedding: io\_uring workers bind from the server config (#5312)

`uring::Workers::bind` takes a `moq_tokio::server::Config`, so generated and rotating certificates work in both worker modes.
Set `listen` and `quic` on a `moq_tokio::server::Config` and pass it to `bind`.

## 0.16 -> 0.17

### Gossip discovery is removed (#4601)

A relay dials only the peers it lists or finds on the LAN, never a URL learned from an announcement, and no longer announces `.internal/origins`.
`--cluster-mesh` and TOML `mesh` are refused by name: list every peer with `--cluster-connect` or `--cluster-connect-api`.
The `/nodes` endpoint lists only peers this relay dialed.

## 0.15 -> 0.16

### 0.14 auth behavior is restored (#4319)

- Removed 0.14 auth settings (`[auth]` `key`, `key_dir`, `auth_api`, `domains`, `mtls_tier`, `[auth.tls]`, their flags and `MOQ_AUTH_*` variables) stop the relay with the replacement named.
- Public and mTLS patterns are rooted at `/`, as in 0.14: `anon/**` admits a client dialed at `/anon`.
- A token's `iss`, `sub`, and `jti` are ignored and `nbf` is enforced; any other claim, `aud` and `cluster` included, refuses the token.
- A relay on `--auth-public` refuses a session presenting a token, including a peer's `cluster.token`, and refuses to start with a client CA.

Replace `key_dir` with `moq auth serve --key-dir keys/` and point `[auth]` `url` at it, such as `http://127.0.0.1:4440`.

## 0.14 -> 0.15

### Flags split by direction

The flags split into `--listen-*` (accepting), `--connect-*` (dialing), and a shared `--quic-*` section.
Environment variables follow the flag (`MOQ_SERVER_BIND` is `MOQ_LISTEN`), and a renamed one is refused with its replacement named.

| Before | After |
| --- | --- |
| `--server-bind` | `--listen` |
| `--server-*`, `--tls-cert`, `--tls-key`, `--tls-generate` | `--listen-*`, `--listen-tls-cert`, `--listen-tls-key`, `--listen-tls-generate` |
| `--client-connect` | `--connect` |
| `--client-*` | `--connect-*` |
| `--client-failover-delay` | `--connect-race` |
| `--client-reconnect=false` | `--connect-once` |
| `--tls-disable-verify`, `--client-tls-disable-verify` | `--connect-tls-insecure` |
| `--server-quic-*`, `--client-quic-*` | `--quic-*`, applied to both directions |
| TOML `[server]`, `[client]` | `[listen]`, `[connect]` |
| TOML `[server.quic]`, `[client.quic]` | `[quic]` |
| TOML `listen`, `connect`, `failover_delay`, `reconnect`, `disable_verify` | `bind`, `url`, `race`, `once` (inverted), `insecure` |
| `--cluster-linger` | removed; a broadcast closes when its last publisher is lost |
| `--cluster-connect host:port` | a full URL, `https://host/?jwt=TOKEN` |

### Auth is one contract (#3688)

The relay asks an auth server per session (`--auth-url`) or applies a static grant (`--auth-public`); exactly one is required.
`--auth-key`, `--auth-key-dir`, `--auth-api`, `--auth-api-mode`, `--auth-public-api`, `--auth-domain`, `--auth-mtls-tier`, and `--auth-tls-*` are gone: run `moq auth serve --key-dir ...` beside the relay and point `--auth-url` at it.
The flag-by-flag mapping is in [Migrating from the relay flags](https://moq.dev/bin/relay/auth#migrating-from-the-relay-flags).
A verified client certificate admits nothing on its own; the auth server grants it.

```bash
moq auth serve --key-dir keys/ --public-publish 'anon/**' --public-subscribe 'anon/**' &
moq-relay --auth-url http://127.0.0.1:4440
```

### Grants are patterns

`anon` is exactly the broadcast `anon`; write `anon/**` for the subtree.
This applies to `--auth-public`, TOML `public`, and `[auth.public]`, which is now `public_subscribe` / `public_publish`.
A public or mTLS pattern with no wildcard refuses to start, naming the subtree to write.
JWT `publish` and `subscribe` claims are patterns too, so a token granting `alice` covers only `alice`.

### Embedding

`Relay` fields are private (#3638): clone the handles you need, such as `relay.cluster().origin.clone()`, mount routes, then call `Relay::run`.
`Cluster::with_cache` moved to `cluster::Options`.
The `quinn` and `quiche` features and the backend setting are gone (#3811).
