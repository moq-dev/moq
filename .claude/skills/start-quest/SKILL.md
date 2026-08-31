---
name: start-quest
description: Find unblocked repository quests and start the user's choice. Use when the user invokes /start-quest, optionally with a base questline such as /start-quest vod, or asks what quest to work on next.
---

Before you begin, read `quest/AGENTS.md` completely.

Resolve an optional argument to a base questline directory under `quest/`, defaulting to `quest/` itself.

Find every ready quest: one with no `Required` section. Questline `README.md`s are never executed, so exclude them:

```bash
rg --files-without-match '^## Required$' <base> --glob '*.md' --glob '!README.md' --glob '!AGENTS.md' --glob '!CLAUDE.md'
```

No output (rg exits 1) means the questline has no ready work; say so.

Drop candidates that are already claimed: a local or remote branch named after
the quest path that is recent or has an open PR. A stale branch (old, no open
PR) does not claim the quest; mention it so it can be reused or deleted. Also
detect local and remote quest branches whose names are ancestors or descendants
of the candidate branch. A recently moved quest may still be claimed by a
branch at its former path; check the file's git history when a similar branch
exists. Before creating the worktree, delete a conflicting ref
only when it is confirmed stale, is not checked out, and has no unmerged
commits; otherwise treat the candidate as claimed.

Order the remaining candidates by a depth-first walk of the questline tree from
the base `README.md` (`Quests` lists are priority-ordered, so the walk is too)
and offer up to the first five. Append any ready quest the walk never reached
and flag it as unlisted - that is a tree defect worth fixing.
Include any relevant higher-level context, such as the goal, parent questline, and/or what completing it unblocks.
Include each candidate's title size.

After the user chooses, read the quest and the relevant repository guides.
Decide the base branch, create a worktree on the quest's branch, and push an empty placeholder commit whose message contains a freshly generated UUID to claim it (skip the push without write access).
Never force this push. A rejected push loses the claim race; stop and choose another quest.
Start implementation when its scope is decision-complete; otherwise use `$plan-quest` to settle it first.
