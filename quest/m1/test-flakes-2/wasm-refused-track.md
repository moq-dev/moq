# [XS] The WASM refused-track case fails fast

## Goal

`test/wasm`'s "refuses a track the publisher does not serve" case passes
reliably and, when it does fail, fails on the refusal it expects rather than
the 30 s case timeout.

## Plan

Timed out once in 40 runs, including on docs-only #5801 (run 39040909046).
On IETF and lite-02 the refusal only shows as no group arriving, so a hang
looks the same as a pass that never resolves. Reproduce it, find which side
stalls, and give the case an observable refusal where the protocol has one.

Public API: none. Wire: none.
