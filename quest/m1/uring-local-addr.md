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
transmit with `IP_PKTINFO` / `IPV6_PKTINFO`.

Grow the control-message storage with it: `CONTROL_LEN = 64`
(`rs/moq-uring/src/udp.rs`) fits only GRO/GSO plus TOS/TCLASS. With packet
info the full set needs 80 bytes of `CMSG_SPACE` on IPv4 and 88 on IPv6
(64-bit Linux), so a receive would set `MSG_CTRUNC` and the parser would
drop the whole completion, and the unchecked send-side `Control` buffer
would overflow. Size `CONTROL_LEN` (and so `RECV_OVERHEAD`) from the
`CMSG_SPACE` sum, assert it at compile time, and test encoding and parsing
the full combination for both families.

Cover IPv4 and IPv6 wildcard binds and a dual-stack socket with an
IPv4-mapped peer, each reached on a second local address. IPv4 needs no
privilege: all of 127/8 is local, so 127.0.0.2 works. IPv6 has only `::1`,
so the `uring.yml` and nightly `rs uring` lanes add a second loopback
address (`sudo ip -6 addr add ... dev lo`) before the suite, and the test
skips loudly without it, refusing under `MOQ_STRICT` like the ECN test.

Decided 2026-10-22 from the io_uring audit: m1.

Public API: breaking. The source address reaches the send encoder
through `moq_uring::udp::Transmit`, which callers construct by literal, so
it gains a `src_ip: Option<IpAddr>` field and `#[non_exhaustive]` with a
constructor in the same change, with a `rs/moq-uring/MIGRATE.md` entry.
Wire: none.
