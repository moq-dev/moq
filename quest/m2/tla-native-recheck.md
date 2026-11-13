# [M] tla-mc runs the TLA+ models locally once it checks the route model

## Goal

The `tla` recipes run [tla-mc](https://github.com/ddalton/tla-mc) on every
exhaustive config once a pinned revision checks the route model, prints a
trace that names each step's action, and keeps TLC's verdict and
distinct-state count on every config. Nightly then runs both engines on every
config and fails when they disagree, as the
[first comparison](https://github.com/moq-dev/moq/pull/8600) planned.

## Plan

[moq-dev/moq#8600](https://github.com/moq-dev/moq/pull/8600) compared four
Rust checkers against TLC 1.8.0 on every exhaustive config and adopted none.
tla-mc agreed with TLC everywhere it ran and cost a third of TLC's CPU on the
resume model (672 against 2,057 CPU-s on `ResumeReader.cfg`), but:

- It rejects `RouteHandoff.tla` with `unknown operator f`, where an operator
  argument is passed on to another operator
  (`Sum(f(_)) == SumOver(ns, f)`), so it runs no route config.
- Its counterexample lists states without the action that led to each, which
  the verify skills' replay steps read (`<Lift(n, x) line ...>` in TLC).

Recheck both at tla-mc's newest revision, built from a pinned flake input in
the Nix sandbox and run there under a memory cap, as #8600 did. If both are
fixed, rerun #8600's sweep on the route and resume configs and adopt it when
it agrees and costs fewer CPU-seconds than TLC on both. Try `-compile` too:
it builds the generated checker with `cargo` at run time, so it needs the
generated crate built as a Nix derivation instead (`-codegen DIR`).

Public API: none. Wire: none.

## Waiting

- Maintainer: report the two tla-mc gaps above upstream (the quest's rule is
  to report upstream bugs to the maintainer rather than file them); done when
  issues exist on ddalton/tla-mc
- ddalton/tla-mc: support passing an operator argument to another operator,
  and name each step's action in a trace; check by running a newer revision on
  `rs/moq-net/verify/route/RouteHandoff.cfg` and `EndedRelay.cfg`
