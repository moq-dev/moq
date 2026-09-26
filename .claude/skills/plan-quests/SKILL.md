---
name: plan-quests
description: Scope, create, and publish a quest through an interactive grilling interview. Use when the user invokes /plan-quests, asks to plan a quest, or wants unsettled work split into quests.
---

Before you begin, read `quest/AGENTS.md` completely.

Interview the user until you reach a shared understanding.

Work the tree in **rounds**.
The **frontier** is every decision whose prerequisites are already settled.
Ask the whole frontier in one round, interactively if supported.
Select at least one answer as (recommended) and wait for the user's answers (never guess) before the next round.

Each round the user answers reshapes the tree: settled decisions push the frontier outward and unblock questions that depended on them.
Recompute the frontier and ask the next round.
A question whose answer depends on another question still open in this round belongs to a *later* round, not this one.

Finding *facts* is your job, never the user's.
When a frontier question needs a fact from the environment (filesystem, tools, etc.), dispatch a sub-agent to find it.
Don't block on it, ask the rest of the frontier now.
The *decisions* are the user's: put each to them and wait.

Search other quests and questlines to keep the larger plan consistent.
When the work changes what a user sees (a wire, an API, a flag, a dashboard), ask whether it needs documentation the feature quest cannot carry inline (a new page or guide), and recommend a quest for that; docs a change makes stale stay in that change.
When the frontier disagrees with a settled quest/plan, challenge the user and resolve the conflict.

Begin the interview by scoping the goal: the observable outcome, why it matters, and its important boundaries and non-goals.
Restate the goal in one sentence and get it confirmed before moving on to implementation decisions.
If the goal contains independently completable outcomes, split them before planning.
Map the implementation plan as a design tree: every material decision branches into the decisions that hang off it.

The session is done when the frontier is empty.
The result may be one quest or multiple quests and questlines, split based on what can be completed independently.
Prefix each quest title with `[XS]`, `[S]`, `[M]`, `[L]`, or `[XL]`, including implementation, verification, and landing work.
Once complete, create, update, or delete the relevant quests and questlines.
Record each settled decision and its reason in the quest's Plan, so later sessions don't ask it again.
New work joins the milestone matching its priority, at its rank; a questline groups only quests that ship together, and its README holds the work no child owns (the end-to-end test, the docs page).

When done, commit and create a draft PR following `CONTRIBUTING.md`.
After local checks pass, mark it ready and monitor CI and the automatic reviews.
Address one review round, then stop and report if the next review still has findings.

Merge the PR when ready.
