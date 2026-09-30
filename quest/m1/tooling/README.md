# Tooling: thin justfiles and CI that calls them

## Goal

A month of merges left the command surface bloated: 873 lines of root
`justfile`, seven self-tests of the tooling itself on every pull request,
three metadata guards, recipes nothing calls, and release workflows that are
the same file with a binary name swapped. The result is a `just` tree that is
a menu, not a language: recipes stay thin, and any non-trivial logic
(conditionals, loops, traps, background jobs) lives in a script under `sh/`
or the package's own script directory, which the recipe and CI both call. The
diff is resolved once by one impact map, and every workflow step runs a recipe
rather than a script path.

## Plan

`just` stays as the entry point because the vocabulary (`just check`, `just
fix`) is in every doc, skill, and workflow. Its cost was
self-inflicted: logic inside recipes.

A recipe that runs a short fixed sequence of commands is thin and stays
inline; line count is not the test.

## Required

- [Check scope](/quest/m1/tooling/check-scope.md) - check.yml's build-or-skip decision comes from sh/dispatch.sh instead of an inline diff grep
