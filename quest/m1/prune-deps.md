# [M] Remove the dependencies that don't earn their weight

## Goal

Every Rust, JS, and vendored dependency earns its weight. One whose use is
trivial, that another dependency already covers, or that serves a feature
not worth keeping is removed. The tree carries the fewest dependencies that
do the job.

## Plan

This is a one-off, off-cycle run of the [dependency audit](/quest/m2/audit-deps.md),
focused on removal rather than upgrades, ahead of the cut. The recurring
audit keeps its schedule, and this quest creates no successor. Its PR
rewrites that audit's baselines and its "Kept on purpose" and "Hand-rolled
code" lists with what this run learned.

Decided 2026-11-01 (maintainer):

- Prefer fewer dependencies. Vendored code costs upkeep too: see the
  [vendor-sync epic](/quest/m1/vendor-sync/README.md). A crate or package
  stays only for nontrivial non-core functionality it covers better than our
  own code would. Count its compile time and transitive tree in its cost.
- Default to removing. Remove inline in this quest's PR, one commit per
  dependency or group. The PR body lists each removal with its cost and what
  replaced it. Dropping a public API or a user-facing feature (a protocol, a
  codec backend, a binary) is a proposal with a recommendation, filed as a
  follow-up through `/quest-plan`.

Re-judge the m2 audit's "Kept on purpose" list under the fewer-dependencies
preference rather than carrying it forward. Look for crates pulled in for one
helper, two crates doing the same job (error derives, hex, base64, backoff,
CLI or config parsing), dev-dependencies a test could live without, and
optional features that enable more than they're used for. `cargo tree -e
features`, `cargo shear`, and `bun why` help find them.
