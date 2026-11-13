# [M] TLC runs cost fewer core-seconds and share the host

## Goal

The `tla` recipes and their callers check every TLA+ config with the same
verdict as before for measurably fewer core-seconds, a TLC run leaves room on
the host for other gated commands, and no run, killed ones included, leaves
its working directory behind. Agents check the one config a change touches;
whole directories are Nightly's.

## Plan

Measured 2026-11-12 on the maintainer's agent hosts:

- An ungated `just test tla rs/moq-net` matched one agent session's whole
  usage: 21.4 CPU-hours over 1.8 hours.
- A gated `just test tla rs/moq-net/verify/route` took about 24 CPU-hours in
  53 minutes at about 31 cores, while 35 other gated commands queued behind
  it, the oldest for 53 minutes. `quest gate` admits by load average, so one
  run that takes every core stalls everything else.
- Another session ran TLC by hand, outside the recipes, for 2.5 hours:
  15.6 CPU-hours and 17.8 GB, on nixpkgs `tlaplus` 1.7.4 and a Java 8 JRE.
- Nightly splits these configs across five runners at about an hour each on
  4 workers.
- A killed run's `-metadir` under `/tmp` still held 26 GB.

Decided 2026-11-12 (maintainer, /quest-plan):

- ✅ Cap TLC's workers in `rs/justfile` and `js/justfile`, which hard-code
  `-workers auto`. Recommended: 4, which the `verify-*` skills' times
  already assume and Nightly's runners have; the native checker spike
  ([moq-dev/moq#8600](https://github.com/moq-dev/moq/pull/8600)) measured
  core-seconds near flat from 2 to 24 workers, so the cap costs wall time,
  not CPU.
- ✅ TLC 1.8 on a modern JDK. The recipes moved to `nixpkgs#tlaplus18` in
  [moq-dev/moq#8499](https://github.com/moq-dev/moq/pull/8499); the 1.7.4 run
  above bypassed them. Leave no doc, skill, or script pointing at the 1.7.4
  package, `nixpkgs#tlaplus`.
- ✅ Replace `SumOver` in `rs/moq-net/verify/route/RouteHandoff.tla`, a
  recursive function over `SUBSET S` that TLC re-evaluates on every use, with
  the Community Modules' `FoldSet` (`FiniteSetsExt`), folding `f(x)` over
  `S`. Not `SumSet({f(x) : x \in S})`, which sums each value once and so drops
  two nodes' equal counts. TLC evaluates `FoldSet` natively only through the
  Java override in `CommunityModules-deps.jar`, which nixpkgs does not
  package: take it from a pinned release, put it on TLC's classpath in both
  recipes, and confirm TLC's startup log reports the override loaded.
  [moq-dev/moq#8564](https://github.com/moq-dev/moq/pull/8564) rewrites
  `SumOver` as an Apalache fold; whichever lands second keeps one fold for the
  model.
- ✅ Add `SYMMETRY` where sound. No config checks a `PROPERTY`, so every one
  checks safety only, which symmetry preserves. A permutation is sound only if
  it fixes every constant the config gives, such as `Links`, `Origin`, or
  `Start`, and the spec never orders those values or does arithmetic on them;
  so turn
  a fully interchangeable set, such as `Relays` or `Endpoints`, into model
  values and permute it, or the members no constant singles out. That can cut
  a state space by up to n!, though `Endpoints = {"a", "b"}` with
  `Start = "a"` gains nothing. Say per config whether it applies and why.
- ✅ Pass `-checkpoint 0`, and make sure no metadir outlives its run. TLC never
  removes one it was killed in, and a signal handler cannot run on SIGKILL.
  Recommended: put the metadir under the checkout's `.tmp/`, remove it when
  the recipe exits, and at start remove the metadirs earlier runs left there
  that no live run owns, so a checkout that outlives killed runs does not
  collect them. Mark ownership so a concurrent run's metadir survives, such
  as a lock each run holds on a file in its metadir, which the kernel drops
  when the run dies.
- ✅ Agents check one config locally: the `verify-*` skills, `REVIEW.md`, and
  the `js/*/verify/` READMEs name single configs, and only Nightly runs
  `just test tla` over a directory.
- Rejected: Apalache, which is no cheaper here and ran out of memory on the
  route model ([moq-dev/moq#8564](https://github.com/moq-dev/moq/pull/8564));
  waiting for a Rust checker, none of which ran the route model in
  [moq-dev/moq#8600](https://github.com/moq-dev/moq/pull/8600).

Not decided: the route-inductive work in progress adds a refinement check,
`RefinesClaims`, to the simulated `Blind.cfg`, where every sampled trace
re-checks it. Recommended: check a refinement on the smallest exhaustive config
that exercises it, where each distinct state is checked once, unless measuring
shows the simulated config is cheaper.

Verification: every config keeps its verdict, and every exhaustive config
without `SYMMETRY` keeps its distinct-state count (the `verify-*` skills list them).
Measure each config's core-seconds through `quest gate` at equal worker counts
before and after, report the table in the PR, and update the times the skills
and Nightly's `tla` matrix comment quote; rebalance that matrix if its legs
change a lot.

Public API: none. Wire: none.

## Related

- [A Rust TLA+ checker is rechecked once the route model drops SumOver](/quest/m2/tla-native-recheck.md) - a faster engine for the same specs, waiting on this quest's `SumOver` rewrite
- [The route rules hold from any state, not only the runs TLC reaches](/quest/m1/route-inductive.md) - rewrites `SumOver` and adds the refinement check named above
