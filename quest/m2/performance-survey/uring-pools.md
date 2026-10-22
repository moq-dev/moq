# [L] UDP pool memory follows useful concurrency

## Goal

The io_uring relay balances burst throughput, growth latency and memory
after bursts using measured pool policy.

## Plan

[UDP pools](/rs/moq-uring/src/udp.rs) grow synchronously on starvation and
never shrink. Each receive completion consumes a whole roughly 64 KiB slot,
even for a small datagram. Defaults start at roughly 1 MiB RX and 4 MiB TX
capacity and permit roughly 16 MiB RX plus 64 MiB TX per socket, excluding
metadata and kernel queues. These are capacity figures, not measured RSS or
eager allocation of the ceiling. More worker sockets multiply the exposure.

Sweep publishers × subscribers, worker sockets, packet sizes and observed
GRO aggregation through cold start, burst, quiet and repeat-burst phases.
Record pool occupancy/capacity, live allocation bytes, touched/resident pages,
faults, growth duration, starvation, CQ overflow, CPU and tail latency. Compare
initial depth, growth and safe reclamation only after attributing the cost.
Measure sparse small packets as well as large coalesced receives.

Preserve kernel ownership until completion and all outstanding packet leases.
The provided-buffer descriptor ring does not imply all payload pages are
fixed registered memory. Keep worst-case GRO capacity: incremental buffers
were deliberately rejected because a short multishot `recvmsg` tail faults.
Coordinate with [QUIC I/O ownership](/quest/m2/quic-io-boundary.md), which may
change how long receive buffers remain borrowed.

Follow the [survey benchmark contract](/quest/m2/performance-survey/README.md):
baseline first, paired evidence, equivalent work and nightly coverage. A
measured no-win is valid. Do not add public tuning knobs before their consumer
benefit is demonstrated; report actual API and wire impact.
