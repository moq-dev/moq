# [S] Write MIGRATE.md at the breaking cut

## Goal

PRs into `main` no longer touch `MIGRATE.md`, so they stop conflicting on it,
while each breaking cut still ships a correct per-package migration guide
stating the net change from the last published version. The guide format
readers see is unchanged.

## Plan

Decided 2026-10-23 (maintainer interview):

- Conflicts come from every breaking PR inserting at the top of the same
  `## Unreleased` section. Notes are written once, at the cut, instead of per
  PR. Rejected: per-PR fragment files stitched at the cut (more files plus a
  stitch recipe) and a `merge=union` driver (GitHub's PR and merge-queue
  merges ignore it, so it only helps local merges).
- A breaking PR keeps the conventional `!` in its title, which becomes the
  squash subject, and says how to migrate in its Impact bullet. Squash
  commits drop the PR body, so the
  [migration audit](/quest/m2/release/migrate-audit.md) greps `git log` for
  `!` subjects since the `release` merge base, reads each PR's Impact
  section, and relies on the release gate's `just api-diff` for
  breaks a PR left unmarked. Rejected: a `breaking` label (one more thing to
  remember) and reading every merged PR (hundreds per cycle). A behavior
  break that keeps its declarations and lacks `!` escapes both; it escapes
  today's per-PR rule the same way, since an author who misses a break
  writes no entry either, so catching it stays with PR review.
- The `## Unreleased` entries already on `main` stay as the cut's starting
  draft, and [moq-dev/moq#6443](https://github.com/moq-dev/moq/pull/6443)
  lands as planned.

Changes:

- `AGENTS.md` Public API and `CONTRIBUTING.md` PRs: a breaking change marks
  its title with `!` and its Impact bullet says how to migrate; drop the
  per-PR `MIGRATE.md` entry rule.
- `CONTRIBUTING.md` Commits: drop the claim that the PR description becomes
  the squash commit's body; the merge queue keeps only the title and
  co-author trailers.
- `CONTRIBUTING.md` Releases: replace the "rename `## Unreleased`" step with
  the cut writing each changed package's version-transition section from the
  audit.
- `doc/setup/upgrade.md` and `doc/lib/index.md`: say entries appear when a
  breaking release is cut, not as changes land on `main`.
- Rewrite every quest that tells its PR to add a `MIGRATE.md` entry
  (`rg -l MIGRATE quest`) to put the note in the PR's Impact bullet instead,
  and update the migration audit's Plan to gather from `!` PRs.

## Related

- [Migration audit](/quest/m2/release/migrate-audit.md) - writes the entries this quest stops PRs from adding
- [MIGRATE matches release](/quest/m1/api-audit/migrate.md) - the in-flight alignment of today's entries
