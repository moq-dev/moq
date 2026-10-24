# [L] Trim moq-quic and the drivers to what has a consumer

## Goal

Every public item of moq-quic, `moq_tokio::quic::driver`, and moq-uring's
`quic` module has a consumer in this repository, moq.pro, moq.dev, or an m1
quest, or it is private or deleted. Unordered reads, the duplicate limit
setters, unused features, and quinn-era docs are gone.

## Plan

Decided 2026-10-23 (maintainer): the api-audit consumer rule, with one
exception: `SendStream::acked` and `StreamEvent::Acked` stay for
[poll_acked](/quest/m2/quic-ack-hook.md).

Scheduling domains go (maintainer, 2026-10-23): `open_domain`,
`close_domain`, `open_in`, and `SendDomain` are `#[cfg(test)]`, yet the
scheduler runs the domain tier (its ring, deficit, hoards, and per-domain
victims) on every pop and datagram push with only the default domain.
Delete the tier from `streams/schedule.rs`, keeping groups' deficit round
robin; [broadcast fairness](/quest/m2/broadcast-fairness.md) re-adds it
with its consumer. Report the pop and datagram-push cost before and after
on `benches/streams.rs`.

Re-check each item against consumers before removing it; this list is from
the 2026-10-23 audit, which found no consumer outside the crate and driver:

- Packet parsing: `ConnectionIdParser`, `FixedLengthConnectionIdParser`,
  `PartialDecode`, `ProtectedHeader`, `ProtectedInitialHeader`, `LongType`,
  `PacketDecodeError`.
- `TimeSource` and `StdSystemTime` (it reads `moq_clock::wall()`, which tests
  already mock), `HashedConnectionIdGenerator`, `BloomTokenLog`,
  `TokenMemoryCache` and the token traits where only defaults are used.
- `pub mod coding`: qmux uses it only to encode a `VarInt`; give `VarInt`
  that method instead.
- Named future types (`Accept`, `AcceptBi`, `AcceptUni`, `OpenBi`,
  `OpenUni`, `ReadDatagram`, `IncomingFuture`, the read/write futures)
  become `async fn` or `impl Future`.
- Driver methods: `force_key_update`, `export_keying_material`, `side`,
  `local_ip`, `min_rtt`, `open_connections`, `rebind_abstract`,
  `read_chunks`, `poll_read_buf`, and the unused `Incoming` methods
  (`retry`, `ignore`, `may_retry`, `remote_address_validated`,
  `orig_dst_cid`, `accept_with`). Keep any that moq-uring or the backends
  call.
- The per-limit setters (`set_max_concurrent_uni_streams`,
  `set_max_concurrent_bi_streams`, `set_send_window`, `set_receive_window`)
  in moq-quic and the tokio driver; `set_limits` is the one runtime path.
  The fuzz target moves to `set_limits`.

Unordered reads go entirely: `RecvStream::read(ordered: bool)` and the
driver's `read_chunk(max, ordered)` lose the flag, and `Chunk.offset`,
`ReadableError::IllegalOrderedRead`, and the assembler's unordered mode are
deleted. The driver's `read_to_end` is their only caller and reads ordered.
Two of the upstream fixes ported so far were unordered-mode bugs.

Moved here from [transport config](/quest/m1/api-audit/transport-config.md):
drop moq-quic's unused features (`rustls-ring`, `tracing-log`, `bloom`,
`platform-verifier`, fips, log) and the "backwards compat" `rustls` alias;
gate `pub use rustls` on `any(ring, aws-lc-rs)` so it exists under
`rustls-aws-lc-rs`.

Docs that still describe quinn: the crate docs (`lib.rs`, "the futures-based
quinn API" and C bindings) point at `moq_tokio::quic::driver`; "Quinn" in
`config/mod.rs`, `crypto.rs`, `cid_generator.rs`, `config/transport.rs`, and
`crypto/rustls.rs`; the driver's "Its API mirrors quinn's". The driver mutex
no longer tracks lock time, so its `lock("purpose")` strings go.

Each removal gets a `MIGRATE.md` entry in its crate.

## Required

- [Reformat](/quest/m1/quic-api/reformat.md) - lands the final formatting first
