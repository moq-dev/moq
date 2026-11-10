# [M] Showing an identity's claims costs the claims that changed

## Goal

A lite-07 session's setup and teardown cost grows linearly with the
broadcasts it carries, as lite-06's does. One identity holding many claims no
longer makes `show` in `rs/moq-net/src/route/mod.rs` quadratic. The session
bench (`rs/moq-net/benches/session`) and the route bench
(`rs/moq-net/benches/routes.rs`) show it, swept over both axes as the
repository's fan-out rule requires.

## Plan

Planned 2026-11-09 (maintainer, /quest-complete follow-up of
[moq-dev/moq#8352](https://github.com/moq-dev/moq/pull/8352)). ✅ m2, in this
epic (rejected: m4, dropping it).

#8352's session bench measured lite-07 setup at 4096 broadcasts at 4.9 s of
CPU, against 0.5 s for lite-06, and teardown the same. It traced this to
showing one identity's claims: every change re-derives what the identity
shows from all of its claims. That ran on main after
[moq-dev/moq#8293](https://github.com/moq-dev/moq/pull/8293) (routes presented
incrementally), but before
[moq-dev/moq#8374](https://github.com/moq-dev/moq/pull/8374) (a relay folds
received route batches by the claims they change).

Re-measure on current main first. If #8374 already made it linear, record the
numbers in the PR and delete this quest. Otherwise keep what `show` derives up
to date per changed claim instead of recomputing it, and add a route-bench
case swept over claims per identity and claims changed.

Public API: none expected. Wire: none.
