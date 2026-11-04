# [XS] Nightly's Rust and JS audits pass

## Goal

Nightly's Rust and JS audit jobs pass on `main`. Each current advisory is
fixed by an upgrade, or by an override or an ignore that names the advisory
and its reason, so a new advisory shows up as the only red.

## Plan

Decided 2026-11-03 (maintainer, /quest-plan of the follow-ups from
[moq-dev/moq#7667](https://github.com/moq-dev/moq/pull/7667)): milestone m0,
a child of the Nightly epic.

Both fail in the scheduled run of 2026-11-03
([run 41656613559](https://github.com/moq-dev/moq/actions/runs/41656613559)):

- **Rust**
  ([job](https://github.com/moq-dev/moq/actions/runs/41656613559/job/125988511066)):
  RUSTSEC-2026-0173, `proc-macro-error2` unmaintained, reached through
  `hax-lib-macros`. Find which of our crates pulls `hax-lib` in. Upgrade it
  if a release drops the edge. Otherwise ignore the advisory in the audit
  config with a comment naming the parent crate, since an unmaintained
  proc-macro crate runs only at build time.
- **JS**
  ([job](https://github.com/moq-dev/moq/actions/runs/41656613559/job/125988512588)):
  the root `bun.lock` resolves `glob` (GHSA-3g3m-2fjr-6pw4) and `minimatch`
  (GHSA-g3hv-75wg-m249) to vulnerable versions. Raise them through the parent
  that pins them, or with a root `overrides` entry.

Keep [the dependency audit](/quest/m2/audit-deps.md) in sync. It already
records that the root lock is not clean, so update its notes once this
lands.

Public API: none. Wire: none.

## Related

- [Dependency audit](/quest/m2/audit-deps.md) - the recurring audit that owns advisory policy
