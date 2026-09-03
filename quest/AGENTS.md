# Quests

Read this file whenever work mentions a quest or questline.

Quests are optional, versioned plans for work that needs durable scope, memory,
or coordination. Think GitHub issues checked into the repository. GitHub issues
remain the public front door, but prefer making a quest.

## Model

- A quest is a task that should be completed independently in a single PR. It
  is a Markdown file such as `archive.md`.
- A questline is an ordered collection of quests and/or other questlines. It is
  a directory whose entrypoint is `README.md`. Questlines are never executed
  directly; a questline is complete when all of its quests are complete.
- Everything lives under `quest/`. [README.md](README.md) is the permanent root
  questline.
- The root's entries are milestones: questlines in directories named `m0`,
  `m1`, ... that group work by priority horizon. Lower numbers matter more, and
  a completed milestone's number is not reshuffled onto the survivors. A
  milestone may open with a gate quest (the release rule under Creation) that
  its later work requires.
- Every `Quests` list is ordered by priority, most important first; ready
  quests are taken in list order. Insert a new quest or questline at its rank
  rather than appending - including the root, where new work joins the
  milestone matching its priority.
- Quests and questlines reference each other with root-absolute links.
- Finished quests and questlines are deleted and remain accessible through git
  history.
- Merge conflicts are expected. Resolve them by aligning quests.

## Format

A quest:

```markdown
# [S] Short title

## Goal

The observable outcome and important boundaries.

## Plan

Current decisions, open questions, or implementation guidance.

## Required

- [Blocker](/quest/foo/bar.md) - work that must finish before this can start

## Closes

- [#701](https://github.com/moq-dev/moq/issues/701) - close this issue when the quest finishes

## Related

- [Other](/quest/other/quest.md) - similar work that is not a blocker
```

`Goal` is required. Prefix the title with `[XS]`, `[S]`, `[M]`, `[L]`, or
`[XL]`, estimating implementation, verification, and landing work. Every other
section is optional. Use these exact headings: readiness checks grep for
`## Required` literally.

A questline uses `Quests` instead of `Required`, listing at least one entry in
priority order, each with a one-line summary:

```markdown
## Quests

- [Child quest](/quest/foo/bar.md) - the outcome, so the list reads without opening it
- [Nested questline](/quest/foo/baz/README.md) - what the whole line delivers
```

- A quest's `Required` section lists its blockers. Its absence means the quest
  is ready to start.
- A quest may require a questline; that blocker clears only when the whole
  questline is complete.
- A `Required` bullet may be plain text naming a condition outside the
  repository; remove it when the condition clears.
- `Required` relationships must be acyclic. Before adding one, follow links
  from the target and ensure they cannot reach the current file.
- `quest check` (the `rs/quest` binary) enforces this section mechanically -
  links resolve, the index matches the file tree, headings stay inside the set
  above, and `Required` stays acyclic. `just check` runs it on any branch
  touching `quest/`.

## Creation

- Quests are created in PRs and reviewed.
- Size every quest in its title. Re-estimate it when scope changes materially.
- Search the living tree and git history before creating a quest.
- Split independently completable work into separate quests. Group them in a
  questline only when they ship together; a one-off sits directly in its parent.
- Represent a release or pin bump that unblocks repository work as its own
  quest, holding the external condition as a plain-text `Required` bullet, and
  make every dependent quest require it. When the condition clears, remove the
  bullet, do the work, and complete the quest; one completion unblocks every
  dependent.

## Execution

- Only quests are executed, and only when ready: no `Required` section means no
  blockers.
- The branch name is the quest path without the trailing `.md`, e.g.
  `quest/foo/bar.md` becomes branch `quest/foo/bar`.
- If a local or remote branch for the quest already exists, someone may be
  working on it; continue only if it is stale (old, no open PR).
- Push the branch immediately with an empty placeholder commit: the remote
  branch is the claim that prevents duplicate work. Skip the push when you
  lack write access to the repository.
- Quests may be updated over time as the plan changes.
- A quest is completed when the plan is executed and no further work is needed.
  Follow-up work becomes a new quest.
- Run `just check` before completing the change.
- When the quest is complete, open a PR per
  [CONTRIBUTING.md](../CONTRIBUTING.md), with a GitHub closing keyword for every
  issue listed under `Closes` by the quest AND by any questline the same PR
  completes - a parent's issues are usually where a line's tracking lives, and
  its last child is the only PR that can close them.

## Deletion

- A quest that is no longer needed or cannot be completed is abandoned: delete
  it and explain why in the PR.
- The quest is deleted in the same PR that completes or abandons it.
- When deleting a quest or questline, grep its absolute path and remove every
  reference; this reveals every quest the finished work unblocks. If the
  removed link was the last entry in a section, remove the heading too.
- Deleting a questline's last quest deletes the questline directory in the
  same change. The root questline is never deleted.
