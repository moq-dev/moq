# [M] Peer grants

## Goal

The relay issues each session a short-lived, hop-bound grant that another
client can verify without learning a signing key. A browser presents that
grant on a direct session and serves only the paths it names. HMAC keys
never leave the relay: a peer grant is signed with an asymmetric key
(ES256 or EdDSA), and only the public JWK is distributed. An operator with
only HS256 keys issues no peer grant, and a session with none serves
nothing on a direct connection.

## Plan

`moq_auth::Claims` stays the bearer token the relay verifies. A peer grant
is a separate type (`moq_auth::PeerGrant`) so presenting one as `?jwt=`
fails verification rather than widening a relay session. It carries the
session's publish and subscribe patterns, the origin hop id, `iat`, and a
short `exp`. No audience reuse of the relay token: a type the relay's
`Claims` verifier rejects is the isolation.

Issuance: the relay owns AUTH via
[Relay tokens](/quest/m2/auth/relay-refresh.md). After the empty-token
AUTH_OK, it writes the peer grant JWT and the public JWKS (kid-indexed,
asymmetric keys only) on that same stream. HMAC material is never in the
JWKS. Reissue before expiry on the same stream; the client replaces the
grant on each live peer session with `auth().add` and drops the old one.
A hop-id change (new origin) mints a new grant; the old hop id is dead.

Verification: `@moq/auth` and `moq-auth` verify the signature against the
JWKS this session received, check `exp`, and check the hop id against the
roster. The receiver's origin then serves only the granted paths, the same
filtering a relay session applies. A missing, expired, or HMAC-signed
grant is not a grant.

P2P is the first consumer:
[Signaling and policy](/quest/m2/p2p/signal.md) presents the grant in band
on each direct session. This quest does not depend on that line.

Docs: `doc/bin/relay/auth.md` states that peer grants need an asymmetric
key, that HS256 operators get none, and that the public JWKS is not a
secret. Tests: ES256 round trip; an HS256-only relay issues nothing; a
peer grant fails `Claims` verification; a hop mismatch is refused; a
refreshed grant replaces the old one without dropping the session.

Additive.

## Required

- [Lite stream](/quest/m2/auth/lite.md) - the AUTH stream the grant rides
- [Relay tokens](/quest/m2/auth/relay-refresh.md) - the relay owns AUTH and knows the session's paths

## Related

- [Signaling and policy](/quest/m2/p2p/signal.md) - the first consumer
