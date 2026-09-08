---
name: plan-quests
description: Scope, create, and publish a quest through an interactive grilling interview. Use when the user invokes /plan-quests, asks to plan a quest, or wants unsettled work split into quests.
---

Before you begin, read `quest/CLAUDE.md` completely.

Interview the user until you reach a shared understanding.

Work the tree in **rounds**.
The **frontier** is every decision whose prerequisites are already settled: the questions you can ask *now* without guessing at answers you haven't heard yet.
Ask the whole frontier in one round, interactively if supported.
Select at least one answer as (recommended).
Wait for the user's answers before the next round.

Each round the user answers reshapes the tree: settled decisions push the frontier outward and unblock questions that depended on them.
Recompute the frontier and ask the next round.
A question whose answer depends on another question still open in this round belongs to a *later* round, not this one.

Finding *facts* is your job, never the user's.
When a frontier question needs a fact from the environment (filesystem, tools, etc.), dispatch a sub-agent to find it.
Don't block on it, ask the rest of the frontier now.
The *decisions* are the user's: put each to them and wait.

Search other quests and questlines to keep the larger plan consistent.
When the frontier disagrees with a settled quest/plan, challenge the user and resolve the conflict.

Begin the interview by scoping the goal: the observable outcome, why it matters, and its important boundaries and non-goals.
Do not move on to implementation decisions until the goal is settled.
If the goal contains independently completable outcomes, split them before planning.
Map the implementation plan as a design tree: every material decision branches into the decisions that hang off it.

The session is done when the frontier is empty.
The result may be one quest or multiple quests and questlines, split based on what can be completed independently.
Prefix each quest title with `[XS]`, `[S]`, `[M]`, `[L]`, or `[XL]`, including implementation, verification, and landing work.
Once complete, create, update, or delete the relevant quests and questlines.

Commit and make a PR then:

- Monitor it for CI failures and reviews.
- Address any automated review findings (Codex/CodeRabbit) you agree with. Turn down any you disagree with with a comment.
- Push any changes you made to the PR, updating the summary if needed.
- Repeat at most once, avoiding a loop of back-and-forth review/feedback cycles.

Merge the PR when ready.
