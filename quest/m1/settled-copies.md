# [S] Settled copies versus whole groups

## Goal

Every check in moq-net's model that asks whether a group is done uses the
meaning it needs: a copy whose writer finished (`Handle::is_settled`) or the
whole group reaching its end (`is_finished`). A finished bounded copy is
never mistaken for the whole group, and never kept open as unfinished.

## Plan

Decided 2026-10-28 (maintainer), from
[moq-dev/moq#6922](https://github.com/moq-dev/moq/pull/6922): audit every
caller at once instead of fixing them as reviews find them. Three review
rounds on #6922 each fixed one site (track teardown, the track's clean end,
and uncached copies).

- List every `is_finished`, `is_settled`, `serves_end`, and `cut` caller in
  `rs/moq-net/src/model/` and state which meaning each needs.
- `tear_down`'s `open` and `lookup.retain` still use whole-group
  `is_finished`. That looks deliberate, since a bounded copy does not serve
  the group's end, but confirm it.
- Each wrong caller gets a test that fails before its fix. Callers that are
  right need no test.

Public API: none. Wire: none.
