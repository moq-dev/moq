# [M] Migration audit of the breaking cut

## Goal

Before the breaking cut merges, every break it ships, including those in
surfaces `just api-diff` cannot see, has its
`MIGRATE.md` entry, and each entry states the net change from the last
published version.

## Plan

Decided 2026-10-16 (maintainer interview); the diff tooling moved to its own
m1 quest on 2026-10-22.

- PRs into `main` no longer write `MIGRATE.md` entries (decided
  2026-10-23). Gather the breaks from `!` PRs: every `!` subject in
  `git log` since the `release` merge base, then each PR's Impact section
  for its migration note, since squash commits drop the PR body. Write each
  note into its package's guide. The API-diff report below catches breaks a
  PR left unmarked; a behavior break with no `!` stays with PR review.
- Correct Unreleased entries that a later PR overturned. Known: `js/net/MIGRATE.md`
  still describes `request(path, { announced: true })`, which
  [moq-dev/moq#7173](https://github.com/moq-dev/moq/pull/7173) replaced with
  `follow(path)` (decided 2026-10-29).
- Run the API-diff report on the cut PR and read the diff of the
  hand-written binding wrappers (`py/moq-rs`, `kt/moq`, `dart/moq`,
  `go/wrapper`, `swift`, `cpp/moq`, `cpp/obs`) and of each binary's flags,
  env vars, and config keys by hand. Add this manual pass to
  CONTRIBUTING.md's Releases section, which already names the other sources.
- Fill every gap the report and the manual pass show in the matching
  `MIGRATE.md`. Known gaps: breaking releases cut after the
  2026-09-23 train have no transition section yet (moq-json 0.4 -> 0.5
  and possibly moq-audio and moq-video), because their breaks predate the
  backfill's merge base. The [compression migration](https://github.com/moq-dev/moq/pull/7046)
  deleted moq-flate before this cut: audit the migration from its published
  API to ordinary tracks with `compress`, rather than promising a new
  moq-flate transition release.
- Each version-transition section, starting from the `## Unreleased`
  entries already on `main`, says how to migrate from the published
  version to the candidate (decided 2026-10-22, maintainer, from an external
  API audit). Collapse entries a later change reversed or reshaped (an API
  removed then reintroduced, a default changed twice) into the net change,
  and drop the intermediate history.
- Read the API-diff report's auto-trait changes too, not only renamed or
  removed items (added 2026-10-27, from an external release-to-main audit
  that found lost `Sync` on JSON consumers, fMP4/HLS importers, video decode
  consumers, and relay state). Restore an accidental loss at its source;
  otherwise the migration note says how to share the type instead.
- Compile-check the code in every `MIGRATE.md` section the cut ships against
  the candidate. Nothing else builds it.
- Required by the cut, after m1's breaking work has landed.
- Start from the candidates [moq-dev/moq#6477](https://github.com/moq-dev/moq/pull/6477)
  listed as breaks matching no name in `MIGRATE.md` (about 15 Rust crates,
  `@moq/hang`, `@moq/net`, `@moq/watch`, and moq-ffi): confirm each, and add
  its migration note or record why none is needed.

## Required

- [Breaking release readiness](/quest/m1/README.md) - all breaking work and the MIGRATE.md convention have landed
