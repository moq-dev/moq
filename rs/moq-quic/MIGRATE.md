# Migrating moq-quic

How to update your code for each breaking change, newest first.

## Unreleased

### `Datagrams::send` takes a send group and order (#5770)

`Datagrams::send(data, group, order)` replaces `send(data, drop)`; pass `None` for an ungrouped datagram.
A full buffer drops the oldest datagram of the lowest-priority group, so `SendDatagramError::Blocked` and `Event::DatagramsUnblocked` are gone.

### `congestion::NewReno` removed

`congestion::NewReno` and `congestion::NewRenoConfig` are gone.
Use `Bbr3Config` (the default) or `CubicConfig` instead.
