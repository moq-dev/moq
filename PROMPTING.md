# Prompting agents

Read this before editing any `CLAUDE.md`, `AGENTS.md`, `CONTRIBUTING.md`, or `SKILL.md`. These files are prompts. Agents are not trained to write prompts for themselves, so the rules below come from the vendors' own guidance (sources at the end) and from what has failed here.

# What belongs

A guide holds what an agent cannot derive from the tree and would otherwise get wrong twice: commands it can't guess, conventions that differ from the language default, invariants no type check enforces, footguns that fail silently, and repository etiquette. Everything else buries the rules that matter.

- Add a line when the same mistake happens a second time, a review catches something the guide should have said, or the same correction gets typed into chat twice.
- Cut a line if removing it would not cause a mistake. Ask that of every line, including the ones already there.
- Never add directory trees, file-by-file descriptions, dependency lists, architecture overviews, how a feature works, or the history of a change. A PR that explains its own change in a guide is adding history.
- Never add a rule an agent already follows. "Write clean code" and restated language conventions cost tokens and adherence.
- A multi-step procedure is a skill. A rule that must hold every time is a hook; guides are advisory.
- Deferred work is a quest, never a note in a guide.

# Where it goes

- The root guide: rules that apply to every change in the repository.
- A nested guide: rules for one area, loaded only when an agent works there. Put a rule in the narrowest directory it applies to.
- `CONTRIBUTING.md`: how a change lands, from commit to merge, including how automated reviews are handled.
- A skill: a procedure someone runs on demand.
- Auto memory: one person's preferences and corrections. Never a repository fact.

# How to write it

- Short. Under 200 lines per file; Codex truncates the whole concatenated chain at 32 KiB, nearest file last.
- Concrete enough to verify. "Run `just fix` before committing" beats "keep the code formatted". Name the exact command, flag, or type.
- Say what to do and why, in one sentence. The why lets the model generalize; a bare "never X" is followed literally or not at all.
- A limit is a number, not a judgment. "One fix round, then stop" holds. "Avoid loops" and "abort if not making progress" do not, because an agent always believes it is making progress.
- No emphasis. Capitals and "IMPORTANT" work on one line per file; on ten, none stand out.
- No contradictions. Between two rules that disagree, the model picks at random. Grep the other guides for the topic before adding a rule.
- Facts, not narration. State the invariant and the failure it prevents. Skip the story of how it was found.
- Prefer the positive form. "Bind `[::]`" over "don't bind v4-only".
- Show a short example when the rule is about shape (a name, a message format). Three lines of example beat a paragraph.
- Plain markdown: headers and bullets, one topic per section, no em dashes. Claude and Codex read the same file, so nothing tool-specific.

# Skills

A skill loads once and stays in context for the rest of the session.

- The `description` decides when it triggers. Lead with the use case and the phrases a user would say; two sentences at most.
- Under 50 lines. Reference material goes in a sibling file, linked from the skill.
- Standing instructions, not one-time steps: the skill is not re-read later in the session.
- What to do, not why. The why belongs in a guide, if anywhere.
- Every loop has a numeric cap and a stop condition that hands control back to the user.
- A skill with side effects (deploy, post, merge) sets `disable-model-invocation: true` so only a person starts it.
- One job per skill. "Review, fix, document, and plan" is four skills.

# Prompting a subagent or Codex

- One task per run. Say what done looks like and the output shape you need.
- Say what to verify before finishing, never "think harder".
- Name what to read, and that unknowns are reported rather than guessed.
- Keep going on low-risk ambiguity; stop only when a missing detail changes correctness, safety, or an irreversible action.
- For a run that writes, bound the scope: no unrelated refactors, no new abstractions, no defensive code for cases that can't happen.

# Sources

- Claude Code, CLAUDE.md: <https://code.claude.com/docs/en/memory>
- Claude Code best practices: <https://code.claude.com/docs/en/best-practices>
- Claude Code skills: <https://code.claude.com/docs/en/skills>
- Prompting Claude: <https://platform.claude.com/docs/en/build-with-claude/prompt-engineering/claude-prompting-best-practices>
- Codex AGENTS.md: <https://developers.openai.com/codex/guides/agents-md>
- AGENTS.md format: <https://agents.md>
