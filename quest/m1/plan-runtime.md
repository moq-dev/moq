# [S] Plan: the remaining thread-per-core runtime work

## Goal

A settled, per-PR split of [#2875](https://github.com/moq-dev/moq/issues/2875)
exists in the quest tree. Today the quest cannot land as one PR: the epic's M1/M2 milestones already landed on dev (worker groups, the moq-tokio rename) and moq-uring is in flight, so the plan text no longer matches reality.

## Plan

Run /plan-quest with the maintainer: refresh the milestone list to the remaining work (eBPF steering, SOCKARRAY, the outstanding uring perf quests) and cut it into per-PR quests. The resulting quests replace
[the parent quest](/quest/m1/2875-thread-per-core-relay-runtime-io-uring-quiche-ebpf.md) and carry its Closes entries forward.
