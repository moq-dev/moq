# Quests

Read this file whenever work mentions a quest or questline.

Quests are versioned plans checked into the repository under `quest/`. GitHub
issues remain the public front door; prefer a quest for work that needs
durable scope or coordination.

## Model

- A quest is a Markdown file, completed in one PR. A questline is a directory
  whose `README.md` is its quest: its `Quests` section lists the children, and
  it completes when its own work is done and every child has merged.
- The root's entries are milestones, `m0`, `m1`, ..., grouping work by
  priority horizon; lower numbers matter more. [README.md](README.md) says
  what each holds. Priority, not breakage, decides the milestone, and starting
  a quest does not move it.
- A document's branch is its path without `.md`: `quest/m1/foo/bar.md` is
  branch `quest/m1/foo/bar`, and its line is `quest/m1/foo/README`. A quest
  merges into its line's branch, a line into its parent's, and a milestone's
  direct children into `main`. Milestones have no branch.
- A published API or wire break retargets to `dev` at PR time, per the root
  `AGENTS.md`; a quest's Plan may note it.
- Every `Quests` list is ordered by priority. Insert at rank, never append.
- Link with root-absolute paths. Finished documents are deleted; git history
  keeps them. Merge conflicts are expected; resolve them by aligning quests.

## Format

```markdown
# [S] Short title

## Goal

The observable outcome and important boundaries.

## Plan

Current decisions, open questions, or implementation guidance.

## Quests

- [Child quest](/quest/foo/bar.md) - the outcome, so the list reads without opening it
- [Nested questline](/quest/foo/baz/README.md) - what the whole line delivers

## Required

- [Blocker](/quest/bar.md) - work that must finish before this can start

## Closes

- [#701](https://github.com/moq-dev/moq/issues/701) - close this issue when the quest finishes

## Related

- [Other](/quest/other.md) - similar work that is not a blocker
```

- `Goal` is required; everything else is optional. Use these exact headings.
- Size the title `[XS]` to `[XL]` for implementation, verification, and
  landing. A README with children carries no size; one without is a plain
  quest and needs one.
- Only a README has `Quests`. `Required` lists what must finish before the
  work starts; no section means ready. A required questline clears when the
  whole line has merged. A plain-text bullet names a condition outside the
  repository; remove it when it clears. `Required` must be acyclic.
- `quest check` enforces this structure; `just check` runs it on any branch
  touching `quest/`. `quest ready [<path>]` prints what blocks a quest, or
  every ready quest. `quest branch <path>` prints the branch and every branch
  it merges through, nearest first and ending at `main`. Run them as
  `cargo run --quiet --locked --package quest -- ...`. They read the tree
  alone: whether a PR already claims a quest is GitHub's question.

## Creation

- Quests are created in PRs and reviewed. Search the tree and git history
  first.
- Split independently completable work into separate quests. Group them in a
  questline only when they ship together, and give the README the work no
  child owns: the end-to-end test, the docs page.
- New work joins the milestone matching its priority, at its rank.
- Every issue under `Closes` carries the `quest` GitHub label
  (`gh issue edit <n> --add-label quest`), applied when the quest lands.
  `Related` is context and gets none.
- A release or pin bump that unblocks work is its own quest holding the
  condition as a plain-text `Required` bullet; every dependent requires it.

## Execution

- Start only ready quests. `quest branch` names the branch and its bases: push
  each missing line branch from the one after it and open its draft PR against
  that base, then push the quest's branch with an empty commit. The remote
  branch is the claim; continue only if an existing one is stale (old, no open
  PR).
- Set the upstream to the base so `just check` scopes against
  it. Keep a line current by merging its base in; never rebase a shared branch.
- Update the quest as the plan changes. Complete it when no work remains, and
  suggest follow-ups as new quests.
- Open the PR per [CONTRIBUTING.md](../CONTRIBUTING.md) against the base, with
  a closing keyword for every issue under `Closes`, including those of any
  questline the same PR completes.
- A line's PR stays a draft until its `Quests` list is empty. The PR that
  removes the last child sizes the README's title; the README is then a ready
  quest whose completion marks the line's PR ready and merges it.

## Deletion

- A quest that is no longer needed or cannot be completed is abandoned: delete
  it and explain why in the PR. Remove the `quest` label from issues no other
  quest tracks.
- Delete a quest in the PR that completes or abandons it. Grep its absolute
  path and remove every reference; that reveals what it unblocks. Remove a
  heading with its last entry.
- Deleting a README deletes its directory. The root and the milestones are
  permanent: an empty milestone stays as a horizon.
