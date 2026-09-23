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
session's publish and subscribe patterns, the origin hop id, a presenter
public key (`cnf`), `iat`, and a short `exp`. A hop id is public on the
roster, so a copied JWT is not enough: the presenter proves possession of
the matching private key on the direct AUTH stream. A roster peer that
received someone else's grant cannot replay it.

Issuance: the relay owns AUTH via
[Relay tokens](/quest/m1/auth/relay-refresh.md). The client writes
`AUTH_CNF { Jwk (b) }` after AUTH on the empty-token stream; the relay
copies that JWK into `cnf`. Then, after AUTH_OK:

- `AUTH_KEYS { Jwks (b) }`, the relay's kid-indexed public JWKS,
  asymmetric keys only, HMAC material never included;
- `AUTH_PEER { Grant (b) }`, the peer-grant JWT.

AUTH_KEYS and AUTH_PEER may repeat for refresh and key rotation. The lite
draft and both language codecs gain AUTH_CNF, AUTH_KEYS, AUTH_PEER, and
AUTH_POP; `just drafts check` passes. Reissue before expiry; the client
replaces the grant on each live peer session with `auth().add` and drops
the old one. A hop-id or key change mints a new grant; the old hop id is
dead.

Verification on a direct session: the opener writes `AUTH { Token (b) }`
with the JWT, then `AUTH_POP { Signature (b) }` over the JWT and the
receiver's hop id. The receiver verifies the relay signature against the
JWKS from its own AUTH_KEYS, checks `exp`, matches hop id and `cnf`
against the roster, and verifies AUTH_POP with `cnf`. The origin then
serves only the granted paths. A missing, expired, HMAC-signed, or
unproven grant is not a grant.

P2P is the first consumer:
[Signaling and policy](/quest/m1/p2p/signal.md) presents the grant in band
on each direct session. This quest does not depend on that line.

Docs: `doc/bin/relay/auth.md` states that peer grants need an asymmetric
key, that HS256 operators get none, and that the public JWKS is not a
secret. Tests: ES256 round trip; an HS256-only relay issues nothing; a
peer grant fails `Claims` verification; a hop mismatch is refused; a
JWT presented without AUTH_POP is refused; a grant copied onto another
session without the private key is refused; a refreshed grant replaces
the old one without dropping the session.

Additive.

## Required

- [Lite stream](/quest/m1/auth/lite.md) - the AUTH stream the grant rides
- [Relay tokens](/quest/m1/auth/relay-refresh.md) - the relay owns AUTH and knows the session's paths

## Related

- [Signaling and policy](/quest/m1/p2p/signal.md) - the first consumer
