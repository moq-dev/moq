---
name: plan-quest-issues
description: Triage every open GitHub issue that no quest tracks, then run the plan-quest interview for each one worth a quest, in one PR. Use when the user invokes /plan-quest-issues, optionally with issue numbers, or asks to turn new issues into quests.
---

Before you begin, read `quest/CLAUDE.md` completely.

An issue is tracked when it carries the `quest` label. Resolve the arguments
to a set of issue numbers, defaulting to every open issue without that label:

```bash
gh issue list --repo moq-dev/moq --state open --limit 500 --json number,title,labels,createdAt,author \
  --jq '.[] | select(all(.labels[]; .name != "quest")) | "\(.number)\t\(.createdAt[:10])\t\(.author.login)\t\(.title)"'
```

`git fetch origin` first. Grep `quest/` on both `origin/main` and
`origin/dev` for `issues/<n>` under a `## Closes` heading: an issue a quest
already closes is only missing its label, so apply the label and drop it from
the pool without asking.

Read every remaining issue in full, including its comments, and show the last
maintainer comment beside each one: an issue triaged as **leave** on an
earlier run has no marker, so that comment is where the earlier verdict lives.
Dispatch sub-agents for the facts a verdict needs (is it already fixed on
`dev`, does an existing quest cover it, does the premise hold against the
code) before putting anything to the user.

Put every issue to the user in one batch, oldest first, each with a
recommendation and the one fact behind it. The choices are **quest** (the
issue gets a quest or joins an existing one's `Closes`), **close** (done,
disproven, or superseded; say which), and **leave** (no quest, stays open).
Do not spend a round per issue.

Then run the `$plan-quest` interview for each **quest** pick in turn, on one
branch, so decisions shared across issues are asked once. A quest that only
adds an issue to an existing quest's `Closes` needs no interview beyond
confirming the fit.

For each **close** pick, post one closing comment that names the commit,
quest, or finding, ending with the `(written by <model>)` line, then close the
issue. The batch answer is the approval; do not ask again per issue.

Finish per `$plan-quest`: `just check`, commit, one PR for the whole tree.
The label tracks a landed quest, so wait for the PR to merge before applying
the `quest` label to every issue the new or updated quests list under
`Closes`; an abandoned PR leaves the issues unlabeled for the next run. Then
offer to start the ready quests.
