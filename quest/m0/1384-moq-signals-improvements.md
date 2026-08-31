# [XS] @moq/signals improvements

## Goal

Implement and verify the behavior tracked in [#1384](https://github.com/moq-dev/moq/issues/1384)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Warn on .set if we provide the same reference as before for non-primatives. Means we'll have to wrap references with a Signals.ref helper or something.

Export functions instead of classes to improve tree shaking? Maybe as a `/lite` path.

Use the TC39 or whatever Signal proposal.

## Closes

- [#1384](https://github.com/moq-dev/moq/issues/1384) - close this issue when the quest finishes
