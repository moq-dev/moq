# [XS] The merge gate runs repository tooling tests only when tooling changes

## Goal

A diff that touches no repository tooling stops paying for the `gh` module's
tests in the merge queue, while Nightly's full `gate` job still runs them
every night.

## Plan

Found 2026-11-14 by the [feature review](/quest/m3/release/feature-review.md).
`sh/dispatch.sh` registers `module gh ''`, an empty pattern that matches every
diff, in `lane=bun` after the JS checks. In queue run 43634181288 (a
`flake.lock` bump) that lane was the whole 497 s run, with `test gh` at 224 s;
in 43622947541 the bun lane (409 s, `test gh` 220 s) outlasted the Rust lane.
Its wall time is about twice its CPU time, and it failed none of 34 queue
failures sampled from 11-12 to 11-14. Three files take 65 s of its 115 s
locally (`dispatch-query.test.py`, `nightly.test.py`, `rs/select.test.py`).

Give `module gh` a path pattern covering what its tests
read (`sh/`, `bench/`, `.github/`, justfiles, `test/smoke/`, the Go publish
scripts) and keep the actionlint and zizmor lints on workflow changes; run
the three slow files in parallel if the lane still leads.

Decided 2026-11-14 (maintainer, pick on [moq-dev/moq#8764](https://github.com/moq-dev/moq/pull/8764)): ✅ (a) scope the module to tooling paths, with Nightly's `gate` as the backstop (rejected: (b) keep it on every diff and only parallelize).

Public API: none. Wire: none.

