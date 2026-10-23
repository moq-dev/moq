# Migrating moq-quic

How to update your code for each breaking change, newest first.

## Unreleased

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
