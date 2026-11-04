# [XS] moq-auth drops the unused peer grant API

## Goal

`moq-auth` no longer carries the P2P peer grant: `PeerGrant`
(`rs/moq-auth/src/peer.rs`), `KeySet::sign_peer`/`verify_peer`,
`KeySet::to_peer_set`, `PeerGrant::binds`, and their wire type are deleted,
along with the "Peer grants" section of `doc/bin/relay/auth.md` and the crate
docs that mention them. No other crate uses them.

## Plan

Planned 2026-11-02 (maintainer, /quest-unblock): P2P trusts its peers, so
relay-issued, key-bound peer grants are dropped until a real use case asks for
per-peer scopes ([Signaling and policy](/quest/m2/p2p/signal.md) records why).
The landed API has no consumer left. git history keeps it if a use case
returns.

Public API: removes `moq_auth::PeerGrant` and the `KeySet` peer methods
(breaking for moq-auth); note it in its MIGRATE. Wire: none.
