# [S] io_uring replies from the address it was reached on

## Goal

An io_uring relay bound to a wildcard address on a multihomed host replies
from the local address each packet arrived on, as the tokio backend does,
so a client reaching a secondary or floating IP gets answers it accepts.

## Plan

The ring's sockets never enable `IP_PKTINFO` / `IPV6_RECVPKTINFO`;
`Endpoint` passes `None` as the destination IP to moq-quic
(`rs/moq-uring/src/quic/driver/endpoint.rs`), and `flush_one` builds
`udp::Transmit` with no source IP (`connection.rs`). Tokio passes
`meta.dst_ip` in and `src_ip` out. The documented relay bind is `[::]:443`,
so any host with more than one address is exposed.

Enable packet info on receive, carry the destination through the ring's
control-message parser into `Endpoint::handle`, and set the source on
transmit with `IP_PKTINFO` / `IPV6_PKTINFO`. Cover IPv4 and IPv6 wildcard
binds and a dual-stack socket with an IPv4-mapped peer, on a host with two
addresses (a dummy interface in the test namespace).

Decided 2026-10-22 from the io_uring audit: m1.

Public API: none. Wire: none.

## Related

- [A failed send never fails the socket](/quest/m1/uring-send-errors.md) - same socket
