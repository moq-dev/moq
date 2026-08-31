# [M] Fix BBR

## Goal

Implement and verify the behavior tracked in [#686](https://github.com/moq-dev/moq/issues/686)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

BBR is a congestion controller that is used extensively for TCP traffic. It provides lower latency than the QUIC default of Cubic/Reno because it monitors RTT to avoid bufferbloat. It's also just better on crappy networks.

Quinn has BBR support but it's horribly broken from every indication I've seen. Quiche has better BBR because it is actually tested in production.

We could either:

1. Switch to Quiche
2. Port Quiche's BBR to Quinn

Option 1 is possible, but it would require rewriting `tokio_quiche` at this point.

## Closes

- [#686](https://github.com/moq-dev/moq/issues/686) - close this issue when the quest finishes
