# [M] Fix origin::Producer::mount scope check, router edit order, settle ended state, and Live::find

## Goal

`origin::Producer::mount` enforces `self.scope.excluded`, `Router::recv(Msg::Edited)`
removes before adding, `route::Node::settle` preserves deferred `ended` state
across `Confirm::Waiting`, and `Live::find` checks coverage across batches.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the exhaustive pre-cut
full-codebase audit): ✅ place high-severity and impactful correctness bugs in
`m1` (rejected: `m0` with `release` backports; `m3`).

1. **Enforce `self.scope.excluded` in `origin::Producer::mount`
   (`rs/moq-net/src/model/origin.rs:2973-2975`).**
   - `mount` currently verifies prefix inclusion without checking overlap with
     `self.scope.excluded`, allowing a mounted producer to widen scope into an
     explicitly excluded subtree.
   - Reject mounts that overlap `self.scope.excluded` (or narrow/intersect the
     child scope cleanly so excluded subtrees remain unreachable; match the
     existing scope-narrowing contract in `origin.rs`).
2. **Apply `edit.remove` before `edit.add` in `route::Router::recv(Msg::Edited)`
   (`rs/moq-net/src/route/relay.rs:1354-1378`).**
   - Applying `add` before `remove` corrupts state when an edit replaces or
     re-inserts an entry under the same key/path in a single message.
3. **Preserve `ended` state on deferred leaving slots in `route::Node::settle`
   (`rs/moq-net/src/route/node.rs:687,927,932-958`).**
   - When `settle` defers removal because of `Confirm::Waiting`, the slot's
     `ended` flag is lost on the retry pass; persist `ended` per leaving slot
     until removal completes.
4. **Fix batch boundary check in `Live::find`
   (`rs/moq-net/src/model/track.rs:8787`).**
   - Change the condition to `if !caught_up || indexed.cover != *groups` so a
     batch that has not caught up or does not cover the requested group span
     does not prematurely treat the lookup as complete.

Public API: none. Wire: none. Add targeted unit/model regression tests in
`rs/moq-net` for each of the four bugs.
