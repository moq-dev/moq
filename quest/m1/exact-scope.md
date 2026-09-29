# [S] An exact broadcast outside a reader's scope stays hidden

## Goal

A reader scoped to `/a/b` never sees an exact broadcast at `/a`, in Rust or
JS. A prefix advertisement at or above the scope still presents as the empty
path, because it may serve paths under the scope; an exact broadcast cannot.

## Plan

Decided 2026-09-28: exact match everywhere, aligning on the JS local view.

- Rust: `sync_cursor` in `rs/moq-net/src/model/origin.rs` admits an exact
  entry by prefix overlap. Match exact entries against the reader's patterns
  instead. `cursor_keeps_an_overlapping_prefix_above_its_scope` covers a
  prefix route and stays.
- JS wire view: `Scope.projectRoutes` in `js/net/src/origin.ts` treats every
  entry above the root as covering. Skip exact entries there, as `#listed`
  already does with `candidate.exact ? pattern.matches(path)`.
- Add the same case to both languages' tests: an exact broadcast at `/a` and
  a prefix route at `/a`, read through a `/a/b` scope, yield only the prefix.

Public API: none. Wire: a narrower reader receives fewer announcements; no
message changes.
