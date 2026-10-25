# [M] moq-quic has no 0-RTT

## Goal

moq-quic, both QUIC drivers, and `quic::web` have no 0-RTT: a client never
sends early data, a server never accepts it, and `into_0rtt`, `is_0rtt`,
`accepted_0rtt`, `has_0rtt`, `ZeroRttRejected`, and the rejection paths in
the stream state are gone.

## Plan

Decided 2026-10-23 (maintainer): delete it entirely. MoQ cannot use 0-RTT:
a WebTransport CONNECT and MoQ's authorization both need the handshake to
finish before any request is acted on, and nothing in the repository or its
quests uses it. Today it costs `ZeroRttRejected` in every stream error, four
`unreachable!("0-RTT not supported")` arms in moq-tokio's `quic::web`, and
`zero_rtt_rejected` paths that drop streams (one of them loses
`StreamEvent::Acked` waiters).

A server configures rustls with early data off, so a client's 0-RTT packets
are discarded as the protocol allows, and the 0-RTT packet space, keys, and
transport-parameter checks for remembered parameters go. Keep session
resumption itself if rustls tickets still shorten the 1-RTT handshake; only
early data goes. Add a test that a peer's 0-RTT packets are dropped without
closing the connection.

The Impact bullet says how to migrate in moq-quic, moq-tokio, and moq-uring.
