# [M] A failed send never fails the io_uring socket

## Goal

A send error on one io_uring worker's shared UDP socket costs at most that
packet or that destination, as on the tokio backend, so one peer can never
take down every connection on the worker. Only an error meaning the socket
itself is gone (EBADF and the like) still fails it. A test injects each
recoverable error class beside a healthy connection on the same socket and
proves the healthy one survives.

## Plan

Today `on_send` (`rs/moq-uring/src/udp.rs`) ignores ECONNREFUSED, routes
unreachable errors to their destination, and falls back from GSO on
EIO/EINVAL; every other error (EPERM, ENOMEM, EMSGSIZE, a short send) calls
`fail_tx`, which is sticky, so every later `poll_acquire` fails and each
connection's `flush_one` dies with it. The relay has one socket per worker
and keeps accepting connections that are already dead. EPERM is the
realistic trigger: a netfilter drop or a full conntrack table, which a
spoofed-source flood can fill, and the endpoint's stateless replies to
strangers share the socket.

- Classify like tokio's `send_failed` (`rs/moq-tokio/src/quic/driver/runtime/mod.rs`):
  per-packet errors are loss, logged at a bounded rate; unreachable errors
  stay per destination; only an error that means the socket itself is gone
  (EBADF and the like) fails it. Settle the exact split from the errno list,
  not by porting tokio's line by line.
- Set `IP_PMTUDISC_PROBE` / `IPV6_PMTUDISC_PROBE` on the ring's sockets, as
  `moq-sock/src/udp/unix.rs` does for tokio, so a forged ICMP
  fragmentation-needed cannot shrink a peer's path MTU below a GSO train and
  turn its next send into EMSGSIZE or a worker-wide GSO fallback. On IPv6
  also set `IPV6_DONTFRAG`, as moq-sock does: Linux's IPv6 output path can
  still fragment under `IPV6_PMTUDISC_PROBE` alone, and MTU discovery needs
  oversized probes dropped, not fragmented. moq-sock tolerates a missing
  option by marking the socket as fragmenting; moq-uring is Linux-only,
  where both exist, so a failure to set either fails socket construction.
- Receive errors: anything but ENOBUFS and ECANCELED fails the endpoint;
  match tokio's tolerance (ConnectionReset, EMSGSIZE) in the same change.
- Test by injecting completion results into the socket's send path, since
  the kernel rarely produces them on demand.
- The `rs/moq-uring/README.md` claim that GitHub runners skip the suite is
  stale: `uring.yml` (release gate) and the nightly `rs uring` job run it on
  `ubuntu-24.04-arm` with `MOQ_STRICT=1`. Fix it here.

The worker-wide GSO fallback on EIO/EINVAL stays: `moq-sock` does the same,
and with `PMTUDISC_PROBE` set a peer can no longer trigger it.

Decided 2026-10-22 from the io_uring audit: m1, since a peer can trigger it.

Public API: none. Wire: none.

## Related

- [io_uring MTU parity](/quest/m1/uring-mtu.md) - same socket; PMTU probing lands there
