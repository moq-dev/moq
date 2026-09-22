# [S] Wake pending E2EE reads on terminal failure

## Goal

A grouped authentication failure terminates the subscriber and wakes pending
reads. Bad datagrams remain typed events until a key limit is exhausted.
Preserve the public API and the ciphertext format.

## Plan

`group::Consumer` only stores an atomic flag on a failed open
(`rs/moq-e2ee/src/group.rs`); reads already parked in the underlying
subscriber do not receive a wake and the failed subscriber keeps its demand
alive.

Reproduce the behavior in the crate's CI tests. Propagate grouped failure
through shared terminal state and release the underlying demand. Pending
group, frame, and datagram reads must observe the same terminal authentication
error without another publisher event. Test sibling handles and a publisher
that remains open and silent after the bad frame. Do not add exported test
hooks.
