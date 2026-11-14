# [L] moq-quic passes the QUIC Interop Runner against other stacks

## Goal

Nightly runs the [QUIC Interop Runner](https://github.com/quic-interop/quic-interop-runner)
with a moq-quic endpoint built from this checkout, as client and as server,
against mature QUIC stacks. Every case moq-quic supports passes or has a
quest, every case it does not support exits 127 with the reason written down,
and a new failure turns Nightly red with the runner's logs and pcaps attached.

## Plan

Decided 2026-11-13 (maintainer, /quest-plan on what testing to add): moq-quic has only ever met itself, browsers over WebTransport, and
moxygen through `test/ietf-conformance`. The runner's cases (handshake and
transfer under loss and corruption, retry, amplification limit, key update,
ECN, port and address rebinding, migration, goodput, cross traffic) are the
ones a hard fork of quinn-proto is most likely to have broken unseen.

- ✅ m1 (rejected: m4, where the questline puts tests; all new testing quests
  in m1).
- ✅ Run it ourselves in Nightly first; public listing is
  [its own quest](/quest/m4/quic-interop-listing.md) (rejected: never
  listing; listing in this quest with our red cells public from day one).

Agent's choices, for the implementer to revisit:

- The endpoint runs on moq-tokio's `quic::driver`, so the I/O loop production
  uses is under test, not only the state machine. It speaks the runner's
  `hq-interop` HTTP/0.9 file transfer; the `http3` case reuses the HTTP/3 the
  WebTransport path already has, or exits 127 if that costs more than a thin
  adapter.
- Peers: quic-go, ngtcp2, quiche, and quinn, in both roles. Add the runner's
  WebTransport suite once the QUIC one is green.
- Keep it under `test/`, with a recipe that runs one pair and one case
  locally, and pin the runner and peer images so a peer's release cannot turn
  Nightly red on its own.
- A failure that is ours becomes a quest in the PR; one that is the peer's is
  reported upstream under the [interop audit](/quest/m3/audit-interop.md)'s
  standing approval for reproducer issues.
- The PR reports the matrix as first measured, and what the job costs
  Nightly.

Public API: none. Wire: none.

## Hosts

- linux - the runner drives Docker and network namespaces

## Related

- [Sim rule checks](/quest/m1/quic-sim-rules.md) - the same rules checked on our own seeds
- [Interop audit](/quest/m3/audit-interop.md) - the MoQ-level matrix; this is the QUIC layer beneath it
