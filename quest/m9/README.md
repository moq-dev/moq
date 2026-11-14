# m9: lab experiments that merge whenever

## Goal

Experiments and measurements that touch no published crate's or package's
public API, no wire format, and no CLI flag, config key, or environment
variable: prototypes, spikes, benchmarks, profiles, and tuning inside one
implementation. Each can start at any time, run beside any other work, and
merge into `main` whenever it is ready, because nothing a release publishes
changes shape when it lands.

## Plan

Decided 2026-11-13 (maintainer, /quest-plan): renamed from `lab` to m9
(rejected: keeping `lab`). `quest unblocked` orders milestones by name, so
`lab` ranked ahead of m0; m9 ranks last and leaves m6 to m8 free for the
shifts after a cut. m9 is never gated, so while m4 and m5 wait for the cut,
spare capacity comes here.

Decided 2026-11-12 (maintainer, /quest-audit before the cut): lab sits beside
the milestones instead of in their order. A cut never waits on it, and the
shift after a cut leaves it in place.

- ✅ A quest belongs here when its whole outcome is internal: an unpublished
  crate, a benchmark or test harness, a measurement, a model, or a change
  behind an existing API that no caller can observe except as speed or cost
  (rejected: leaving such work in m4 and m5, where it queues behind a cut it
  cannot affect).
- A lab quest that finds it needs a public API, wire, or CLI change stops and
  brings that to the maintainer; the change is planned as its own quest in the
  milestone its kind and urgency call for, and the lab quest keeps only the
  internal half.
- A lab quest with a named consumer waiting on its result says so in its Plan
  and still stays here; `quest unblocked` ranks it by what it unblocks.
- A lab quest nobody has started and nobody waits on is deleted when it goes
  stale, like the rest of the deferred work; git history keeps it.
