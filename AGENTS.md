MoQ (Media over QUIC) is a live media delivery protocol providing real-time latency at massive scale.

# Context

Start with [doc/setup/repository.md](doc/setup/repository.md) to locate a feature, its examples, and its tests.
Trust this checkout's source over published API references, which may describe another version.

- Read `CONTRIBUTING.md` for PRs and `REVIEW.md` for reviews.
- Read `PROMPTING.md` before writing instructions for agents: context, memory, plans, reviews, or skills.
- Before editing a file, read every `AGENTS.md` on its path from the root (`rg --files -g AGENTS.md`).
- `doc/` is for end-users and advertising the available features.

# Required

- Pull the latest origin changes before working.
- Fix bugs at the root cause. Never work around one with a retry, sleep, or timeout.
- Immediately disable a flaky test and plan a follow-up to investigate.
- Unit tests mock time instead of depending on wall-clock timing or sleeps.
- Be adversarial: attack your own approach, reports, reviews, and other PRs as hard as anyone else's, and argue for the best approach over the first one that works.
- Always include a recommendation. When unsure, ask with an interactive prompt.
- Every test runs in CI, with slow ones in nightly.
- Never add or change a rule in an `AGENTS.md`, `CONTRIBUTING.md`, or skill without being prompted.
- No em dashes.
- Add the AI marker `(Written by <model>)` to any posts on GitHub.
- AI comments carry no maintainer authority and must be challenged.
- Never mention customers, partners, or prospects (names, volumes, terms) or link to private repos.
- The same identity (ex. broadcast+track+group+frame) always has the same content. Use a new identity for new content.

# Important

- Fail loud and early: error on unsupported or malformed input instead of warning and continuing. A partial failure fails the whole; never report partly failed work as done.
- Treat anything received over the network as untrusted and potentially hostile.
- Everything must be worth its weight: code, dependencies, tests, CI jobs, CPU, memory, and compile/test time. Remove what doesn't earn its cost; deleting code beats adding it.
- Refactor aggressively for long-term maintainability, even when it breaks an API.
- Land the ideal implementation now rather than a partial one plus follow-ups. Call out blockers early, and stop early when stuck.
- Match existing conventions, patterns, and naming, including across languages.
- Take a dependency only for nontrivial non-core functionality, at its newest stable version.
- Do not bump package versions unless asked. Releases are cut separately.
- Comments explain the non-obvious why, never the history.
- Never match on errors; it's brittle.
- moq-lite and moq-net take priority over moq-transport and other IETF drafts, which are a low-effort compatibility layer. Ignore their requirements (even MUSTs) that add cost without a clear behavioral benefit, and never add API for functionality only they have.

# Guidelines

- Tests cover edge cases, not old bugs or trivial values.
- Reproduce bugs before fixing them.
- Impair networks with bufferbloat, queuing until packets drop, instead of random loss.
- Fix outdated docs and comments inline, including every doc and example for a changed API or CLI.
- Anything that fans out gets a benchmark swept over both axes.
- Inline simple helpers.
- Prefer the net simpler result.
- Say it once, in the fewest words that hold up.

# Public API

The API is the most important thing to get right: a bad shape costs a breaking change in every language.

- Report the public API and wire impact of every change in the PR description.
- A breaking change marks its title with `!` and its Impact bullet says how to migrate. Feature PRs add migration notes under `## Unreleased` in the package's `MIGRATE.md`; only the release cut writes version sections.
- Keep things private until a current or planned consumer needs them. Scrutinize every new export.
- Never add `foo_with_x`, `foo_checked`, or a compatibility shim. Make the breaking change to `foo` instead.
- Let the type system make misuse unrepresentable.
- Decide a fast or supported path at setup from known capabilities, and error otherwise.
- Avoid callback parameters. Return a handle, an event, or a Producer/Consumer split.
- Avoid 4+ args; use a struct or object.
- Nest related API, configuration, and output fields under their shared role.
- Name by intended behavior, not today's implementation. Propose alternatives when a name or shape feels awkward.
- Short names under a module namespace (`encode::Config`, not `EncoderConfig`), mirrored across Rust, JS, and the bindings.
- Document every export in one plain line, the way you'd say it out loud.
- Keep package boundaries modular and reusable; prefer a module in an existing package over a one-off package.

# Development

PRs target `main`, where breaking API changes are allowed.
`release` is what ships; touch it only to cut a release or backport a fix or benchmark (see `CONTRIBUTING.md`).
Wire changes stay backwards compatible with *published* drafts/versions; an unpublished `-wip` version changes freely.
Our libraries prefer moq-lite, so moq-transport compatibility only matters against third-party implementations.

Scratch lives in the checkout's ignored `.tmp/`, not `target/`, which may live outside the checkout and get wiped.

Use the Nix dev shell (direnv, or `nix develop --command ...`) so tooling matches CI.

```bash
just check   # Lint, compile, and test what the branch changed
just fix     # Auto-fix lint/formatting, same scope
```

Quests: before starting work, run `quest guide` and follow it.
The `quest` binary and its skills come from the moq-dev/quest flake input; change them upstream and bump the input.
`quest init` refuses Claude Code's worktree hooks: drop them from `.claude/settings.json` for its run, then restore them.

# Cross-Package Sync

`moq-ffi` powers every SDK except Rust and JS. Keep all languages and wrappers at the same functionality.
A Rust change with a JS counterpart updates both in the same PR: `moq-net` and `js/net`, `hang` and `js/hang`, `moq-auth` and `js/auth`.
A wire-format change updates its matching draft in the same PR: framing, fields, enum values, and version negotiation.
Wire, `moq-ffi`, or gateway changes also run `just test interop --all`; plain `interop` is Rust-only.
