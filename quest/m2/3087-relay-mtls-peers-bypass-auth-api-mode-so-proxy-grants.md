# [M] relay: mTLS peers bypass the auth API mode, so a proxy grant cannot refuse or scope them

## Goal

An mTLS peer is authorized through the same auth API path as every other
connection. In proxy mode the endpoint's grant, or its absence, decides what
the peer may publish and subscribe; in token mode a certificate-authenticated
peer with no grant stays unrestricted, so a deployment answering
`{alias, tier}` today is unaffected. Fixes on dev, where the mode lands.

## Plan

`Auth::verify_mtls` in `rs/moq-relay/src/auth.rs` calls `resolve_mtls`, which
builds its own `AuthApiRequest`, reads only `alias` and `tier` off the reply,
and mints `AuthToken::unrestricted`. Nothing the endpoint returns can narrow
that, and the request carries no `host`, so host-routed tenants dialing the
same path are indistinguishable at the endpoint.

- Delete `resolve_mtls`. Build the request through `api_request` with
  `mtls: true`, plus `host` in proxy mode as the mode already sends it, and
  resolve through `authorize`, so a grant is read one way for every credential.
- Token mode: `mtls: true` satisfies "has a credential" without a JWT or a
  `key`; the certificate is the token. No grant means unrestricted, as today.
- Proxy mode: the endpoint returns a grant like anyone else, and no grant is a
  refusal, consistent with the rest of the mode.
- `revalidate` stays `None` for mTLS peers, which the "mTLS peers must never
  revalidate" test already pins. A deployed endpoint sending a blanket
  `Cache-Control: max-age` would otherwise arm revalidation across a production
  relay mesh the moment this ships, gating fleet interconnect on that endpoint
  staying reachable. Mesh revalidation is its own change: an explicit opt-in
  for `mtls=true` and a relay-side floor on the staleness window. Note
  `stale-if-error` alone is not enough, since an endpoint that successfully
  answers "no" still partitions the mesh.
- Tests: a proxy-mode mTLS peer refused by an empty reply, scoped by a narrow
  grant, and admitted unrestricted in token mode; `host` present on the proxy
  request. Update the mTLS section of `doc/bin/relay/auth.md`.

## Required

- [Auth verdict](/quest/m2/auth-verdict.md) - lands the proxy mode, the grant response, and the `host` field this builds on

## Closes

- [#3087](https://github.com/moq-dev/moq/issues/3087) - close this issue when the quest finishes
