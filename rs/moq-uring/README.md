# moq-uring

Experimental Linux io\_uring support for the native MoQ stack: a
thread-per-core `Worker` that owns a `SINGLE_ISSUER | DEFER_TASKRUN |
COOP_TASKRUN` ring, a userspace timer heap, a local (`!Send`) task set, and
the UDP sockets bound through it.

- **Receive**: one persistent multishot `recvmsg` per socket, fed from a
  registered provided-buffer ring of worst-case-sized buffers (one per
  completion), with `UDP_GRO` coalescing. Received packets borrow the pool and
  hand the space back on drop, which is also the receive-side backpressure.
  Incremental consumption (`IOU_PBUF_RING_INC`) cannot back a multishot
  `recvmsg`: the kernel faults the receive once a buffer's leftover tail is
  smaller than the recvmsg header.
- **Send**: `sendmsg` with an explicit `UDP_SEGMENT` control message per call,
  staged in a fixed pool of buffers owned by id and released on completion
  (the shape a later `SENDMSG_ZC` needs).
- **Timers**: a heap the worker sweeps; the earliest deadline rides
  `io_uring_enter` as an absolute timeout. Zero timeout SQEs. The worker's
  `Handle` implements `moq_net::Timers`.
- **Parking**: a futex word per worker. Remote wakes are an atomic store, plus
  one `futex(2)` wake only while the worker is actually parked (a `FUTEX_WAIT`
  SQE armed on the word).

Requires **Linux 6.12**; `Worker::new` refuses older kernels with a legible
error rather than degrading (note that default container seccomp policies
block io\_uring entirely). There is no fallback here: older kernels keep using
the tokio stack.

## Validation

`tests/echo.rs` runs a raw [quiche](https://github.com/cloudflare/quiche) echo
over the worker: handshake, half a megabyte each way, timers driven by
quiche's own timeout. It skips (loudly) below the kernel floor, which includes
GitHub-hosted CI runners.

## Benchmarks

`udp_tokio` and `udp_uring` are the disposable syscall-level matrices from the
first spike (recv batching x GRO x GSO, epoll vs io\_uring); see git history
for their methodology. `echo_quiche` is the ablation matrix over the real
worker: the same quiche echo with receive batching, GRO, and GSO toggled one
at a time.

```bash
just rs bench-udp --sample-size 20 --measurement-time 2 --warm-up-time 1
just rs bench-echo
```
