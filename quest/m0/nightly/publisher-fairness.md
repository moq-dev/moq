# [M] The browser publisher completes its fairness sweep

## Goal

The publisher fairness benchmark completes its backlog on the hosted ARM runner, yielding browser tasks while serving every group.

## Plan

[Audio quality's fairness step](https://github.com/moq-dev/moq/actions/runs/40698329026/job/122851001011) fails in the
IETF 128-group, eight-viewer case: after ten seconds it reports
`only 0/1024 streams, 4832088 bytes`. Earlier sweep cells complete.
`just js serve-bench` passes on the local x86 host, including the failing
cell in 6.14 s with 5,109,136 bytes.

Reproduce the ARM failure and distinguish excess work from unfinished stream
lifecycle. Fix the source rather than raising the deadline or deleting the
timer-starvation assertion. Keep the browser sweep as the performance proof;
Bun or Node timing is not acceptance evidence.

Public API: none expected. Wire: none expected.
