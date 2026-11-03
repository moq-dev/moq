MoQ (Media over QUIC) is a live media delivery protocol providing real-time latency at massive scale.

# Context

Start with [doc/setup/repository.md](doc/setup/repository.md) to locate a feature, its examples, and its tests.
Trust this checkout's source over published API references, which may describe another version.
`doc/` is for end-users and advertising the available features.

# Required

- Immediately disable a flaky test and plan a follow-up to investigate.
- The same identity (ex. broadcast+track+group+frame) always has the same content. Use a new identity for new content.
- Never match on errors; it's brittle.
- moq-lite and moq-net take priority over moq-transport and other IETF drafts, which are a low-effort compatibility layer. Ignore their requirements (even MUSTs) that add cost without a clear behavioral benefit, and never add API for functionality only they have.

# Guidelines

- Impair networks with bufferbloat, queuing until packets drop, instead of random loss.
- Anything that fans out gets a benchmark swept over both axes.

# Public API

The API is the most important thing to get right: a bad shape costs a breaking change in every language.

- Feature PRs add migration notes under `## Unreleased` in `MIGRATE.md`; only the release cut writes version sections.
- Replace a callback parameter with a handle, an event, or a Producer/Consumer split.
- Nest related API, configuration, and output fields under their shared role.
- Propose alternatives when a name or shape feels awkward.
- Short names under a module namespace (`encode::Config`, not `EncoderConfig`), mirrored across Rust, JS, and the bindings.
- Keep package boundaries modular and reusable; prefer a module in an existing package over a one-off package.

# Development

PRs target `main`, where breaking API changes are allowed.
`release` is what ships; touch it only to cut a release or backport a fix or benchmark (see `CONTRIBUTING.md`).
Wire changes stay backwards compatible with *published* drafts/versions; an unpublished `-wip` version changes freely.
Our libraries prefer moq-lite, so moq-transport compatibility only matters against third-party implementations.

Build output may live outside the checkout and gets wiped, so scratch never goes in `target/`.

```bash
just check   # Lint, compile, and test what the branch changed
just fix     # Auto-fix lint/formatting, same scope
```

Quests: before starting work, run `quest context` and `quest guide`, and follow both.
The `quest` binary and its skills come from the moq-dev/quest flake input; change them upstream and bump the input.
`quest init` refuses Claude Code's worktree hooks: drop them from `.claude/settings.json` for its run, then restore them.

# Cross-Package Sync

`moq-ffi` powers every SDK except Rust and JS. Keep all languages and wrappers at the same functionality.
A wire-format change updates its matching draft in the same PR: framing, fields, enum values, and version negotiation.
