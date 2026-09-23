# [S] Send batching across connections

## Goal

A measured verdict on batching sends across connections. The tokio path sends
one `sendmsg` per GSO train per connection; a worker flushing many small
connections pays a syscall each. `sendmmsg` submits every ready train in one
call. The io_uring path already queues one SQE per train and submits them
together, so the question there is whether it already gets the benefit.

## Plan

- Add `sendmmsg` to `moq-noq-udp` behind the existing GSO path: the runtime
  collects the trains ready across connections in one poll and submits them
  together, falling back to per-train `sendmsg` on partial failure the way
  the GSO fallback does.
- Measure syscalls per second and CPU per Gbps on the chat and fanout shapes
  with many connections, tokio workers versus io_uring workers, before and
  after. Confirm from the io_uring worker's `enters` counter that the ring
  already amortizes the same way.

A measured no-win abandons the tokio change; the io_uring result is recorded
either way.

## Related

- [Kernel pacing](/quest/m2/quic-kernel-pacing.md) - pacing per train
  changes what a batch can contain
