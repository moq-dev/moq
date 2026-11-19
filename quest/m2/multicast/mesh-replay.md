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
- The same decision kept the fix for the per-broadcast rewrite after the
  cut, now in [route wakes](/quest/m4/route-wakes.md), on the condition that
  the cost is measured on a deployed mesh's shape before that mesh's cluster
  links move to lite-07.
  [moq-dev/moq#9114](https://github.com/moq-dev/moq/pull/9114) benched it:
  10 origin writes per broadcast when an upstream reopens twice. This
  quest's report is that measurement on a real mesh.

Decided 2026-11-18 (maintainer, on
[moq-dev/moq#9110](https://github.com/moq-dev/moq/pull/9110)): ✅ it blocks
the lite-07 cut, under this epic (rejected: m3, starting now without the cut
waiting on it). After the cut the lite-07 wire is frozen, so a wire flaw
found on a real mesh mid-roll would wait for lite-08. A flaw it finds is
fixed on lite-07-wip, in the quest that owns the code, before the cut.

Agent's choices, for the implementer to revisit:

- Reuse the drills' shaper for the links and `test/interop/compat.sh`'s
  download of the released relay.
- The file names relays and links, each link with its cost and whether
  either end marks it. The script is a list of timed events, so two runs
  share one sequence and their reports compare.
- Sweep the report over both axes: relays in the mesh and broadcasts per
  relay.
- The counts are a report, not a gate, as benchmarks are (2026-10-20,
  maintainer).
- Each relay's route counts come from its stats (`routes` in a label's
  presence) or `/metrics` (`moq_relay_route_*`), which
  [moq-dev/moq#9126](https://github.com/moq-dev/moq/pull/9126) added; see
  `doc/concept/stats.md`.
- A pair of edges joined over lite-06 can still transit, a known gap of the
  lite-06 path that ends when the mesh finishes its roll. Name it in the
  report instead of failing on it.

Public API: none. Wire: none.

## Related

- [Route wakes](/quest/m4/route-wakes.md) - owns the rewrite this measures on a real mesh
- [The cases no test covers run on real sessions](/quest/m2/multicast/accept.md) - the same failures on mock sessions with mocked time
- [Random fault schedules](/quest/m4/cluster-fault-schedules.md) - searches fault timings on the simulated clock
- [A nightly soak](/quest/m4/relay-soak.md) - one real relay under churn
- [A draining relay drains the cluster links it dialed](/quest/m3/drain-dialed-links.md) - without it, a drained relay's dialed links end with DEAD and no DRAIN
