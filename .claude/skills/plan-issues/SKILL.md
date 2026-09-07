---
name: plan-issues
description: Plan quests from open GitHub issues without the quest label. Use when the user invokes /plan-issues or asks to turn unplanned GitHub issues into quests.
---

Read and invoke [plan-quests](../plan-quests/SKILL.md) for the selected issues, using its full interview and publishing workflow.

Default to the current repository's open GitHub issues without the `quest` label. Use any provided arguments to narrow that scope. Fetch all matching issues, paginating rather than silently limiting the results, and read their bodies and comments before planning. If no issues match, report that and stop.

Present the eligible issues and use them as input to the plan-quests interview. Reuse facts and settled decisions from the issue discussions; ask the user about unresolved decisions. Related issues may share an interview, but preserve independently completable outcomes as separate quests.

Search existing quests and history for each issue before creating a duplicate. Include each source issue under `Closes` in the appropriate quest or questline. Recheck that an issue is still open and lacks the `quest` label before planning it.

After the quest PR merges, add the `quest` label to the issues covered by the merged plans. Leave the issues open for the implementation PRs to close. Report any issues left unplanned and why.
