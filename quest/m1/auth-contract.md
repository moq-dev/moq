# [S] The auth contract has one type per concept

## Goal

`moq-auth` and `@moq/auth` ship the contract moq.pro adopts with nothing
left over: no metering handle nothing meters, one struct for a
publish/subscribe pair, a reference server that scopes the way the library
and the docs say, and a JWK reader that refuses what the release note says
it refuses.

## Plan

- Delete `moq_auth::Counters`. The only writes are one closure at session
  end in `rs/moq-relay/src/connection.rs`; the relay's other paths and
  moq.pro pass `Counters::default()`. The consumer hands totals over when it
  ends the lease: `lease::Consumer::close(self, reason, bytes)`, `Drop`
  reports zero. `Client::connect(request)`, `Auth::admit(request)`,
  `Admission { request, reply }`, and `supervise(session, lease, shutdown)`
  each lose an argument.
- `serve::Policy::decide` calls `Claims::authorize(&request.path)` and grants
  the residuals instead of `root != path` refusal, and drops its private
  `normalize`. `doc/bin/relay/auth.md` and `doc/lib/rs/moq-auth.md` attribute
  the overlap rule to "the relay"; the relay forwards the raw path and
  enforces the grant it gets, so the docs name `moq auth serve`.
- Delete the `kty` default to `oct` in `rs/moq-auth/src/key.rs` and its
  test; JS already refuses and the release note claims Rust does.
- Delete `serve::Rules`; `Policy.public` and `Policy.mtls` are
  `Permissions`. `Grant`, `Claims`, `Scope`, `Permissions`, `Rules`, and the
  relay's `auth::Token` spell the same pair six ways today; stop at `Rules`
  unless flattening `Permissions` into `Grant` (JSON unchanged) reads better
  at the call sites.
- `Request::new(node, transport, path)` mints the id in every build; the
  four-argument form and the `client`-gated `connect` constructor go.
- `Peer.name` folds a DNS SAN, a CN, and a fingerprint into one string a
  server cannot tell apart. Decide between
  `Peer { fingerprint, dns: Vec<String>, common_name: Option<String>, expires, issuer }`
  and leaving `name` with its precedence documented on the wire.
- Decide whether `Grant.tier: Option<String>` becomes a `Tier` newtype with
  `Tier::INTERNAL`, shared by `moq-stats` and `@moq/auth`; the label is a
  string in three languages today and a mismatch bills the mesh to
  customers. Changing the field type is a break, so it is decided here, not
  after the release.
- Decide the `jwt::` module move that [CAT verify](/quest/m3/cat/verify.md)
  defers: `Claims`, `Key`, `Jwk`, `KeyId`, `Algorithm`, `KeySet`, and `Scope`
  are root exports today. Moving them is free before 0.1.0 ships and a
  breaking release after.
- Parity: `ClaimsSchema.exp` is `z.number()` where Rust refuses a float;
  the CLI verbs spell their flags `--out`/`--in` in Rust and `--key` in JS.

Public API: breaking on moq-auth, @moq/auth, and moq-relay's auth module,
so on dev. Wire: the auth JSON changes only if `Peer` is split. Consumers:
moq-relay, moq-cli `auth`, moq.pro's edge authorizer and API.

## Related

- [One auth path](/quest/m1/auth-one-path.md) - the mode fold that lands on the same types
- [Auth embedder](/quest/m2/auth-embedder.md) - the additive pieces an embedder still lacks after this
