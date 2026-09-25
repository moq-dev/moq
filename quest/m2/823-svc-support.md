# [M] SVC support?

## Goal

Implement and verify the behavior tracked in [#823](https://github.com/moq-dev/moq/issues/823)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

WebCodecs supports SVC, but somebody should actually test it out.

If it works, we could add a field to the catalog indicating `layer`. The media download logic will get more complicated but it should be possible to support.

## Closes

- [#823](https://github.com/moq-dev/moq/issues/823) - close this issue when the quest finishes
