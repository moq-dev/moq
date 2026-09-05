---
name: spawn-quest
description: Triage every quest in a scope interactively, then spawn background agents to open a PR for each one worth starting now. Use when the user invokes /spawn-quest, optionally with a scope such as /spawn-quest m0, or asks whether a milestone's quests should be worked on now.
---

Before you begin, read `quest/AGENTS.md` completely.

Resolve the first argument to a questline directory under `quest/`, defaulting
to `quest/` itself: `m0` means `quest/m0`.

`git fetch origin` **before** looking at claims. The remote branch is the
coordination mechanism, so stale remote-tracking refs mean offering a quest
another agent already claimed, and the spawned agent finds out only when its
push is rejected.

Read every quest in the scope, not just the summaries in each `README.md`. Drop
the ones a recent branch or open PR already claims.

Resolve a stale branch (old, no open PR) before keeping its quest in the start
pool, rather than only mentioning it. Left in place it makes the agent's claim
push fail as non-fast-forward, which the agent then reports as a lost race that
never happened.

Inspect what the branch carries beyond its base. A claim placeholder is itself
a commit, so "carries no work" is not the same as "has nothing unmerged", and
conflating the two strands a quest forever:

- **Placeholder commits only.** Someone claimed the quest and abandoned it
  without starting. Delete the ref and keep the quest in the pool: nothing is
  lost, and treating it as claimed would retire the quest permanently on the
  strength of an empty commit. Pin the remote delete to the tip you inspected,
  with `git push --force-with-lease=<branch>:<sha> origin --delete <branch>`.
  An unconditional delete races the owner coming back, and would erase a real
  commit pushed between the inspection and the delete along with the claim it
  renewed. A rejected lease means that is what happened, so treat the quest as
  claimed. Delete the local ref too when it points at that same tip, or the
  agent cannot cut the branch it was told to cut.
- **Real commits.** Do not cut a fresh branch over them. Hand the agent the
  existing branch to continue, rebased onto its base. It still claims, with a
  fresh UUID placeholder pushed under a lease pinned to the tip you inspected.
  Two runs that both adopt the same branch would otherwise both skip the claim,
  and a rebase that changes nothing gives neither of them the rejected push
  that decides the race.
- **Checked out in a worktree, or carrying an open PR.** Treat the quest as
  claimed and take it out of the pool.

Put every quest to the user in priority order - the depth-first walk of the
scope's `Quests` lists - with a recommendation and the one fact behind it.
Batch the questions; do not spend a round per quest. The choices are **start**,
**plan** (use `$plan-quest` when a material decision is unmade), **move**,
**delete**, and **leave**.

Offer **start** only for a ready quest: `quest/AGENTS.md` executes one only when
it has no `Required` section. For work an agent cannot do - a decision that is
a conversation, a credential only the user can mint, or verification this
machine cannot run, such as a Linux-only build or a benchmark whose exit
criteria is a measured before/after - recommend **leave** and name what would
unblock it.

## Spawning

Settle a base branch **per quest** and pass it to that quest's agent. Apply the Branch Targeting rules in `CLAUDE.md`: `dev`
only for a semver break in a published API, `main` for everything else. One
scope mixes both, so a single base for the whole wave puts some PR on the
wrong branch. Never let an agent derive its own base: an agent branching from
wherever it stands puts this skill's own tree edits in its PR.

Spawn one background agent per **start**, each with `isolation: "worktree"`, in
waves of two or three unless the machine is idle - a saturated box makes agents
ship unverified. Never edit a running agent's worktree; message it instead.

Give each agent the base branch, the quest path, and nothing it could read for
itself. Instruct it to:

- Read `quest/AGENTS.md`, the quest, `CONTRIBUTING.md`, and the guides for the
  areas it touches.
- Cut the quest branch (the quest path without `.md`) from that base and claim
  it with an empty placeholder commit whose message contains a freshly
  generated UUID, pushed immediately with `git push origin HEAD` after
  pointing the upstream at the base. When triage hands over an existing branch to
  continue, rebase it onto the base and claim it the same way, but push the
  placeholder with `--force-with-lease` pinned to the tip triage inspected, so
  a second adopter loses the race rather than joining it. The UUID is what makes the claim a race:
  two agents claiming the same quest from the same base in the same second
  otherwise produce the same commit object, and the loser's push succeeds as
  already-up-to-date instead of being rejected. A rejected push lost the race:
  stop and report. Never force the claim push; `--force-with-lease` pinned to
  its own claim is fine after a rebase.
- Implement, run `just fix`, `just check`, and `just test` through
  `nix develop --command`, exercise the changed surface for real, walk the
  Cross-Package Sync table, and open a PR per `CONTRIBUTING.md`, deleting the
  quest and every reference to it in that same PR.
- Leave package versions and lockfile version metadata alone.
- Name in the PR any check that did not run, rather than claiming it or leaving
  it out.
- Never deploy, apply, or mutate anything outside the repository.
- On a blocker: write what it learned into the quest so the next attempt starts
  ahead, push that, then stop and report rather than improvising past it.

**The scope ends at the open PR.** Neither you nor the agents merge one, and
nobody waits on CI. Report each PR as it lands, raise blockers for the user to
decide, and hand back what is still open.
