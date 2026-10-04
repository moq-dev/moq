# [M] moq-uring: use SENDMSG_ZC for large UDP GSO trains

## Goal

GSO trains above a measured byte threshold go out as `SENDMSG_ZC`, the TX
lease outlives the notification CQE on every path, and the sweep shows it
beats `SendMsg` end to end before it is on by default.

## Plan

Follow-up to #2875. The closed, unmerged prototype
[#3224](https://github.com/moq-dev/moq/pull/3224) (on the quiche-era dev
tree) did this together with #3204's fixed buffers, opt-in behind
`udp::Config::send_zc_threshold`. On loopback it was 6 to 9% slower, but
`IORING_SEND_ZC_REPORT_USAGE` showed the kernel copying there, so loopback
measures only the forced-copy overhead: the sweep needs a remote peer
through a physical NIC.

The UDP path already assembles up to 64 KiB GSO trains in stable pool buffers, then submits `SendMsg` and recycles the buffer at the first CQE. Large trains are the promising case for `SENDMSG_ZC`; individual QUIC datagrams are likely below the copy-avoidance crossover.

#### Mechanism

`IORING_OP_SENDMSG_ZC` can avoid copying payload pages into kernel memory. A successful request normally produces a normal send completion and a later notification CQE. The source buffer must remain untouched until the notification arrives. `IORING_SEND_ZC_REPORT_USAGE` can report when the kernel copied instead.

#### Proposal

- Use `SendMsgZc` only above a measured GSO-train byte threshold. Keep `SendMsg` for small sends.
- Extend the send operation state to track both completion phases and retain the TX lease until the notification CQE.
- Enable usage reporting and expose counters for zero-copy success, copy fallback, and notification latency.
- Include the extra notification CQEs in CQ sizing and teardown accounting.
- Preserve the staged-send teardown guarantee from #3141.
- Add tests proving buffers cannot be reused after the send CQE but before the notification, including error, cancellation, and worker-drop paths.

#### Acceptance

Sweep the threshold across realistic chat and media packet trains. Record relay CPU, goodput, CQEs per send, copy-fallback rate, TX-pool pressure, p99 latency, and memory residency at fixed offered load. Enable it by default only where the end-to-end result beats regular `SendMsg`.

Decided in the 2026-09-30 audit: moved to m3. The #3224 prototype was 6 to
9% slower, the win needs a physical-NIC sweep nobody has run, and io_uring
ships in no package.

## Required

- [A physical-NIC peer for the zero-copy sweep](/quest/m3/zero-copy-peer.md) - loopback only measures the forced copy

## Closes

- [#3201](https://github.com/moq-dev/moq/issues/3201) - close this issue when the quest finishes
