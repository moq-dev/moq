Read this before editing any `CLAUDE.md`, `AGENTS.md`, `CONTRIBUTING.md`, or `SKILL.md`. These files are prompts.

Agents are not trained to write prompts for themselves.
They're (currently) trained on the outputs, not the inputs.

# Context
Focus on best practices and conventions.
Don't document the repository; the code and documentation can handle that.

These files need to be light because they're loaded into every prompt.
More specific context (ex. per language or project) should exist in respective folders.

These base prompts are meant to keep agents from constantly drifting in the wrong direction.
There needs to be a history of misuse to justify a change to `CLAUDE.md` and friends.
If you think a slight tweak to the base prompt would help, make it.

Check the official Claude/Codex guidance when making any changes.

# Skills
Skills are meant to avoid repeated duplicate prompts.
Read transcripts to determine the user's repeated intent.
