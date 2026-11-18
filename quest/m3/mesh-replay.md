# [L] Real relays replay a mesh and its faults

## Goal

One recipe starts a `moq-relay` process for each relay of a mesh read from a
file, joins them over links that queue until they drop, and plays a script
of faults against it while publishers and subscribers read through:

- a link reopening, and one relay reopening every link it has, twice;
- a relay killed and restarted;
- a roll that drains each relay with GOAWAY and replaces its binary, one
  relay at a time, from the last published release to this checkout and back.

It fails when a reader with a route to its broadcast stays unfed, a
broadcast stays shown after its source ended, or a relay does not go quiet
once the script does. For each event it reports the route and announce
messages and bytes, the time until the mesh is quiet, the broadcasts
rewritten into the origin, and each reader's interruption.

Nightly runs it on a fixed mesh whose routes cross three links. A deployment
replays its own graph by passing its own file.

## Plan

Decided 2026-11-18 (maintainer, in the coordinating session, from a survey of
the moq.pro deployment): ✅ planned (rejected: leaving it out). What it adds
to the tests that exist:

- The route simulator and its sweeps run generated shapes over mock
  sessions. The drills inject one fault over real sockets into a relay or a
  pair. `just test wire-compat` and the cut's two-relay smoke pair one
  released relay with one from this checkout.
- moq.pro rolls one relay at a time, so its mesh spends hours with the
  newest and previous release meshed, and lite-07 is chosen link by link:
  only two new relays speak it to each other. Nothing runs a mesh in that
  state, where one path crosses both versions.
- The same decision keeps the per-broadcast rewrite of
  [route benches](/quest/m3/route-benches.md) in m3 on the condition that it
  is measured on a deployed mesh's shape before that mesh's cluster links
  move to lite-07. This quest's report is that measurement.

Agent's choices, for the implementer to revisit:

- m3, as work a downstream deployment waits on. It does not gate the lite-07
  cut. A flaw it finds in the wire is still fixed before the cut if the cut
  has not happened.
- Reuse the drills' shaper for the links and `test/interop/compat.sh`'s
  download of the released relay.
- The file names relays and links, each link with its cost and whether
  either end marks it. The script is a list of timed events, so two runs
  share one sequence and their reports compare.
- Sweep the report over both axes: relays in the mesh and broadcasts per
  relay.
- The counts are a report, not a gate, as benchmarks are (2026-10-20,
  maintainer).
- A pair of edges joined over lite-06 can still transit, a known gap of the
  lite-06 path that ends when the mesh finishes its roll. Name it in the
  report instead of failing on it.

Public API: none. Wire: none.

## Required

- [A relay counts what its route layer does](/quest/m3/route-counters.md) - the counts the report reads from each relay

## Related

- [Route benches](/quest/m3/route-benches.md) - the costs this measures on a real mesh
- [The cases no test covers run on real sessions](/quest/m2/multicast/accept.md) - the same failures on mock sessions with mocked time
- [Random fault schedules](/quest/m4/cluster-fault-schedules.md) - searches fault timings on the simulated clock
- [A nightly soak](/quest/m4/relay-soak.md) - one real relay under churn
