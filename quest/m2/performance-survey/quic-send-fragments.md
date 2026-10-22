# [M] Fragmented QUIC sends cost proportional to the bytes touched

## Goal

Native relay egress avoids repeated traversal of unrelated retained write
chunks, if benchmarks establish a material cost.

## Plan

At the audit revision, [SendBuffer::get](/rs/moq-quic/src/connection/send_buffer.rs)
starts at the first unacknowledged segment for each lookup (lines 146-162).
[Packetization](/rs/moq-quic/src/connection/streams/state.rs) calls it again
for every piece of a range. N small retained segments can incur a triangular
number of visits while sending sequentially; delayed ACKs or a prefix hole
keep earlier segments in the scan.

Extend `send_write_size` in the QUIC stream benchmarks. Sweep write chunk
size against retained flight size independently, with fixed total payload
controls, normal sequential sending, old-offset retransmission and ACK holes.
Measure segment visits, instructions/byte, CPU, allocations and retained memory.
The existing 64 KiB stream case does not isolate retained-flight growth.
Confirm any win through a relay with publishers × viewers and finite-queue
bufferbloat using a real congestion controller. Preserve ACK, retransmission,
reset and truncation semantics. An offset index or cursor is a candidate,
not a predetermined design.

Related: [egress profiling](/quest/m2/quic-egress-profile.md) owns syscall,
copy and pacing attribution; this quest owns segment lookup complexity.

Follow the [survey's benchmark contract](/quest/m2/performance-survey/README.md):
measure the baseline before optimizing, retain paired evidence, verify equivalent
work, schedule coverage nightly, and accept a measured no-win. Expected public
API and wire impact: none; report the actual impact in the implementation PR.
