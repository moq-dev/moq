# [M] Scoped peers prove a route key

## Goal

A lite-07 peer proves a route key once per announce stream. The requester
sends a nonce in ANNOUNCE_REQUEST, and the publisher's ANNOUNCE_OK carries its
public key and a signature over that nonce, bound to the session. A relay
files a proven peer's own routing identity, a u64 derived from its key, bare.
Every identity that peer forwards goes under a key qualified by the proven
key, so a scoped peer that forges or inflates another origin's identity only
affects claims inside its own grant. A peer that proves nothing gets a
per-session key. This lands in lite-07 before the cut.

## Plan

Decided 2026-11-04 (maintainer, /quest-unblock), from the keying question in
[Route session integration](/quest/m1/cluster-routing/routes/session.md).
This reverses the 2026-10-25 and 2026-11-02 decisions that no identity proof
gates the lite-07 cut. Options shown for each:

- Proof timing: ✅ before the lite-07 cut. Not chosen: an issuer-named route
  label in the grant now with the proof after the cut, or a per-session key.
- Where a relay's key comes from: ✅ a key file. An operator gives all its
  relays the same file, so two separate relays (the l1/l2 two-uplink fixture,
  `scoped_delivery_through_two_uplinks_reaches_one_key`) fail over as one key.
  A relay without a file generates one. The own identity stays fresh per
  restart as H(key, per-process salt), as `restart_reusing_identity_is_refused`
  requires. Not chosen: a fresh key per process.
- Browsers: ✅ prove with WebTransport's `exportKeyingMaterial(label, context,
  outputLength)` where the browser exposes it, detected at session setup (no
  runtime fallback); otherwise unproven with a per-session key. As of
  2026-11-04 only Firefox ships it (spec-matching from Firefox 160); Chrome has
  an Intent to Prototype and Safari nothing. Tabs do not forward routes, so an
  unproven tab loses little. Not chosen: browsers never proven, or signing the
  nonce with the dialed server name and role.
- Identity on the wire: ✅ ROUTE keeps its u64 node ID, the first 64 bits of
  H(key, salt), so the router's `Id` type and ROUTE stay as they are. Not
  chosen: the full 32-byte key in each ROUTE.

Binding: a bare nonce can be relayed (a peer passes another relay's signed OK
off as its own), so the signature covers the session too. Rust uses the TLS
exporter (`rs/moq-quic` already exposes one); browsers use
`exportKeyingMaterial`. Settle the exact signed transcript (nonce, exporter
output, role, a domain label) and record it in the draft.

Cost, estimated 2026-11-04: about 16 bytes of nonce in ANNOUNCE_REQUEST and
about 90 bytes of key and signature in ANNOUNCE_OK, no extra round trip, and
one Ed25519 sign and verify per announce stream rather than per route update.

Reuse: `rs/moq-crypto`'s `sign::PublicKey` and `SecretKey::load_or_generate`,
backed by `rind` Ed25519, and `js/crypto`'s WebCrypto Ed25519 signing.
moq-crypto's `sign` feature depends on moq-net, so either moq-net calls `rind`
directly or the key type moves to a lower crate. Decide which in the PR.

Public API: moq-net gains the proof on announce streams; moq-relay and
moq-tokio gain a route key file option. Wire: ANNOUNCE_REQUEST and ANNOUNCE_OK
change in lite-07 and its draft.

## Related

- [Route session integration](/quest/m1/cluster-routing/routes/session.md) - files identities under the key this proves
- [lite-07 route wire](/quest/m1/cluster-routing/routes/wire.md) - the announce messages this extends
- [P2P](/quest/m2/p2p/README.md) - its roster id is already planned as a public key
