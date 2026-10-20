# [M] Dependency and alternatives audit

## Goal

Dependencies are current, deduplicated, needed, and the best maintained option,
and hand-rolled non-core code that a maintained library covers is planned for
replacement.

## Plan

Recurs once per major release (decided 2026-10-19). The PR that finishes
this run also adds the next one with the same file name at the end of the next
milestone's `Required` (m0 to m1, m1 to m2). The successor keeps the Goal
and this paragraph; the rest of its Plan is rewritten with what this run
learned: baselines, results, and what to check first. A successor placed in m2
also lists the [release cut](/quest/m2/release/cut.md) under `Required`, so it
waits for the release in flight to ship. Only `/quest-delete` ends the
recurrence. Fixes land in the run's PR, one commit each, rather than as follow-ups.
Only a change that needs a maintainer decision (public API, scope) or is
independently landable and too large to review alongside the rest becomes a
follow-up through `/quest-plan`.

Research the state of the art for each candidate: compare maintenance,
performance, and API fit before planning a swap.

Starting points (2026-10-19): Dependabot bumps weekly with a 7-day cooldown, so
look for the majors it cannot take. `deny.toml` warns on duplicate versions;
count them in `Cargo.lock` and `bun.lock`. `cargo deny` runs only
nightly. Look for unmaintained dependencies (RUSTSEC
`unmaintained`, no release in a year) and for hand-rolled utilities a
maintained crate or package now covers.

Partial run (2026-10-19): compatible Bun updates reduced the root audit from
27 findings across 11 packages to six across four. Vue's renderer, browser
metadata, brace-expansion, PostCSS, nanoid, and source-map-js were refreshed.
Wrangler 4.151.2 removed the vulnerable sharp pin in `infra/moq-sh`.
All four independent Worker lockfiles now audit cleanly. `just js audit`
checks all five Bun graphs nightly without installing dependencies, and
`deny.toml` now points to the actual Rust audit recipe.
The obsolete standalone `doc/bun.lock` was removed: `doc` belongs to the
root Bun workspace, and that lock still named its old package and tooling.

Baselines: Cargo.lock has 1,078 entries, 91 names with multiple versions
(110 extra versions), plus 11 names repeated from different sources at the
same version. The root Bun lock moved from 70 duplicate names / 82 extra
versions to 68 / 79. Independent Worker locks have no duplicate names.
These counts are inventory, not evidence that incompatible major versions
can safely be unified.

Remaining advisories need an upstream release or a scope decision; no new
exceptions or overrides were added:

- ashpd 0.13.13 pulls zbus 5 and the unmaintained zvariant, zvariant_derive,
  and zbus_names crates (RUSTSEC-2026-0370, 0371, 0372). Upstream's
  [zbus 6 migration](https://github.com/bilelmoussaoui/ashpd/pull/491) is open.
  Prefer its maintained release over removing desktop portal support or
  adding a Git dependency.
- VitePress 1.6.4 pins Vite 5, which pins vulnerable esbuild. The replacement
  VitePress line is still a prerelease. A migration, override, or advisory
  exception needs a decision.
- `@tailwindcss/typography` 0.5.20 pins postcss-selector-parser 6.0.10;
  the fix starts at 7.1.6. Prefer a maintained upstream update.
- braces 3.0.3 has no published fix for
  [GHSA-vfj7-8cjw-p6xm](https://github.com/advisories/GHSA-vfj7-8cjw-p6xm).
  It is pulled by fast-glob/micromatch and remark-cli's watcher chain.

This run is incomplete: compare direct dependency majors, maintenance and
alternatives, investigate duplicate ownership, and review hand-rolled non-core
utilities before declaring the audit complete and adding its successor.
