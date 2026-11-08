# [XS] A lint rejects expect(clock.drive(...)).rejects and .resolves

## Goal

A JS test can't wrap a `Manual` clock's `drive` in
`expect(...).rejects` or `expect(...).resolves`. A lint rule rejects the
pattern with a message pointing at the fix.

## Plan

Planned 2026-11-07 (maintainer, /quest-complete), a follow-up of
[moq-dev/moq#8226](https://github.com/moq-dev/moq/pull/8226).

Bun's `expect(...).rejects` and `.resolves` run a nested event loop, which
silently breaks the order `drive` relies on, and the resulting failure looks
like a clock bug. Today only a doc line in `@moq/clock` warns. Add a
`noRestrictedSyntax`-style rule in `biome.jsonc` (or a GritQL plugin if
biome's built-in rules can't match it) that flags `drive` or `driveUntil`
called inside `expect(...)` followed by `.rejects` or `.resolves`. Its message
says to await `drive` first and assert on the settled result. Fix any existing
matches.

Public API: none. Wire: none.
