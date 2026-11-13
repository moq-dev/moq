# [M] A Rust TLA+ checker runs the models locally if it agrees with TLC

## Goal

The `tla` recipes run a native TLA+ checker locally when one gives TLC's
verdict and distinct-state count on every config in the tree it can run
(the rest stay on TLC, as the Plan says) and costs fewer
core-seconds on the route and resume models. Nightly (`just test tla`, which
`sh/tla.sh` drives) then runs both engines on every config and fails when they
disagree. If no checker agrees, the PR
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

- Every config the checker runs: the same verdict as TLC, and the same broken
  rule for an open-finding config. A passing config also matches TLC's
  distinct-state count; a violating one does not, since each engine stops at
  its first violation after exploring a different, order-dependent part of
  the space. The specs use
  `RECURSIVE`, `CHOOSE`, `CASE`, `LAMBDA`, `Sequences`, `FiniteSets`,
  `CONSTRAINT`, and `VIEW`, unmodified; the route model also folds with the
  Community Modules' `FoldSet` (`FiniteSetsExt`, through `INSTANCE`), which
  `sh/tlc.py` puts on TLC's classpath.
- Measure both engines at equal worker counts through `quest gate` on a quiet
  host, against TLC as the `tla` recipes run it. The 2026-11-10 baseline, on a
  24-core desktop and the route model as it stood then (3.67M distinct
  states): about 50 s and 930 CPU-seconds on TLC 1.8.0 with Java 21, down from
  about 118 s and 1,730 on TLC 1.7.4 with Java 8, with CPU-seconds near flat
  from 2 to 24 workers. The model has since changed, and `FoldSet` cut TLC's
  core-seconds on most route configs by 1.4 to 3.6 times, so remeasure TLC
  and report the table in the PR.
- A config the checker cannot run, such as one with a `\* Simulate:` line
  that `sh/tla.sh` samples at random, stays on TLC; the recipe picks the engine per config and errors rather than
  falling back at runtime.
- A broken rule must still print a trace as readable as TLC's, which the
  Reading a failure in `rs/moq-net/verify/README.md` relies on. Say how.
- Build each candidate from a flake input pinned to a revision, inside the Nix
  sandbox. Run none of their build scripts or binaries outside it.
- Report upstream bugs to the maintainer rather than filing them.

Public API: none. Wire: none.

## Related

- [Every delivery order of a small mesh runs on the real route layer](/quest/m2/cluster-routes/route-exhaustive-sim.md) - the other spike, which checks the Rust instead of a faster model
