# [XS] moq-tokio: one abort-on-drop guard instead of five

## Goal

One type per crate owns "abort this task when its owner drops", and every site
that wants that behavior uses it.

## Plan

Dev only, which is why this sits in m1. Five copies of the same `Drop` exist
today:

- `rs/moq-tokio/src/worker.rs`: `AbortOnDrop<T>(JoinHandle<T>)`, awaited through
  after the guard is moved, so the shared type has to keep the handle reachable.
- `rs/moq-tokio/src/connection.rs`: `AbortOnDrop { handle: AbortHandle, closed:
  CloseGuard }`. The `closed` field is connection policy that stays where it is;
  only the abort half is shared.
- `rs/moq-tokio/src/tls.rs`: `Reload(JoinHandle<()>)`, whose doc comment explains
  what a leaked certificate watcher costs, not what the guard does. It keeps its
  name and its comment and becomes a thin newtype.
- `rs/moq-ffi/src/ffi.rs`: two more, one declared inside `detached` and one at
  module scope, both over `AbortHandle`.

Collapse the three `moq-tokio` sites onto one crate-private guard and the two
`moq-ffi` ones onto another. `moq-ffi` already depends on `moq-tokio`, so a
single shared guard is reachable, but that means a new `pub` item in a published
crate for a five-line `Drop` with two internal callers, which Public API Scrutiny
does not favor. Promote it if a third crate wants it.

Nothing about behavior changes, so this lands on the existing tests.
