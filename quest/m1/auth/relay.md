# [L] The relay and moq --server-bind admit through a lease

## Goal

moq-relay and `moq --server-bind` authorize every accepted session through
`moq_auth`: `--auth-url` asks a server per connection, `--auth-public` grants
anonymous patterns, setting both or neither fails at startup, and every other
auth flag is gone. A verified client certificate is reported in the request
and admits nothing on its own; cluster peers are admitted the same way. A
grant's `expires`, `revalidate`, `root`, and `tier` take effect on the live
session and an `end` event follows every close. The relay holds no HTTP
client and no cache. On dev.

## Plan

`rs/moq-relay/src/auth.rs` is deleted along with its config, the shared
HTTP cache, in-flight coalescing, `Cache-Control` parsing, `verify_mtls`,
`AuthToken::unrestricted`, and `--auth-domain`. What remains of `AuthConfig`
is `url: Option<Url>` and `public: Option<Public>`, validated as exactly
one. The `MtlsPeer` marker in `web.rs` and `websocket.rs` carries the
identity so WebSocket and HTTPS sessions report it too.

- The accept loop in `connection.rs` builds `moq_auth::Request` from
  `moq_tokio::server::Request`: `transport()`, `path()`, `query()`,
  `authority()`, `role()`, and `peer_identity()` exist; `moq-tokio` gains
  `remote_addr()`, `local_addr()`, `server_name()`, and `alpn()` on the
  request, and `PeerIdentity` gains `name()` and `issuer()` beside
  `fingerprint()` and `expiry()`, parsed with the `x509-parser` it already
  uses. The session `id` is minted here and logged, replacing the `u64`
  counter as the correlation key. `node` is the configured node name.
- `--auth-url` builds one `moq_auth::Client`; `--auth-public` builds a
  `lease::Producer` per session holding the static grant with no expiry. Either
  way the session receives a `lease::Consumer`, and the origin handles are
  scoped from `grant().publish` and `grant().subscribe` through the
  prefix-shaped adapter the package quest introduced, refusing any other
  pattern at connect and naming it. `root` replaces the dialed path before
  scoping, which is where aliasing lands.
- The session loop selects on `Consumer::changed()` and `Consumer::closed()`
  beside its existing work: a changed `tier` swaps the stats handle's label
  in place, a changed `root` or a grant that no longer covers the current
  scope closes with `Unauthorized`, a revocation closes with the reason
  mapped to the existing `Expired` variants. Dropping the `Consumer` on close
  sends `end`; the byte counters handed to the client are the session's
  existing meters.
- Cluster: the dial side keeps `connect.tls` or a URL token. The accept side
  goes through the lease like any client, so the smoke cluster fixture runs a
  `moq auth serve --mtls-publish '**' --mtls-subscribe '**'` and
  `doc/bin/relay/cluster.md` says that is what a mesh needs. `lan_peer_token`
  and the mDNS credential stay as they are and the docs say why.
- `moq --server-bind` in `rs/moq-cli` takes the same two flags and refuses
  to serve without one; `spawn_serve` stops calling `request.ok()`
  unconditionally and goes through the lease.
- Delete `reqwest-middleware` and `http-cache-reqwest` from the relay;
  `reqwest` stays only if something other than auth uses it.
- Docs: rewrite `doc/bin/relay/auth.md` around the contract (request, grant,
  refusal, revalidate and outage, the URL schemes, the mTLS section, the
  migration table the serve quest started), `doc/bin/relay/config.md`'s
  `[auth]`, `doc/bin/relay/cluster.md`, `nix/modules/moq-relay.nix`,
  `packaging/moq-relay/relay.toml`, `doc/bin/cli.md`, and the relay
  CHANGELOG as breaking. Search the repository for every deleted flag and
  `MOQ_AUTH_*` variable.
- Tests: `tests/auth_lifetime.rs` rewritten against `moq auth serve`
  covering admit, refuse, a 5xx at connect refusing, revalidate moving the
  tier, a narrower grant closing, a refusal closing, outage surviving until
  `expires`, and `end` arriving with counters; an mTLS peer refused with no
  server grant and scoped by a narrow one over QUIC and WebSocket; both flags
  set and neither set failing at startup; the cluster, LAN mesh, GOAWAY,
  released-CLI, and smoke suites on the new flags; `moq --server-bind`
  refusing without a flag; `just test smoke-full` green.

Public API: breaking on every relay auth flag and on the embedding surface
(`Relay` takes a lease per session; `moq-tokio` request accessors are
additive). Wire: none.

## Required

- [Serve](/quest/m1/auth/serve.md) - the server the tests and the smoke
  cluster run against

## Closes

- [#3087](https://github.com/moq-dev/moq/issues/3087) - close this issue when the quest finishes
- [#3603](https://github.com/moq-dev/moq/issues/3603) - close this issue when the quest finishes
- [#3058](https://github.com/moq-dev/moq/issues/3058) - close this issue when the quest finishes

## Related

- [Origin scopes](/quest/m2/path-patterns/origin.md) - lifts the
  prefix-shaped restriction and resizes instead of closing on a narrower grant
- [Relay tokens](/quest/m2/auth/relay-refresh.md) - an in-band token becomes
  a `connect` request through the same client
