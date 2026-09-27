# [S] JS VarInt

## Goal

js/net has a `VarInt` type that holds the full 62-bit range, converts to and
from `number` with a loud error outside the safe range, and encodes and
decodes without BigInt on the hot path. It is the TypeScript type rs2ts maps
Rust's `VarInt` to.

## Plan

Measured in node 24 for an 8-byte encode plus decode: `number` written as two
`u32` halves 2.4 ns, a `{hi, lo}` pair 4.8 ns, `bigint` 10 ns, and js/net
today (BigInt on the wire, then `Number()`) 32 ns. The leading-ones encoder
also converts every value to BigInt, even small ones.

Guidance:

- Store two `u32` halves; offer `fromNumber` and `toNumber` (throwing above
  2^53 or on a negative or fractional input), `fromBigInt` and `toBigInt`,
  and comparison and increment methods so sequence logic never converts.
- Move js/net's varint reading and writing onto it, dropping the BigInt
  round trip for QUIC and leading-ones varints.
- Unit-test the boundaries (2^30, 2^53, 2^62 - 1) against Rust's encoder in
  `just test interop`.

Public API: additive to `@moq/net`; lands on `main`. Wire: none.
