# [S] moq-uring: use fixed-file slots for worker UDP sockets

## Goal

Implement and verify the behavior tracked in [#3202](https://github.com/moq-dev/moq/issues/3202)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Follow-up to #2875.

Every UDP SQE currently uses `types::Fd`, so the kernel resolves and takes references on the socket fd for each receive, send, and cancellation path. Worker sockets are long-lived and naturally fit a ring-owned fixed-file table.

#### Mechanism

Registered files let SQEs address a stable table slot with `types::Fixed`. This removes repeated fd-table lookup and reference work from the hot path.

#### Proposal

- Create a sparse fixed-file table owned by the worker ring.
- Allocate and update one slot when `Handle::udp` binds a socket.
- Use the fixed index for receive, send, and related socket SQEs.
- Keep slot ownership in one socket state object. Do not clear a slot until multishot receives, sends, and cancellations have reached terminal CQEs.
- Define teardown ordering explicitly so OS fd reuse cannot make a stale SQE target a different socket.
- Add stress tests for rapid bind/drop cycles, numeric fd reuse, cancellation races, and worker drop with operations in flight.

#### Acceptance

Benchmark steady-state send/receive traffic and high socket-churn workloads. Record CPU, cycles, instructions, throughput, and socket lifetime cost. Keep the implementation only if the hot-path win justifies the slot-lifecycle complexity.

## Closes

- [#3202](https://github.com/moq-dev/moq/issues/3202) - close this issue when the quest finishes
