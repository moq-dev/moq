# [M] Stamp routing identities

## Goal

The receiver of every session, a relay or a direct peer, stamps the
session's routing identity and ignores a self-named one. The identity a
session's own claims are filed under is one the receiver assigned to that
session, never a value the session declared, so no session can advertise
another origin's identity as its own or advance its seqno. Rust and JS both
do it, and the lite draft says so.

## Plan

Decided by the maintainer 2026-10-25: relays stamp the identity themselves
instead of making sessions prove a per-process key. A self-named identity
cannot be verified without a key proof, and the proof prototype
([#6310](https://github.com/moq-dev/moq/pull/6310)) found that fresh nonces
alone let a keyless intermediary relay both honest exchanges, so a sound proof
needs a transport exporter or authenticated routing controls. Key proof
belongs to [Peer grants](/quest/m2/p2p/peer-grant.md), which owns the key
(`cnf`) and its proof. lite-07 is not gated on a proof.

Decided by the maintainer 2026-10-25: no cross-path recognition. A direct
peer stamps the session itself, exactly as a relay does, so an origin
reached through the CDN and directly is two identities, and switching between
them resumes only across a matching explicit epoch. Simpler: one identity
regime, with nothing carrying a relay's stamp onto another session.

Today a receiver takes the peer's declared Hop ID (ANNOUNCE_OK `Hop ID`) as
that session's identity and appends it to every hop chain; the identity it
assigned (`peer_hop`, `Client::with_peer_hop`) is used only when the peer
declares 0 and is never forwarded (`rs/moq-net/src/lite/subscriber.rs`
`chain`/`via`, `js/net/src/lite/subscriber.ts`). The stamp is a fresh value
per session, distinct from forwarded and subject-qualified identities
([Routes and announces](/quest/m1/cluster-routing/routes.md)), and is
forwarded downstream like any other identity.

This lands on today's announce path, ahead of routes, which files every
claim by identity. Open, for the implementer to settle and record: path
vectors detect loops by a relay finding its own declared Hop ID in a chain,
so a neighbor-stamped transit hop would hide loops through it. A declared Hop
ID may stay a loop and split-horizon hint (a liar only drops its own routes)
while only the identity of claims the session originates is stamped, or
every hop is stamped and loops are cut another way. Also settle whether
lite-06 changes at all (it is published).

Draft (`drafts/draft-lcurley-moq-lite.md`, lite-07-wip): Routing (a receiver
"MAY assign an identity" that "MUST NOT" be forwarded), Hop Parameter,
ANNOUNCE_OK (`Hop ID` as the publisher's own, "SHOULD assign itself one"),
ANNOUNCE_START (a relay appends the upstream's ANNOUNCE_OK `Hop ID`; an
assigned identity is never forwarded), and Routing Metadata and Privacy;
`just drafts check` passes.

Tests: a session declaring another origin's identity gets its own stamp and
captures none of that origin's claims; two sessions declaring one identity
are filed apart; a loop through a stamped hop is still cut. Rust/JS interop
in CI. Public API: report any change to `with_peer_hop`.
