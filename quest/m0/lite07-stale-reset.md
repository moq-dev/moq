# [L] A lite-07 publisher resets a stale group's stream when it expires

## Goal

A lite-07 publisher aborts every group stream whose group expired, including one whose end it already wrote, so stale video can't fill the connection's send window and stall the streams behind it. The scheduled TS workflow's validation passes again: no audio expires on its 7 Mb/s path.

## Plan

Caused by [moq-dev/moq#8908](https://github.com/moq-dev/moq/pull/8908) (`fb443a5ac7`), found by bisecting the TS validation failure (run [44135366690](https://github.com/moq-dev/moq/actions/runs/44135366690), still failing on 11-18). Under lite-07, a stale group stream whose GROUP_END was already written is no longer reset when the group expires (`poll_stopped` and `closes_expired` in `rs/moq-net/src/lite/publisher.rs`). Stale video groups then fill moq-quic's 8 MiB connection send window, the control stream sending SUBSCRIBE_SKIP stalls 1–10 s behind them, and audio expires: 60–100 s lost out of 120. Removing the `!has_group_end()` guard brings audio loss to zero.

Decided 2026-11-18 (maintainer, /quest-plan): ✅ reset stale groups again, as lite-06 did, in Rust, JS, the draft's Expiration section, and the `verify-subscribe` model (rejected: keep the wire rule and let higher-priority streams get past a full send window in moq-quic, since stale video would still use bottleneck bandwidth and other QUIC stacks would stall the same way; rejected: measure both first). This relaxes #8908's rule that each group is named only once; lite-07 is unpublished `-wip`, so the wire change is free.

Decided 2026-11-18 (maintainer): moq-net never finishes a stream or producer and forgets it; something must abort it eventually. The rule is in `rs/moq-net/AGENTS.md`. As part of this quest, audit moq-net (Rust and `js/net`) for other streams or producers that are finished and then never aborted, such as fetch, datagram, and announce streams, and fix each one found or plan it as a follow-up.

Public API: none. Wire: lite-07-wip expiry resets a finished group's stream again; update the draft in the same PR.
