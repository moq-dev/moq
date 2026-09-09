# Commits

PRs are squash-merged, so the PR title becomes the commit subject and the PR description becomes the body in `git log`.

- Use conventional-commit subjects (`feat(watch): ...`, `fix: ...`, `chore: ...`, `docs: ...`)
- AI commit attribution goes in a `Co-Authored-By:` trailer, not the commit body.
- Never commit binaries or build artifacts (`.a`, `.so`, `.dylib`, `.dll`, wheels).

# PRs

Keep the body short and structured, not narrated.

- **Summary**: a few bullets on what changed and why. For a bug fix, state the root cause.
- **Public API**: every new/renamed/removed/updated exported item, with breaking ones called out.
- **Wire**: any change to the on-the-wire format, and the draft under `drafts/` updated with it.

When pushing additional commits to an existing PR, update the title and description if needed.
When taking over someone else's PR, push commits on top of theirs so they keep credit.

Create a draft PR.
Switch it to "Ready for review" when you're finished and local `just check` and `just test` pass.
Fix any merge conflicts and failing CI checks.

# AI

AI-assisted issues, pull requests, reviews, and comments are welcome.
GitHub issues are the public front door for brainstorming. Prefer a quest for work needing durable scope or coordination.

Add the AI marker `(Written by <model>)` to any posts on GitHub, excluding commit messages that contain `Co-Authored-By:` trailers.

# Reviews

Codex and CodeRabbit review every push on their own.
Never request a review from @codex or @coderabbitai.

Codex reacts with thumbs up if there are no findings.
CodeRabbit may be rate-limited, treat it as optional.

For each finding:

- If you don't agree with it, reply to the finding and move on.
- If it's a simple improvement, fix it and push. Update the summary if needed.
- If it's out of scope, trigger `/plan-quests` to create/update a follow-up quest.

Otherwise, interactively prompt the user what to do next, including a recommended course of action.
