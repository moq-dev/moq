# Migrating moq-quic

How to update your code for each breaking change, newest first.

## Unreleased

### `SendStream::checkpoint` takes `&mut self`

A checkpoint now registers to learn when the peer acknowledges the data before it, read with `Checkpoint::acked`.
Callers holding a shared borrow of the `SendStream` need a mutable one.

```rust
// Before
let checkpoint = stream.checkpoint(); // stream: &SendStream

// After
let checkpoint = stream.checkpoint(); // stream: &mut SendStream
```

### `StreamEvent::Acked`

`StreamEvent` gained `Acked { id }`, reported when a `Checkpoint` on that send stream was acknowledged or never will be.
An exhaustive `match` needs an arm for it.

```rust
// After
StreamEvent::Acked { id } => wake_checkpoint_waiters(id),
```

### `Datagrams::send` takes a send group and order (#5770)

`Datagrams::send(data, group, order)` replaces `send(data, drop)`; pass `None` for an ungrouped datagram.
A full buffer drops the oldest datagram of the lowest-priority group, so `SendDatagramError::Blocked` and `Event::DatagramsUnblocked` are gone.

### `congestion::NewReno` removed

`congestion::NewReno` and `congestion::NewRenoConfig` are gone.
Use `Bbr3Config` (the default) or `CubicConfig` instead.
