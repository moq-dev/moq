# [S] The interop matrix's cpp to js cell stays inside its budget

## Goal

The `cpp -> js` cell of `just test interop --all` finishes inside its 30 s
budget on CI, fixed at its cause rather than by raising the budget.

## Plan

It took 38 s once in a CI Interop dispatch while working
[moq-dev/moq#6302](https://github.com/moq-dev/moq/pull/6302) (run 39734065658).
Find which phase overran (C++ publisher start, the browser's first frame, or
teardown) from that run's log first, then reproduce under load.

Public API: none expected. Wire: none.
