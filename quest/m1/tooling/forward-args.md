# [S] Recipes forward arguments intact

## Goal

`just rs test`, `check-test`, and `capture-test` interpolate `{{ args }}`, so
just joins the arguments before the shell parses them and a quoted nextest
filterset such as `-E 'test(a) | test(b)'` becomes a shell syntax error
(#4342). Recipes that forward arguments pass each one through unchanged.

## Plan

- Recipes that forward arguments use `[positional-arguments]` and `"$@"`
  instead of `{{ args }}`, starting with the three above and covering the
  other forwarding recipes in `rs/justfile`.
- No self-test for it: the line deleted the tooling's self-tests from every
  pull request, and this adds none back.

Public API: none. Wire: none.

## Closes

- [#4342](https://github.com/moq-dev/moq/issues/4342) - close this issue when the quest finishes
