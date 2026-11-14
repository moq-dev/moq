# [L] Random fault schedules run against a simulated relay cluster

## Goal

Nightly runs seeded schedules of faults against relays, publishers, and
subscribers over real QUIC on the simulated clock: partitions that heal,
relays killed and restarted, links whose bandwidth collapses behind a deep
queue, and clients that vanish. Invariants are checked throughout and once
traffic stops, a failing seed replays exactly, and a fixed set of seeds runs
on the merge gate if it stays fast.

## Plan

Decided 2026-11-13 (maintainer, /quest-plan on what testing to add): the drills (`rs/moq-relay/tests/drills.rs`) inject one fault at a chosen
moment over real sockets, and `mesh_sweep.rs` randomizes meshes over the
in-memory transport. Nothing searches fault timings over the relay's own code
and QUIC together, which is how FoundationDB, TigerBeetle, and Antithesis
find the bugs scripted tests miss.

- ✅ m4 (rejected: m1): it cannot start before the relay runs on the sim.
- ✅ The simulated soak lives here (rejected: only the
  [real-process soak](/quest/m4/relay-soak.md)): after the schedule ends and
  the survivors go quiet, every table (routes, announces, sessions, cached
  groups past their age, streams, timers) returns to its baseline. Hours of
  simulated churn cost seconds, and a leak replays from its seed.

Agent's choices, for the implementer to revisit:

- Invariants during the run: no panic; a subscriber reads each group whole
  or gets an explicit drop or reset, never different bytes under the same
  identity; delivery resumes within a stated bound of a heal; one broadcast's
  faults never disturb another's.
- Faults follow `AGENTS.md`: bandwidth below the media bitrate behind a deep,
  finite queue, with a check that it overflowed. Random loss only for
  recovery correctness.
- Generate schedules with proptest so a failure shrinks to its shortest
  schedule, and print the seed.
- Prove the harness finds things: `test/drill/mutations`'s patches, applied
  one at a time, must each fail some seed within the nightly budget. Report
  which do not, since those are the drills this cannot replace.
- No turmoil or madsim: `kio::clock::sim` and `moq_quic::sim` already give a
  seeded scheduler and network with no shims.

Public API: none. Wire: none.

## Required

- [The relay over QUIC on the sim](/quest/m1/quic-sim.md) - relays and clients on one simulated `Network`

## Related

- [Sim rule checks](/quest/m1/quic-sim-rules.md) - QUIC's rules checked on the same runs
- [Route layer acceptance](/quest/m2/cluster-routes/acceptance.md) - the scripted partition and failover fixtures
