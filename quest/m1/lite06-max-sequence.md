# [S] Resolve lite-06's max-sequence browser gap

## Goal

A group whose sequence is 2^62-1, the largest QUIC varint, either reaches a moq-lite-06
subscriber through the relay or is refused loudly where it cannot be carried. In the
browser fixture the publisher accepts it and the subscriber waits forever, while
moq-lite-02 and moq-transport-19 deliver it and 2^53+1 crosses all three. That fixture
also uses the sequence as the frame timestamp; isolate the two before changing behavior.

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

Native reproduction on `0955b19c1` separates the two values the browser fixture couples:

- The whole sequence ladder, including 2^62-1, arrives over moq-net memory transport
  both directly and through a forwarding origin when timestamps are representable.
- The browser also writes `timestampUs = sequence`. Published lite-06
  [FRAME section 7.20](https://datatracker.ietf.org/doc/html/draft-lcurley-moq-lite-06#section-7.20)
  zigzags the first timestamp from zero. Its largest positive first-frame timestamp is
  2^61-1 in the track's timescale; 2^62-1 encodes as 2^63-2 and cannot fit a QUIC varint.
- The actual moq-relay `Connection` over memory transport reproduces the browser hang
  with `Timescale::MICRO` and `Timestamp::from_micros(sequence)`. On paused Tokio time,
  `connection::largest_quic_sequence_reaches_the_subscriber` fails at the final lite-06
  group with `moq-lite-06 sequence=4611686018427387903 hung`. To reproduce from the
  retained test, replace its timestamp's `sequence.min((1 << 61) - 1)` with `sequence`.
  The command was
  `nix develop --max-jobs 2 --command env CARGO_BUILD_JOBS=2 NEXTEST_TEST_THREADS=4 just rs test -p moq-relay --test integration -E 'test(largest_quic_sequence)'`.

Keep the sequence tests independent of unsupported timestamps: use the same ladder
with each timestamp capped at 2^61-1 and check exact timestamp delivery where carried.
The browser known-gap branch remains until browser verification and an explicit
decision about surfacing an unencodable timestamp. The wire encoder already refuses
it, but resetting its group stream leaves the live subscription waiting when that
group has not become visible. Decide the refusal boundary and recovery contract
before changing it; do not reinterpret the published lite-05/06 framing or silently
alter the independent SUBSCRIBE_DROP work.
