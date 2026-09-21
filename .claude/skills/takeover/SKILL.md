---
name: takeover
description: Adopt someone else's open PR and drive it to landable.
---

Someone else opened this PR; you are now responsible for it.
If unsure about any course of action, prompt the user for guidance.

- Parse the arguments to determine the PR number.
- Read the PR, any linked issues, reviews/comments posted on the PR, and the surrounding code.
- Judge the approach before touching anything. Would you take a different approach?
- Fix any issues with the PR, such as merge conflicts and failing CI checks.
- Address any automated review findings (Codex/CodeRabbit) you agree with. Turn down any you disagree with with a comment. One round only; never request a review.
- Push any changes you made to the PR, updating the summary if needed.
- Summarize the changes made and any potential follow-up actions.
- Do not merge the PR; that is the `merge` skill's job, and only once the user approves.
