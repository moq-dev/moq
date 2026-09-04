# [M] Peer reconfigure

## Goal

A cluster peer entry can carry structured policy that has no home in a URL:
the price this relay charges to pull from the peer, the price it declares to
the peer for the reverse direction, and the credential. Changing any of it at
runtime replaces the live session the same way a `?cost=` change does today,
and an identical render stays a no-op.

## Plan

[moq#2874](https://github.com/moq-dev/moq/pull/2874) landed the URL half:
`?cost=` and an inline `?jwt=` are dial configuration, the query-less URL is
the identity, and a changed render replaces the session while preserving
inactive fallbacks and the last-good topology. What it cannot express is an
asymmetric link from one side. One `?cost=N` is both what this relay charges
locally to pull from the peer and what it declares in SETUP as its own egress
price, so pricing the two directions differently needs the peer to list us
with its own `?cost=`.

Decisions:

- Peer entries in `cluster.connect` and `connect_api` accept an object beside
  the bare URL string, deserialized untagged into the existing `DialTarget`:
  `url` (required, still the canonical identity), `cost` (what this relay
  charges to pull from the peer, the routing input), `egress` (what it declares
  in SETUP as its own price toward the peer, defaulting to `cost`), and
  `token` (replaces an inline `?jwt=`). `DialTarget` grows `egress` and a
  normalized credential, with an inline `?jwt=` and an object `token` parsed
  to the same representation, and its equality covers every field, so an
  `egress`-only or `token`-only update is a change and the token is never
  dropped. Unknown fields reject the whole list, so a typo keeps the
  last-good topology exactly as a malformed URL does.
- Gossip and mDNS keep advertising URLs only. Their allowlist admits `?cost=`
  alone, and the draft already makes a declared price an assertion the
  receiver may override, so a peer never needs to push structured policy at us.
- Any field change replaces the session, the rule [moq#2874](https://github.com/moq-dev/moq/pull/2874)
  set for `?cost=`. A URL entry and an object entry that normalize to the same
  `DialTarget` are one entry, deduplicated as `parse_peer_list` already does;
  two entries for one identity with differing policy are the conflict that
  rejects the list, whatever their forms. Reconciling cost in place without a
  redial is deliberately not done: it would be a second code path for one
  field.
- No per-peer wire version. ALPN negotiation picks the best common version per
  session and the global `--version` list is the only pin; a per-peer
  override is a fleet cutover concern that stays downstream.

The split lands in `moq_tokio::Client` as separate charged and declared costs
(today `with_cost` sets both), and in the relay's session setup so the routing
side reads the charged value while SETUP carries the declared one.

Tests: the asymmetric link in both directions, two relays each pricing the
other differently, with routes ranked per side from the charged value and the
declared value visible on the far side; a `connect_api` update that changes
only `egress` or only `token` redials that peer and no other; an identical
object render is a no-op; equivalent URL and object forms of one peer
deduplicate while differing policies for one identity conflict; an unknown
field keeps the previous list. Update `doc/bin/relay/cluster.md` and
`doc/bin/relay/config.md` with the object form.
