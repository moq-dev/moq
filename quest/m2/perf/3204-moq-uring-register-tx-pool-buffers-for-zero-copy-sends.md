# [M] moq-uring: register TX-pool buffers for zero-copy sends

## Goal

TX-pool buffers are registered as fixed buffers once per worker, eligible
zero-copy sends use the fixed index, and the same #3201 matrix shows a
measurable gain over plain `SendMsgZc` or the change is dropped.

## Plan

Follow-up to #2875 and dependent on the `SENDMSG_ZC` experiment in #3201.

The TX pool already owns stable `Box<[u8]>` allocations and grows lazily. If zero-copy send wins, registering those allocations lets send SQEs reference fixed buffers and can reduce repeated page accounting on the large-train path.

#### Proposal

- Register a sparse buffer table once per worker.
- Give each lazily allocated TX buffer a stable ring-wide buffer index and populate it with `register_buffers_update`.
- Submit eligible zero-copy sends with the fixed-buffer flag and index.
- Keep allocation, registration, and TX lease ownership in one type so a buffer cannot move, unregister, or free while the kernel can reference it.
- Preserve lazy pool growth. Account for locked-memory limits and expose registration failures rather than silently growing unbounded pinned memory.
- Define worker teardown ordering and add tests for growth, partial registration failure, reuse after notification, and drop with sends in flight.

#### Acceptance

Compare #3201 with and without registered buffers using the same threshold and workload matrix. Record CPU, cycles, registration cost, locked memory, pool starvation, throughput, and latency. Do not add the complexity unless it improves the winning zero-copy range beyond ordinary `SendMsgZc`.

## Required

- [#3201: moq-uring: use SENDMSG_ZC for large UDP GSO trains](/quest/m2/perf/3201-moq-uring-use-sendmsg-zc-for-large-udp-gso-trains.md) - complete the prerequisite issue first

## Closes

- [#3204](https://github.com/moq-dev/moq/issues/3204) - close this issue when the quest finishes
