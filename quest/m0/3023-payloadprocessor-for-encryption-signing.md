# [M] PayloadProcessor for encryption/signing

## Goal

Implement and verify the behavior tracked in [#3023](https://github.com/moq-dev/moq/issues/3023)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

I've created a small encryption/signing lib for moq payloads, [moq-secure](https://github.com/cathode-ray-tube/moq-secure) but struggling to find a slot to implement it.

In moq-net, there are multiple write\_frame/read\_frame functions across group.rs and track.rs.

A PayloadProcessor (perhaps behind a feature flag) would be useful, alternatively any suggestions where to apply the encryption/signing?

## Closes

- [#3023](https://github.com/moq-dev/moq/issues/3023) - close this issue when the quest finishes
