# [M] js/net: expose typed announcement status instead of active boolean (AI review)

## Goal

Implement and verify the behavior tracked in [#2216](https://github.com/moq-dev/moq/issues/2216)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Problem

Rust models announcement updates as `Active`, `Restart`, and `Ended`, and the wire format carries all three states. The public JS API reduces this to `{ path, active: boolean }`, so `Restart` becomes indistinguishable from a normal active announcement.

That loses protocol information and makes it hard to add restart-specific behavior later without another API change.

#### Suggested direction

Expose a discriminated status such as `"active" | "restart" | "ended"` in `Announced.Event`. This is a breaking shape change, so the dev merge is the inexpensive time to make it. Keep any boolean helper as derived convenience rather than the canonical event shape.

## Closes

- [#2216](https://github.com/moq-dev/moq/issues/2216) - close this issue when the quest finishes
