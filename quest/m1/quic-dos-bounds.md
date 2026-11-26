# [S] Bound unauthenticated Initial buffering, sparse stream-ID allocation, CID exhaustion, and stateless-reset rate limiting

## Goal

Hostile UDP traffic cannot exhaust memory or stall connections through `moq-quic`'s endpoint or stream tables: spoofed Initials are bounded/authenticated before filling `IncomingBuffer`, a single sparse `STREAM` frame cannot eagerly allocate up to `max_streams` hash-map slots, `Endpoint::cids_exhausted()` accounts for `HashedConnectionIdGenerator`'s 24-bit nonce space, and a single off-path spoofed source cannot suppress stateless resets for all legitimate peers.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan` of the `moq-quic` audit): ✅ plan all verified audit bugs, latency, CPU, and coverage work (rejected: bugs and real-time latency only; drop). Joins `m1` as additive hardening before the 0.22 cut.

Issues found in the audit:

- **Unauthenticated Initial buffering (`endpoint.rs:415-520`, `config/server/mod.rs:118`)**: `handle_first_packet` allocates `IncomingBuffer` (default `max_incoming = 65536`, up to 100 MiB total) and inserts into `connection_ids_initial` *before* AEAD-authenticating the Initial packet (which waits until `start_accept`). Authenticate Initial packets (or reject invalid AEAD) before buffering/queueing `Incoming`, and also handle the empty-DCID + token retransmit case (`endpoint.rs:436-441, 1146-1152`) without spawning duplicate `Incoming` objects.
- **Sparse stream-ID eager `HashMap` insertion (`streams/state.rs:1326-1345`)**: Receiving a single `STREAM` frame at `id = max_streams - 1` loops `next_remote..=id.index()` inserting `None` entries into `self.recv` (and `self.send` for bidi). Track implicitly opened remote streams lazily (a watermark + set of opened/closed streams, or a bounded window) so one 10-byte packet cannot allocate tens of MB when `max_streams` is high, and ensure `StreamsState::quiesce` (`state.rs:539`) checks `unaccepted` before allocating `Box<Recv>`.
- **`HashedConnectionIdGenerator` 24-bit exhaustion (`cid_generator.rs:232-265`, `endpoint.rs:401-412, 1064-1077`)**: `cids_exhausted()` only checks `cid_len <= 4`, missing `HashedConnectionIdGenerator` (`cid_len == 8`, only 24-bit nonce = 16.7M CIDs), which would spin forever in `Endpoint::new_cid()`. Either widen the nonce/entropy or let `ConnectionIdGenerator` report its entropy bits to `cids_exhausted()`, and check `cid.len() == NONCE_LEN + SIGNATURE_LEN` in `validate()`.
- **Global stateless-reset suppression (`endpoint.rs:277-283`)**: `last_stateless_reset` is a single endpoint-wide timestamp, so 1 spoofed UDP packet every 20 ms suppresses stateless resets for every peer after a server restart. Replace with a small bounded token bucket or per-prefix/hashed-IP limiter, and clear `buf` consistently across `Endpoint::handle` response helpers.

Public API: none. Wire: none.
