# Commits
PRs are squash-merged, so the PR title becomes the commit subject and the PR description becomes the body in `git log`.

- Use conventional-commit subjects (`feat(watch): ...`, `fix: ...`, `chore: ...`, `docs: ...`)
- AI commit attribution goes in a `Co-Authored-By:` trailer, not the commit body.
- Never commit binaries or build artifacts (`.a`, `.so`, `.dylib`, `.dll`, wheels).

# PRs
Keep the body short and structured, not narrated.

- **Summary**: a few bullets on what changed and why. For a bug fix, state the root cause.
- **Public API**: List every new/renamed/removed/updated item, with breaking ones called out.

When pushing additional commits to an existing PR, update the title and description if needed.

# AI
AI-assisted issues, pull requests, reviews, and comments are welcome.
If the right solution is not obvious, open an issue before writing code so contributors and maintainers can brainstorm the approach together.

Add the AI marker `(Written by <model>)` to any posts on GitHub, excluding commit messages that contain `Co-Authored-By:` trailers.

# Reviews
Codex and CodeRabbit automatically review PRs.
CodeRabbit may be rate-limited; don't require it to produce a review.

You should address review comments or leave a comment if you disagree with a suggestion.
If a review comment is out of scope or not relevant to the PR, make or update a follow-up quest.
