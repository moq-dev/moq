# [S] Feature PRs stop editing MIGRATE.md

## Goal

No PR to `main` adds an "Unreleased" section to a package's `MIGRATE.md`, so
breaking PRs stop conflicting with each other there. Each version's migration
section is written only by the release cut, as
[doc/setup/upgrade.md](/doc/setup/upgrade.md) already says, from the `!` PRs'
Impact bullets and the API diff.

## Plan

Measured 2026-10-26: from 2026-10-23 to 10-26, 41 of the merge queue's 246
merge-conflict removals touched a `MIGRATE.md` Unreleased section
(`rs/moq-net` alone 21, the binding guides about 8 to 11 each). About 40
`MIGRATE.md` files carry one today, though `doc/setup/upgrade.md` and
`doc/lib/index.md` say a section "is written when that release is cut, not as
changes land on `main`".

Decided 2026-10-26 (maintainer): drop the Unreleased sections rather than
switch to per-change fragment files. The cut already rebuilds each section
from the PRs' Impact bullets (CONTRIBUTING's release steps), so the per-PR
edits only duplicated it.

- Find where the habit comes from (an `AGENTS.md`, a skill, or copying), and
  correct that source in the same PR. Any rule change is reviewed like one.
- Keep the content of the existing Unreleased sections until the next
  breaking cut consumes it. The cut's [migration
  audit](/quest/m2/release/migrate-audit.md) starts each version section from
  those entries, so no curated note is lost.
- A check that refuses an Unreleased heading on `main` is optional, only if
  the source cannot be corrected.

Public API: none. Wire: none.
