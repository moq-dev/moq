# [M] Expose TransportConfig

## Goal

Implement and verify the behavior tracked in [#816](https://github.com/moq-dev/moq/issues/816)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

hello,

It would be nice to be able to adjust transport window sizes, idle timeout, keep alive etc and also congestion controller used by quinn without needing to patch moq-native

would that be possible?

thanks

## Closes

- [#816](https://github.com/moq-dev/moq/issues/816) - close this issue when the quest finishes
