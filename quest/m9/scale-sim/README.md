# Audience-scale simulation

## Goal

One command on a laptop simulates a large live event on virtual time: a
publisher, a cluster of real `moq-relay`s, and as many viewers as the laptop
fits, each a real simulated-QUIC session running the real moq-net code, over
links that queue until they drop (bufferbloat). It reports each relay's cost
(instructions, memory, bytes) and the viewers' latency and stall percentiles,
so a fan-out change is benchmarked at audience scale. It runs nightly on
`main` and on demand locally, never on the PR gate.

Not included: finding correctness faults
([random fault schedules](/quest/m4/cluster-fault-schedules.md) owns that),
and load against real hosts (`moq-bench` and
[the nightly soak](/quest/m4/relay-soak.md) own that).

## Plan

Decided 2026-11-25 (maintainer, /quest-plan from a brainstorm of outlandish
improvements): ✅ an audience-scale fan-out benchmark on virtual time
(rejected: folding it into the fault schedules, whose seeds and gate differ;
drop it). ✅ Every viewer is a real simulated-QUIC session with a cheap
test-only cipher (rejected: sampled edges, where a few edge relays carry real
viewers and the rest one stand-in each; viewers as in-process moq-net
sessions without QUIC, which skips the per-session cost that matters most at
the edge). ✅ No fixed viewer target: the scenario runs at the laptop's
measured ceiling, which grows as per-viewer cost drops (rejected: 1M real
viewers on a large self-hosted runner; going back to sampled edges).

The building blocks exist: `kio::clock::sim` runs tasks on virtual time,
`moq_quic::sim::Link` carries datagrams over seeded, impaired links, and
`moq_tokio`'s `sim::Network` routes dials over them (the `test-support`
feature). `moq-quic` already has a plaintext crypto behind its `fuzz` feature.

[What a simulated viewer costs](/quest/m9/scale-sim/viewer-cost.md) comes
first, then [the event scenario](/quest/m9/scale-sim/event.md) runs at the
ceiling it measures.
