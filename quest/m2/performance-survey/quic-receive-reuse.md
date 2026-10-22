# [M] QUIC receive state recovers after bursts

## Goal

Long-lived QUIC connections do not retain unjustified reassembly metadata
from completed streams, without losing the benefit of cheap stream reuse.

## Plan

[Stream recycling](/rs/moq-quic/src/connection/streams/state.rs) puts freed
receive state in `free_recv`. [Assembler reset](/rs/moq-quic/src/connection/assembler.rs)
preserves its deque allocation and clears entries. Payload release therefore
does not release historical chunk capacity. Relay connections that once
received many fragmented streams are the consumer; this is separate from
unread payload credit in [receive memory](/quest/m1/quic/receive-memory.md).

Sweep connections × peak concurrent streams × fragments, followed by stream
completion and an idle phase. Include ordered traffic, gaps/reordering,
cancellation, and another burst. Measure live allocated metadata, recycled
state counts/capacities, allocation rate, CPU and latency when traffic resumes.
Distinguish application-owned allocations from allocator-resident pages.
Any reclamation must preserve stream credit and avoid repeated allocation
thrashing. Coordinate with the receive-memory implementation as it lands.

Follow the [survey benchmark contract](/quest/m2/performance-survey/README.md):
baseline first, paired evidence, equivalent work and nightly coverage. A
measured no-win is valid. No API or wire change is expected; report actual impact.
