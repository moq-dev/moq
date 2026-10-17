# [M] Peer grants

## Goal

The relay issues each session a short-lived, node-bound grant that another
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
session's publish and subscribe patterns, the origin's authenticated process/tab routing identity (shared by its live
sessions and refreshed on restart, per Routes and announces), a presenter
public key (`cnf`), `iat`, and a short `exp`. A node id is public on the
roster, so a copied JWT is not enough: the presenter proves possession of
the matching private key on the direct AUTH stream. A roster peer that
received someone else's grant cannot replay it.

Issuance: the relay owns AUTH (`rs/moq-relay/src/auth/refresh.rs`). The
client's routing key, announced and proven by
[Identity proof](/quest/m1/cluster-routing/identity.md), is the `cnf`; the
grant's routing identity is that key's hash, so a client cannot obtain a grant
for someone else's identity. Then, after AUTH_OK:

- `AUTH_KEYS { Jwks (b) }`, the relay's kid-indexed public JWKS,
  asymmetric keys only, HMAC material never included;
- `AUTH_PEER { Grant (b) }`, the peer-grant JWT.

AUTH_KEYS and AUTH_PEER may repeat for refresh and key rotation. The lite
draft and both language codecs gain AUTH_KEYS and AUTH_PEER in whichever lite
wip is current when this lands; they do not gate the lite-07 cut (decided
2026-10-13). The key and its proof moved to Identity proof, which does gate
it (decided 2026-10-14); `just drafts check` passes. Reissue before expiry; the client
replaces the grant on each live peer session with `auth().add` and drops
the old one. A node-id or key change mints a new grant; the old node id is
dead.

Verification on a direct session: the opener writes `AUTH { Token (b) }`
with the JWT. Possession needs no separate AUTH_POP: Identity proof already
binds the opener's key to this session, so the grant only has to name it. The
receiver verifies the relay signature against the JWKS from its own
AUTH_KEYS, checks `exp`, matches node id against the roster, and requires
`cnf` to equal the key the session proved. The origin then serves only the
granted paths. A missing, expired, HMAC-signed, or unproven grant, or one
whose `cnf` is not the session's proven key, is not a grant.

[Signaling and policy](/quest/m2/p2p/signal.md) presents the grant in band
on each direct session. P2P is its only consumer, so it lives on this line
rather than the auth line (decided in the 2026-09-30 audit).

Docs: `doc/bin/relay/auth.md` states that peer grants need an asymmetric
key, that HS256 operators get none, and that the public JWKS is not a
secret. Tests: ES256 round trip; an HS256-only relay issues nothing; a
peer grant fails `Claims` verification; a node-id mismatch is refused; a
grant whose `cnf` is not the session's proven key is refused; a grant copied onto another
session without the private key is refused; copying both grant and a prior
valid proof onto another session is refused; a refreshed grant replaces
the old one without dropping the session.

Additive.

## Required

- [Identity proof](/quest/m1/cluster-routing/identity.md) - the per-process key the grant binds as `cnf`
- [Routes and announces](/quest/m1/cluster-routing/routes.md) - the route-layer node id a grant binds
- [In-band auth](/quest/m1/auth/README.md) - relay tokens reach `main` with this line, so the relay owns AUTH and knows the session's paths
