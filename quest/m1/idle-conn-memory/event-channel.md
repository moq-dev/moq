# [S] A QUIC connection's event channel stops holding 12 KiB

## Goal

An idle QUIC connection's endpoint-to-connection event channel holds about
1 KiB instead of 12.6 KiB, with no extra allocation per received datagram
on a busy connection.

## Plan

`moq-tokio`'s endpoint gives each connection an
`mpsc::unbounded_channel::<ConnectionEvent>()`
(`rs/moq-tokio/src/quic/driver/endpoint.rs`, `ConnectionSet::insert`) and
sends every datagram it routes there as a `ConnectionEvent::Proto`, which
carries moq-quic's `DatagramConnectionEvent` inline (two `Instant`s, the
remote address, a `PartialDecode`, the remaining `BytesMut`). That makes an
event about 220 bytes. tokio's list channel allocates 32 slots per block and
keeps a drained block for reuse until the channel drops, so each connection
holds a 7 KiB block from the moment it is accepted, and 0.8 more on average
once a burst crosses a block boundary. The 2026-11-27 idle profile
([epic](/quest/m1/idle-conn-memory/README.md)) measured 6.8 KiB at channel
creation and 5.7 KiB from `Tx::find_block` growth per connection.

Reclaim the idle storage rather than boxing each event: the endpoint enqueues
one event per routed datagram, so a box per event adds an allocation to every
receive on a busy relay. Replace tokio's block list with a per-connection
queue that reuses its storage under load and releases it once drained, or
that starts small and grows, and confirm with the `fanout-heavy` heap profile
that its churn does not rise.
