# moq-uring

Experimental Linux io\_uring support for the native MoQ stack. The crate does not
expose a public API yet. Its disposable benchmarks validate the UDP primitives
needed by a QUIC transport before those primitives become production
abstractions.

## Tokio/epoll baseline

`udp_tokio` runs a Tokio current-thread runtime, which uses epoll on Linux. It
transfers fixed bursts between connected loopback sockets and independently
toggles:

- one `recvmsg` per receive operation or batched `recvmmsg`;
- `UDP_GRO` receive coalescing;
- `sendmmsg` without GSO or `sendmsg` with a `UDP_SEGMENT` control message for GSO.

## io\_uring comparison

`udp_uring` uses the same payload, burst, GRO, and GSO settings. Without GSO it
submits 32 `IORING_OP_SENDMSG` entries as one batch. With GSO it submits one
`IORING_OP_SENDMSG` carrying a `UDP_SEGMENT` control message. Receive uses one
persistent multishot `IORING_OP_RECVMSG` request and a registered provided-buffer
ring.

Each completion is parsed with `RecvMsgOut`, including the `UDP_GRO` control
message, so the logical datagram count is checked rather than inferred from the
buffer size. The benchmark fails immediately if the kernel rejects the ring,
GSO, GRO, or any byte and datagram count.

Run either backend, or run both with identical Criterion arguments:

```bash
just rs bench-udp-tokio
just rs bench-udp-uring
just rs bench-udp --sample-size 20 --measurement-time 2 --warm-up-time 1
```

Every configuration moves the same number of 1280-byte datagrams in 32-packet
bursts. The receiver socket reserves four bursts of memory and drains each burst
before the next one. A one-second deadline turns any remaining packet loss into
an explicit benchmark failure instead of an indefinite wait.

The complete io\_uring benchmark requires Linux 6.1 or newer. Multishot
`recvmsg` arrived in Linux 6.0, registered provided-buffer rings in Linux 5.19,
and the deferred-task-run setup flag in Linux 6.1. The ring uses that flag and
the single-issuer flag because the benchmark and the intended relay shard both
have one thread driving each ring. It does not use `SQPOLL`.

This is deliberately below quiche. It establishes the syscall and completion
ceiling, but does not decide whether the relay is faster. That requires the same
matrix around quiche at a fixed offered load, reporting CPU per message and
latency on the benchmark rig rather than loopback throughput alone.
