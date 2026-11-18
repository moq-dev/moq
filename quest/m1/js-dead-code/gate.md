# [M] A dead-code check for JS joins the gate

## Goal

`just check` runs one JS dead-code tool over the whole workspace and fails
on any unused file, export, type, class member, dependency, or undeclared
dependency. Every real entry point is configured, and the tree passes clean.
Rust already has the equivalent for dependencies (`cargo-shear`). Public API:
none. Wire: none.

## Plan

Decided 2026-11-17 (maintainer, /quest-plan): ✅ compare the candidates on
this repository and adopt the winner in the gate (rejected: evaluate and
report only; an on-demand recipe for the code audit).

Candidates, compared with the same entry points and on the same commit:

- [knip](https://knip.dev): the established Node tool, with the most
  framework plugins.
- [fallow](https://fallow.tools): Rust and oxc, at 1.x, with a knip config
  migration. Its docs say it is not always faster than knip v6.
- [skif](https://github.com/polylint/skif): Rust and oxc; it also checks
  version drift and manifest order. Ran in 0.56 s on 0.9.0 (2026-11-17), but
  it was 16 days old with breaking minors released daily.

Entry points the trial showed a tool cannot infer: benches, examples, and
`test-dts` in each `js/*` package; scripts that shell files launch
(`run.sh`, `sh/*.sh`); `*.browser.ts` pages; `Bun.resolveSync` targets; and
the nested workspaces under `test/interop/clients/`. Exclude `rs/` test data
and `.direnv/`.

Pick by false positives on the configured tree first, then by run time, then
by config size; the PR body shows the comparison with each tool's version and
time. Wire it into `just check` so it runs whenever the branch changes any
JS, but always over the whole workspace, not only the selected packages:
removing the last use of an export in one package leaves it dead in the
package that defines it, which a branch-scoped run never analyzes. If the
winner is too slow for that, the PR body says what it adds to `just check`.
Fix whatever it finds that the sweep did not.

## Required

- [Dead export and manifest sweep](/quest/m1/js-dead-code/sweep.md) - clears the findings known today, so this PR holds only the tool and what it newly finds
- [js/common becomes a workspace](/quest/m1/js-dead-code/common-workspace.md) - without it, every tool misattributes the plugin imports

## Related

- [Code audit](/quest/m1/audit-code.md) - its dead-code pass uses the configured tool
