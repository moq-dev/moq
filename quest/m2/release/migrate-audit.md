# [M] Migration audit of the breaking cut

## Goal

Before the breaking cut merges, every break it ships, including those in
surfaces the [API-diff check](/quest/m1/api-diff.md) cannot see, has its
`MIGRATE.md` entry, and each entry states the net change from the last
published version.

## Plan

Decided 2026-10-16 (maintainer interview); the diff tooling moved to its own
m1 quest on 2026-10-22.

- PRs stop writing `MIGRATE.md` entries once
  [MIGRATE.md at the cut](/quest/m0/migrate-at-cut.md) lands (decided
  2026-10-23): gather every `!` subject in `git log` since the `release`
  merge base and write each PR's Impact-bullet migration note into its
  package's guide.
- Run the API-diff report on the cut PR and read the diff of the
  hand-written binding wrappers (`py/moq-rs`, `kt/moq`, `dart/moq`,
  `go/wrapper`, `swift`, `cpp/moq`, `cpp/obs`) and of each binary's flags,
  env vars, and config keys by hand. Add the audit step to CONTRIBUTING.md's
  Releases section.
- Fill every gap the report and the manual pass show in the matching
  `MIGRATE.md`. Known gaps: breaking releases cut after the
  2026-09-23 train have no transition section yet (moq-json 0.4 -> 0.5
  and possibly moq-audio and moq-video), because their breaks predate the
  backfill's merge base. The [compression migration](/quest/m1/compress/migrate.md)
  deletes moq-flate before this cut: audit the migration from its published
  API to ordinary tracks with `compress`, rather than promising a new
  moq-flate transition release.
- Each `## Unreleased` section says how to migrate from the published
  version to the candidate (decided 2026-10-22, maintainer, from an external
  API audit). Collapse entries a later change reversed or reshaped (an API
  removed then reintroduced, a default changed twice) into the net change,
  and drop the intermediate history.
- Compile-check the code in every `MIGRATE.md` section the cut ships against
  the candidate. Nothing else builds it.
- Required by the cut, after m1's breaking work has landed.

## Required

- [API-diff check](/quest/m1/api-diff.md) - the report this audit reads
- [Breaking release readiness](/quest/m1/README.md) - all breaking work and the MIGRATE.md convention have landed
