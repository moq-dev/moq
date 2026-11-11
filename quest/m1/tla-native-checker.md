# [M] A Rust TLA+ checker runs the models locally if it agrees with TLC

## Goal

The `tla` recipes run a native TLA+ checker locally when one gives TLC's
verdict and distinct-state count on every config in the tree and costs fewer
core-seconds on the route and resume models. Nightly then runs both engines on
every config and fails when they disagree. If no checker agrees, the PR
records what each one failed on and adopts nothing.

## Plan

Decided 2026-11-10 (maintainer, /quest-plan), as a spike: TLC interprets a
spec on the JVM, and the route model's 4-node runs take tens of minutes of
many cores.

- ✅ A checker that agrees runs locally, and Nightly cross-checks it against
  TLC (rejected: local runs with a TLC-only Nightly, where a checker bump or
  spec change could make it wrong unnoticed; replacing TLC everywhere, where
  nothing checks the checker again; reporting the numbers and adopting
  nothing).

Candidates found that day, each young, at least one AI-written, and benchmarked
only by its authors:

- [tla-mc](https://github.com/ddalton/tla-mc) compiles a spec to a native
  parallel checker and claims about 4x TLC at 8 workers.
- [tlaplusplus](https://github.com/zoratu/tlaplusplus) claims about 10x on
  many cores and passes 174 of 182 of tlaplus/Examples.
- [tla-rs](https://github.com/bbcvc/tla-rs) claims state-for-state agreement
  with TLC on 823 recorded runs and 2x to 3x on large models with one worker.
- [tlx](https://github.com/Medfin-AI/tlx) reports some models slower than TLC.

A checker bug shows as a rule passing that should break, so agreement is the
bar, not speed:

- Every config: the same verdict as TLC, the same broken rule for an
  open-finding config, and the same distinct-state count. The specs use
  `RECURSIVE`, `CHOOSE`, `CASE`, `LAMBDA`, `Sequences`, `FiniteSets`,
  `CONSTRAINT`, and `VIEW`, unmodified.
- Measure both engines at equal worker counts through `quest gate` on a quiet
  host, against TLC as the `tla` recipes run it. That baseline, measured
  2026-11-10 on a 24-core desktop: `RouteHandoff.cfg` (17.7M states generated,
  3.67M distinct) in about 50 s and 930 CPU-seconds on TLC 1.8.0 with Java 21,
  down from about 118 s and 1,730 on TLC 1.7.4 with Java 8. CPU-seconds stayed
  near flat from 2 to 24 workers. Report the table in the PR.
- A config the checker cannot run, such as a liveness property or random
  simulation, which [the mesh model](/quest/m1/route-mesh-model.md) plans,
  stays on TLC; the recipe picks the engine per config and errors rather than
  falling back at runtime.
- A broken rule must still give `/statechart` a trace it can read with the
  spec's `*.view.ts`. Say how.
- Build each candidate from a flake input pinned to a revision, inside the Nix
  sandbox. Run none of their build scripts or binaries outside it.
- Report upstream bugs to the maintainer rather than filing them.

Public API: none. Wire: none.

## Related

- [The TLA+ models run nightly and reviews resync them](/quest/m1/model-checks-nightly.md) - the Nightly job the cross-check joins
- [Every delivery order of a small mesh runs on the real route layer](/quest/m1/route-exhaustive-sim.md) - the other spike, which checks the Rust instead of a faster model
