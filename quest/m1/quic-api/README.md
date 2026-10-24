# moq-quic's API improves on quinn's

## Goal

Before the next cut publishes them for the first time, moq-quic,
`moq_tokio::quic::driver`, and moq-uring's `quic` module ship one mirrored
API that is smaller and harder to misuse than quinn's: namespaced names, one
error type per direction, terminal operations that consume their handle,
plain config structs, and no quinn compatibility baggage. Consumers lose the
workarounds quinn's shape forced on them.

Non-goals: the congestion controller API (redesigned in another quest), a
buffer pool ([QUIC I/O boundary](/quest/m2/quic-io-boundary.md)), and the
non-QUIC backends' move off `web_transport_trait`
([drop web-transport-trait](/quest/m2/drop-web-transport-trait.md)).

## Plan

Planned 2026-10-23 from two read-only audits of HEAD 8e8bd3faf2: moq-quic and
the driver against quinn at the fork point (`7616e6b2`), and the
correctness of the carried features. Neither crate is on `release` (it still
ships noq), so every break here is free until the cut.

Decided with the maintainer:

- Scope is moq-quic plus both drivers, mirrored; the drivers have already
  drifted.
- Rename freely: quinn is a reference, not an upstream we cherry-pick from by
  sed. The crate takes the workspace formatting first.
- An m1 epic of its own, linked from the
  [API audit](/quest/m1/api-audit/README.md), whose consumer rule it follows.
- The audit's bugs are standalone m1 quests ranked ahead of this epic.
- No docs page: rustdoc and each PR's Impact bullet carry it.
- A third read-only audit listed the workarounds quinn's shape forced on
  moq-tokio, moq-uring, qmux, and moq-relay; each child names the ones its
  change deletes. The maintainer pulled the driver's poll forms and native
  WebTransport into m1 from m2, and deleted 0-RTT entirely: MoQ cannot use
  it, since CONNECT and authorization need the finished handshake.

This README's own work, after the children: read the three layers' rustdoc
side by side and fix any name or signature that still differs, and check
`doc/lib/rs/index.md`'s driver line still holds.

## Required

- [Reformat](/quest/m1/quic-api/reformat.md) - moq-quic, moq-sock's `udp`, and the tokio driver take the workspace rustfmt; quinn's cherry-pick recipes go
- [Trim](/quest/m1/quic-api/trim.md) - every public item has a consumer; unordered reads, send domains, duplicate limit setters, unused features, and quinn-era docs are gone
- [Zero-RTT](/quest/m1/quic-api/zero-rtt.md) - no early data anywhere, and no 0-RTT API or error variants
- [Streams](/quest/m1/quic-api/streams.md) - `send::`/`recv::` namespaces, one error per direction, `finish(self) -> send::Finished`, consuming `reset`/`stop`, `Buf` writes, both drivers mirrored
- [Crypto](/quest/m1/quic-api/crypto.md) - crypto traits sealed to rustls, concrete peer identity and handshake data
- [Config](/quest/m1/quic-api/config.md) - plain config structs, `Limits` as the one spelling, one endpoint constructor and `connect` that own the socket, v1-only `Version`, a local-close event
- [Poll forms](/quest/m1/quic-api/poll.md) - both drivers' waits poll without leaking, the async forms wrap them, and `send::Finished` is a future
- [Native WebTransport](/quest/m1/quic-api/native-web.md) - `quic::web` in both crates implements moq-net's poll traits, deleting the QUIC paths of the async-to-poll adapter

## Related

- [Priority requeue](/quest/m1/quic-priority-requeue.md) - audit fix ranked ahead
- [Lease bookkeeping](/quest/m1/quic-lease-bookkeeping.md) - audit fix ranked ahead
- [poll_acked](/quest/m2/quic-ack-hook.md) - the m2 consumer that keeps `send::Stream::acked` in the API
