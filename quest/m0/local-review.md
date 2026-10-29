# [S] Adopt local reviews

## Goal

PRs here are reviewed locally by the other vendor's model through
`/quest-review` before they are pushed for review, and merging never waits
on an online reviewer. Start once `quest review` is on kixelated/quest's
`main`, where it is planned as `quest/m0/local-review.md`.

## Plan

Decided 2026-10-28 (maintainer, /quest-plan in kixelated/quest):

- Bump the `quest` flake input to a kixelated/quest rev with `quest review`.
- Rewrite the review policy: the PR body's `## Review` record counts as the
  required review when it names the head being merged, and so does a
  recorded skip of a change the author judged too trivial to review.
  Online reviewers' findings already posted on the head are addressed at
  merge, never waited for or requested.
- In `CONTRIBUTING.md`'s Reviews section, drop waiting for "a review from
  any reviewer other than Grok", the Codex 👍 rule, the "push with no new
  review was judged trivial" rule, and asking the maintainer to arrange a
  fork's review: the merging session checks out the head and runs
  `/quest-review`.
- `REVIEW.md` is added by its own PR; check that `quest review` reads it
  from the base.
