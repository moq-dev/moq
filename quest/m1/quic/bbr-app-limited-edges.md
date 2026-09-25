# [S] Mark BBR starvation wherever the source runs dry

## Goal

BBR hears application starvation whenever the sender runs out of data, not
only on a fully empty poll, and a local sender cap never masquerades as
starvation. Receiver flow control stays application-limited, and a test
pins that policy.

## Plan

The app-limited fix notifies `Controller::on_app_limited(in_flight)` only
when a transmit poll sends nothing. In moq-dev/noq
`noq-proto/src/connection/mod.rs`:

- A poll that sends some packets and then runs dry is not marked. Mark it,
  as Linux does when its write queue empties
  ([`tcp_rate_check_app_limited`](https://github.com/google/bbr/blob/90210de4b779d40496dee0b89081780eeddf2a60/net/ipv4/tcp_rate.c#L217-L233)).
- The local `send_window` (the sender's own cap on unacked data,
  `streams/state.rs` `write_limit`) reads as starvation because blocked data
  never reaches the transport. It is a local limit, like a full Linux
  sndbuf, which is not application-limited there; stop counting it.
- Receiver credit (`MAX_DATA`, `MAX_STREAM_DATA`) keeps counting as
  application-limited, matching
  [QUICHE](https://github.com/google/quiche/blob/535a2730e77d47e0dc03746555cc9c34b17bc9e9/quiche/quic/core/quic_session.cc#L952-L955).
  Linux and draft-06 section 4.1.1.3.1 treat receive windows as bottlenecks
  instead; for MoQ a peer's credit window is configuration, not path
  capacity. Document the choice on the trait and add a test that pins it.

Transport-boundary tests through the real callbacks: a partial poll that
drains, a sender blocked only by `send_window`, and a stream blocked by
receiver credit. Stacks on the seven fixes' noq branches until they merge;
does not gate [the BBR release](/quest/m1/quic/bbr-release.md). No public
API or wire change is intended.

## Related

- [Release BBR fixes](/quest/m1/quic/bbr-release.md) - includes the first app-limited fix this extends
- [BBR3 app-limited](/quest/m2/quic-bbr-app-limited.md) - measures natural draining on the corrected controller
