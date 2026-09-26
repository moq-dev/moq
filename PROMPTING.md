Read this before indirectly instructing agents.

LLMs are not trained on how to write prompts.
They're trained on the outputs, not the inputs, and thus generate slop when chained.
So when you're drafting instructions for a future agent, please keep these rules in mind.

# General

**Do not over-prescribe**.
The facts will evolve over time.

Focus on articulating the goal and/or root problems.
Keep things flexible so an agent can adjust as it digs deeper into the possible problems or solutions.

You can (and should) include recommendations.
But focus on best-practices and things to look out for, not specific lines of code.
Mocking up an API makes a lot of sense but again, it should be flexible enough to adjust with the implementation.

# Context

This includes editing any `AGENTS.md`, `CONTRIBUTING.md`, etc.

These files need to be light because they're loaded into every prompt.
More specific context (ex. per language or project) should exist in optional files/folders.

These base prompts are meant to fix regular agent mistakes.
If there is a history of bad practices, or sub-optimal behavior, update `AGENTS.md` to steer away from them.
If you think a slight tweak to the base prompt would help, propose it.

Focus on best practices and conventions.
Don't document the repository; the code and documentation can handle that.

Before changing agent-instruction files, check the relevant official Claude/Codex guidance:

- <https://code.claude.com/docs/en/memory>
- <https://code.claude.com/docs/en/best-practices>
- <https://code.claude.com/docs/en/skills>
- <https://platform.claude.com/docs/en/build-with-claude/prompt-engineering/claude-prompting-best-practices>
- <https://developers.openai.com/codex/guides/agents-md>
- <https://agents.md>

# Skills

Skills are meant to avoid repeated duplicate prompts.
If the usage of a skill is unclear, offer to update to skill to reduce the ambiguity.
