# [S] lite-06 drops group sequence 2^62-1 silently

## Goal

A group whose sequence is 2^62-1, the largest QUIC varint, either reaches a moq-lite-06
subscriber through the relay or is refused loudly where it cannot be carried. Today the
publisher accepts it and the subscriber waits forever, while moq-lite-02 and
moq-transport-19 deliver it and 2^53+1 crosses all three.

## Plan

Found by the browser binding spike (#5838): `test/wasm/uniffi/cases.ts` publishes the
ladder 5, 2^32, 2^53-1, 2^53+1, 2^62-1 from one moq-ffi session to another through each
relay flavour. On lite-06 it records 2^62-1 as a known gap, and the case fails once the
gap closes.

Reproduce natively first, with a moq-net test over the memory transport and then through
moq-relay, to find whether the publisher, the relay, or the subscriber drops it. Fail
loud if the value is illegal on lite-06; otherwise deliver it. Land a regression test
with the fix, then remove the browser case's known-gap branch.

Public API: none expected. Wire: none if the value is legal; otherwise the refusal
matches the published draft.
