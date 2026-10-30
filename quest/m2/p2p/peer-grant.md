# [M] Peer grants

## Goal

The relay issues each session a short-lived, key-bound grant that another
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
session's publish and subscribe patterns, a presenter public key (`cnf`),
`iat`, and a short `exp`, and no routing identity. Landed in `moq-auth`:
the type (a `root` plus patterns, `cnf.jwk` per RFC 7800), ES256/EdDSA-only
`KeySet::sign_peer`/`verify_peer`, the public `KeySet::to_peer_set`, and
`PeerGrant::binds` for the roster and proven-key checks, with the docs.
What remains is the relay issuance, the wire, the possession proof, and
`@moq/auth`. The presenter key is
public on the roster, so a copied JWT is not enough: the presenter proves
possession of the matching private key on the direct AUTH stream. A roster peer that
received someone else's grant cannot replay it.

Issuance: the relay owns AUTH (`rs/moq-relay/src/auth/refresh.rs`). The
client mints one key per process or tab and announces its public half on the
relay's AUTH stream (AUTH_CNF); it is the `cnf`. Then, after AUTH_OK:

- `AUTH_KEYS { Jwks (b) }`, the relay's kid-indexed public JWKS,
  asymmetric keys only, HMAC material never included;
- `AUTH_PEER { Grant (b) }`, the peer-grant JWT.

AUTH_KEYS and AUTH_PEER may repeat for refresh and key rotation. The lite
draft and both language codecs gain AUTH_KEYS and AUTH_PEER in whichever lite
wip is current when this lands; they do not gate the lite-07 cut (decided
2026-10-13). Decided 2026-10-25 (maintainer): this quest owns AUTH_CNF and
the possession proof, which do not gate the cut either, since lite-07
routing identities stay declared and unverified; `just drafts check` passes. Reissue before expiry; the client
replaces the grant on each live peer session with `auth().add` and drops
the old one. A key change mints a new grant; the old key is dead.

Verification on a direct session: the opener writes `AUTH { Token (b) }`
with the JWT and proves possession of the `cnf` key bound to this session.
The proof prototype ([#6310](https://github.com/moq-dev/moq/pull/6310)) found
that mutual fresh nonces alone let a keyless intermediary forward both honest
exchanges. ✅ Decided 2026-10-30 (maintainer): the presenter signs each
routing control the session sends with the `cnf` key, over a session
transcript. This works over WebRTC data channels, which expose no DTLS
exporter, and keeps one code path and one set of replay tests. Binding a
transport exporter was rejected: browsers expose none, so it covered only
native QUIC pairs, and a hybrid would need both. The wire work defines the
transcript and adds the replay tests before the wire is fixed. The
receiver verifies the relay signature against the JWKS from its own
AUTH_KEYS, checks `exp`, matches `cnf` against the presenter key in the
peer's roster entry, and requires `cnf` to equal the key the session proved.
The origin then serves only the granted paths. A missing, expired,
HMAC-signed, or unproven grant, or one whose `cnf` is not the session's
proven key, is not a grant.

The grant authorizes paths and binds a key; it carries no routing identity.
A routing identity is declared by its origin and unverified
([Routes and announces](/quest/m1/cluster-routing/routes.md)). Decided
2026-10-26 (maintainer): recognizing a routing identity needs key proof, and
this quest owns the key and its proof; receiver-stamped per-session
identities were dropped because they split one origin into several
identities across a relay mesh. Binding a routing identity to the key is
not planned yet. Switching between a CDN and a direct path resumes only
across a matching explicit epoch.

[Signaling and policy](/quest/m2/p2p/signal.md) presents the grant in band
on each direct session. P2P is its only consumer, so it lives on this line
rather than the auth line (decided in the 2026-09-30 audit).

Docs: `doc/bin/relay/auth.md` states that peer grants need an asymmetric
key, that HS256 operators get none, and that the public JWKS is not a
secret; update its last paragraph once the relay issues them. Tests
remaining, on the wire: a key that does not match the
peer's roster entry is refused; a
grant whose `cnf` is not the session's proven key is refused; a grant copied onto another
session without the private key is refused; copying both grant and a prior
valid proof onto another session is refused; a refreshed grant replaces
the old one without dropping the session.

Additive.
