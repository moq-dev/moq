# Migrating moq-quic

How to update your code for each breaking change, newest first.

## Unreleased

### Early data is removed

Wait for `Event::Connected` before opening streams or sending application data.
`Connection::has_0rtt`, `accepted_0rtt`, and the `crypto::Session::early_crypto` and `early_data_accepted` hooks are gone.
Custom crypto sessions no longer implement those hooks.
Rustls configs disable early data, including caller-supplied configs, while session tickets still resume 1-RTT handshakes.

### QUIC APIs follow their consumers

`RecvStream::read()` and `Chunks::next(max)` always return ordered bytes.
Drop the ordering flag and `Chunk::offset`; track bytes consumed if an application needs its own offset.
`ReadableError::IllegalOrderedRead` is gone.
Use `Connection::set_limits` with `Limits` instead of `set_max_concurrent_streams`, `set_send_window`, and `set_receive_window`.
`Limits::max_streams` sets both directions, so code that raised only `Dir::Uni` now raises the bidirectional limit too.
Each field is a `NonZeroU64`, and `None` leaves that limit unchanged, so a live connection cannot set a stream limit or window to 0; set a per-direction or zero stream limit at construction with `TransportConfig::max_concurrent_uni_streams` and `max_concurrent_bidi_streams`.
Use inherent `VarInt::encode` instead of the removed public `coding` module and its `Codec` trait.
Nothing public decodes a `VarInt` or the other former `Codec` types (`StreamId`, `FrameType`, `TransportErrorCode`) anymore; parse them with your own decoder.

Packet parsers and headers, `HashedConnectionIdGenerator` (with `set_lifetime`), `BloomTokenLog`, `TokenMemoryCache`, token policy traits (`TokenLog`, `TokenStore`, and `TokenReuseError`), `NoneTokenLog`, `NoneTokenStore`, `ClientConfig::token_store`, and `ValidationTokenConfig::log` are internal.
Use `Endpoint` to parse incoming datagrams, `RandomConnectionIdGenerator` or a consumer's `ConnectionIdGenerator`, and the default token policy.
`TimeSource`, `StdSystemTime`, and `ServerConfig::time_source` are gone; address validation uses `kio::clock::wall()`, including an installed simulated clock.
Unused `Connection::local_ip`, `min_rtt`, `has_pending_retransmits`, `ping`, `force_key_update`, `path_changed`, and `max_concurrent_streams` accessors are no longer public.
`Endpoint::set_server_config`, `incoming_buffer_bytes`, and `Incoming::orig_dst_cid` are also internal or removed; configure the endpoint at construction.
To rotate TLS certificates on a running endpoint, give the `ServerConfig`'s rustls config a `ResolvesServerCert` that returns the current certificate, as moq-tokio's `tls::Serve` does.
Use `Connection::stats` for RTT metrics and receive metadata for local addresses.

### `mux` is an internal integration surface

The `mux` module is hidden from public API documentation; use the `qmux` crate for reliable-transport sessions. It remains public only so qmux can share the stream state machine.

Each entry is a change from `quinn-proto`.

### `StreamEvent::Acked` (#5817)

`StreamEvent` gained `Acked { id, offset }`, reported when the answer changes for the lowest offset a `SendStream::acked` call is waiting for: the stream's acknowledged prefix reached it, or `STOP_SENDING` or a reset made it fail.
An exhaustive `match` needs an arm for it.

```rust
// After
StreamEvent::Acked { id, offset } => wake_ack_waiters(id, offset),
```

### Streams and datagrams take `SendOptions` (#5770, #5872)

`Streams::open` and `Datagrams::send` take a `SendOptions { group, priority }`,
the send group from `Connection::send_groups` and the priority within it.
`SendOptions::default()` is ungrouped at priority 0, where `open` used to start a stream.
A datagram is scheduled with the streams instead of ahead of them, so `send` loses its
`drop` flag: it never blocks, and a full `datagram_send_buffer_size` drops the oldest
datagram of the lowest-priority group.

```rust
// Before
let id = conn.streams().open(Dir::Uni).unwrap();
conn.datagrams().send(data, true)?;

// After
let id = conn.streams().open(Dir::Uni, SendOptions::default()).unwrap();
conn.datagrams().send(data, SendOptions { group: Some(&group), priority: 0 })?;
```

`SendDatagramError::Blocked` and `Event::DatagramsUnblocked` are gone, since sending
never blocks, and `SendDatagramError` is `#[non_exhaustive]`.

### `SendStream::set_priority` takes an `i64`

`set_priority` and `priority` use `i64` instead of `i32`, the range of a send group's
priority. Higher still goes first.

### `fuzzing` module and `arbitrary` feature removed

The `cfg(fuzzing)` `fuzzing` module and the `arbitrary` feature are gone. The fuzz
targets run through the hidden `fuzz` module behind the `fuzz` feature; see
[`fuzz/`](fuzz/README.md).

### `congestion::NewReno` removed

`congestion::NewReno` and `congestion::NewRenoConfig` are gone.
Use `Bbr3Config` (the default) or `CubicConfig` instead.
