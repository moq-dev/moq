# [M] Enforce E2EE receive limits and terminal failure

## Goal

Every attempted AEAD open consumes its invocation budget, and a grouped
authentication failure terminates the subscriber and wakes pending reads.
Bad datagrams remain typed events until a key limit is exhausted. Preserve
the public API and the ciphertext format.

## Plan

The release API audit found `TrackKey::open` increments usage only after
decryption succeeds (`rs/moq-e2ee/src/key.rs`), so repeated invalid tags do
not approach the invocation limit. `group::Consumer::fail_auth` only stores
an atomic flag; reads already parked in the underlying subscriber do not
receive a wake and the failed subscriber keeps its demand alive.

Reproduce both behaviors in the crate's CI tests. Charge actual AEAD attempts,
including failures, while keeping malformed-input prechecks and plaintext-byte
accounting consistent with the profile. A failed datagram must not mark its
sequence as successfully received and suppress a later authentic one.

Propagate grouped failure through shared terminal state and release the
underlying demand. Pending group, frame, and datagram reads must observe the
same terminal authentication error without another publisher event. Test
sibling handles and a publisher that remains open and silent after the bad
frame. Use test-private near-limit counters instead of millions of operations;
do not add exported test hooks.

## Related

- [Rust E2EE core](/quest/m0/e2ee-api.md) - API and profile alignment must preserve these receive invariants
