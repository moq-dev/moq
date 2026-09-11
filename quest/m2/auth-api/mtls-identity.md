# [M] An mTLS peer is authorized through the auth API

## Goal

A peer that presented a client certificate is authorized through the same
auth API request as every other connection, and the endpoint learns which
certificate: `mtls=<identity>` replaces `mtls=true`. In proxy mode the
endpoint's grant, or its absence, decides what the peer may publish and
subscribe; in token mode a certificate-authenticated peer with no grant stays
unrestricted until [mTLS explicit scope](/quest/m2/auth-api/mtls-scope.md),
so an endpoint answering `{alias, tier}` today keeps that reply shape. The
request side is a breaking change to the endpoint contract: an endpoint that
compares `mtls` to the literal `true` must accept any non-empty value before
its relays upgrade. Replies cache per identity. On dev, where the mode lives
and where breaking changes go.

## Plan

`Auth::verify_mtls` in `rs/moq-relay/src/auth.rs` calls `resolve_mtls`, which
builds its own `AuthApiRequest` carrying `mtls: true` and nothing else about
the peer, reads only `alias` and `tier` off the reply, discards the cache
hints, and mints `AuthToken::unrestricted`. The party being scoped picks the
scope, because the root is the path the client dialed.

- Delete `resolve_mtls`. Build the request through `api_request` and resolve
  through `authorize`, so a grant is read one way for every credential, with
  `host` in proxy mode as the mode already sends it.
- `mtls=<identity>` names the peer: the leaf certificate's first SAN DNS
  name, the CN when it has none, and the leaf's SHA-256 fingerprint when it
  has neither, so the value is never empty. `moq_tokio::tls::PeerIdentity`
  grows `name()` beside `expiry()` and `fingerprint()`, parsed with the
  `x509-parser` it already uses. The docs say the identity is a value the
  endpoint matches, never proof by itself; the CA that signed the chain is.
  This replaces the documented `mtls=true`, and an endpoint that compares
  the literal breaks for every mTLS peer, cluster peers included, until it
  accepts any non-empty value. The auth API section of `doc/bin/relay/auth.md`
  states the new value, the migration (accept non-empty first, then upgrade
  relays), and the release note carries it as breaking.
- The identity has to reach `verify_mtls` on every transport: QUIC keeps a
  `PeerIdentity` on the request, but the HTTPS and WebSocket path reduces the
  verified chain to the unit `MtlsPeer` marker in `rs/moq-relay/src/web.rs`,
  so that marker carries the identity too.
- Caching: the identity is in the query, so the shared HTTP cache and the
  in-flight coalescing already key on it: one request per (root, identity,
  transport), however many sessions that peer opens. `revalidate` stays
  `None` for mTLS peers, which the "mTLS peers must never revalidate" test
  pins: a deployed endpoint sending a blanket `Cache-Control: max-age` would
  otherwise arm revalidation across a relay mesh the moment this ships. Mesh
  revalidation is its own change, an explicit opt-in per identity plus a
  relay-side floor on the staleness window; `stale-if-error` alone is not
  enough, since an endpoint that answers "no" still partitions the mesh.
- Token mode: `mtls` satisfies "has a credential" without a JWT or a `key`;
  no grant means unrestricted, as today. Proxy mode: the endpoint returns a
  grant like anyone else, and no grant is a refusal.
- Tests: a proxy-mode mTLS peer refused by an empty reply, scoped by a narrow
  grant, and admitted unrestricted in token mode; `host` present on the proxy
  request; the SAN name, the CN for a certificate with no DNS SAN, and the
  fingerprint for a nameless one, over both QUIC and WebSocket; two sessions
  from one identity produce one endpoint request. Update the mTLS and auth
  API sections of `doc/bin/relay/auth.md`.

## Closes

- [#3087](https://github.com/moq-dev/moq/issues/3087) - close this issue when the quest finishes

## Related

- [mTLS explicit scope](/quest/m2/auth-api/mtls-scope.md) - removes the
  unrestricted default once the endpoint speaks v1
