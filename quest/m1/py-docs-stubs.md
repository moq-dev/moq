# [XS] Python docs rebuild their API stubs

## Goal

A renamed or removed binding symbol never breaks `just py docs` through
stale generated stubs.

## Plan

`py/justfile`'s `docs` recipe clears only `dist/docs`. The gitignored
autosummary stubs in `py/moq-rs/docs/api/` survive between builds, so
`sphinx-build -W` fails on a symbol that no longer exists. This broke
`just check` for several agents. Clear that directory in the recipe.

Public API: none. Wire: none.
