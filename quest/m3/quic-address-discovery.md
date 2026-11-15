# [M] QUIC address discovery

## Goal

A moq-quic client learns the address its peer sees it from, and every later
change to it, such as a NAT rebinding, from the connection itself: moq-quic
implements draft-ietf-quic-address-discovery (its transport parameter and
`OBSERVED_ADDRESS` frames), and a relay sends each client its observed
address. A downstream node reads its NAT-mapped address from its relay
connection with no STUN server.

## Plan

Decided 2026-11-15 (maintainer, `/quest-plan` from kixel.ai's
[hole punching](https://github.com/kixel-ai/kixel.ai/blob/main/quest/m2/punch.md),
[kixel-ai/kixel.ai#116](https://github.com/kixel-ai/kixel.ai/pull/116)):

- ✅ In moq, as a generic QUIC extension, while punching itself stays in
  kixel.ai (rejected: restoring the relay's STUN responder, removed with P2P,
  which needs a demux and polling to see a rebinding; a moq-lite whoami
  message, non-standard for what QUIC standardizes).
- ✅ m3 (rejected: m4, the rule for a downstream's later plans).
- ✅ On by default in both directions, with no config: every endpoint offers
  to send and to receive observed addresses (rejected: a client opt-in; off
  with a config to enable). A peer only ever learns its own address, and the
  cost is one frame per path at the handshake and on each change.

Implement the newest draft version with its codepoints; a peer that does not
know the transport parameter ignores it. Send a fresh `OBSERVED_ADDRESS` on a
new path and when a path's remote address changes, under the draft's
sequence-number rules so a reordered frame never rolls an address back.
iroh's quinn fork carries an implementation to compare against.

Public API, recommended: moq-quic's `Connection` reports the latest observed
address and a `poll_observed_address` wait, in the crate's `poll_*` style;
moq-tokio's QUIC session exposes it, and moq-relay exposes it per upstream
link to an embedder, which is where kixel.ai's daemon reads it. Recommended:
not in moq-ffi or JS until a consumer asks, since browsers cannot read it and
it says nothing about media.

Tests on the seeded sim with mocked time: the client learns its address at
the handshake, a NAT rebinding mid-connection delivers the new one, a
reordered frame never rolls back, and a peer without the extension
interoperates. Run the QUIC Interop Runner's pinned peers to confirm the
unknown parameter costs nothing.

Public API: additive. Wire: a QUIC transport extension; no moq-lite change.
