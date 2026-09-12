# Auth API

## Goal

`--auth-api` decides every connection the relay admits, with one request and
reply contract for every credential kind, and the relay asks the endpoint as
rarely as it can. Today an mTLS peer is admitted to whatever it dials with
publish and subscribe unscoped, the endpoint is told only that some
certificate was verified, a revalidation reply cannot move a session's tier,
and an endpoint that names no `max-age` turns revalidation off without a
trace. This line closes those gaps on the HTTP contract; the wire side is
[In-band auth](/quest/m2/auth/README.md).

## Plan

The caching model every quest here keeps:

- Token mode: the request depends on (root, `kid`, transport), so an audience
  sharing a signing key is one cached request per relay, however many viewers
  arrive at once.
- Proxy mode: the credential is part of the request and the reply is cached
  per credential, so auth cost tracks distinct credentials, not viewers.
- mTLS: the request carries the peer's identity and the reply is cached per
  (root, identity, transport). Not unrestricted by default once the endpoint
  speaks v1; never revalidated, so an endpoint outage cannot partition the
  mesh.
- Revalidation rides the admission cache, so a re-check can be answered from
  an entry up to one `max-age` old and the revocation window is up to twice
  `max-age`, floored at one second. That belongs in operator documentation,
  not a doc comment.

`--auth-api-mode proxy` landed on dev in #3044, and the quest that planned it
is deleted. Everything here branches from dev unless a quest says main.

## Quests

- [mTLS identity](/quest/m2/auth-api/mtls-identity.md) - an mTLS peer is
  authorized through the auth API like any other connection, named by its
  certificate, and a proxy grant can scope or refuse it
- [mTLS explicit scope](/quest/m2/auth-api/mtls-scope.md) - a v1 reply must
  grant an mTLS peer its scope; unrestricted survives only for unversioned
  endpoints
- [Revalidation updates](/quest/m2/auth-api/revalidation-updates.md) - a
  re-check moves the tier in place, names an alias change, and a reply that
  disables revalidation says so once

## Related

- [Relay auth](/quest/m2/path-patterns/relay-auth.md) - versions the grant
  shape and resizes a live session; mTLS explicit scope rides its v1
- [In-band auth](/quest/m2/auth/README.md) - the AUTH stream, where a
  credential can arrive after the connection
