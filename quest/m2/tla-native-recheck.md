# [M] A Rust TLA+ checker is rechecked once the route model drops SumOver

## Goal

The `tla` recipes run a native TLA+ checker locally when one gives TLC's
verdict and distinct-state count on every exhaustive config and costs fewer
core-seconds than TLC on the route and resume models; Nightly then runs both
engines on every config and fails when they disagree. If none does, the PR
records what each failed on and adopts nothing.

## Plan

[moq-dev/moq#8600](https://github.com/moq-dev/moq/pull/8600) compared tlx,
tla-mc, tla-rs, and tlaplusplus with TLC 1.8.0 on every exhaustive config and
adopted none. Each agreed with TLC wherever it finished, and tlx and tla-mc
cost a fifth to a third of TLC's CPU on the resume model, but on the route
model:

- tla-mc rejects `RouteHandoff.tla` (`unknown operator f`) where `SumOver`'s
  operator argument is passed on (`Sum(f(_)) == SumOver(ns, f)`).
- tlx and tla-rs exceed a 16 GB cap where TLC fits in 4 GB, tlx after
  finding 5% to 50% of TLC's states.

`SumOver` was a recursive function over `SUBSET S`, the likely cost for both.
[moq-dev/moq#8628](https://github.com/moq-dev/moq/pull/8628) replaced it with
the Community Modules' `FoldSet` (`FiniteSetsExt`, through `INSTANCE`), which
`sh/tlc.py` puts on TLC's classpath, so a checker must also resolve that
module. Rerun #8600's comparison at each checker's newest revision, against
TLC remeasured after #8628: route and resume first, since they decide cost,
then every other exhaustive config, since adoption needs agreement on all of
them:

- Build each from a pinned flake input inside the Nix sandbox, and run each
  check there as a derivation under `prlimit --as=16G`, one at a time through
  `quest gate`; an uncapped tla-rs run took the host's memory in #8600.
- Every config a checker runs gives TLC's verdict and broken rule, and a
  passing one TLC's distinct-state count. `Simulate:` configs stay on TLC,
  picked per config by the recipe.
- A broken rule prints TLC's trace format, with each step's action, which the
  verify skills' replay steps read; tla-mc prints states alone.
- Report upstream bugs to the maintainer rather than filing them.

Public API: none. Wire: none.
