# [M] Coalesce frame headers without copying payloads

## Goal

Reduce measured WebTransport write overhead by combining frame header fields
while preserving payload ownership, public APIs, wire bytes, and backpressure.

## Plan

Lite and IETF encode a frame through multiple awaited writes. Count writes
for each negotiated version and frame shape rather than assuming one fixed
count. Writer accepts a single Uint8Array, and payloads have no guaranteed
prefix headroom, so a single contiguous header-plus-payload write generally
requires copying the payload.

Scope the optimization to internal header encoding: assemble header fields
into an owned bounded buffer, write that header, then write the existing
payload. Derive the bound from supported versions and fields. Retain scratch
until its write settles; preserve ordering, typed transport errors, reset,
and cancellation. Do not add an exported Writer method or change producer
buffer ownership. Payload concatenation and headroom redesign are outside
this quest.

Compare write calls, header allocations, copied bytes, CPU, and latency on
real WebTransport with small audio and large video payloads, including empty
payloads and slow readers. Expect fewer header writes, not a universal single
write per frame. Keep the current path if savings are not repeatable.

Add CI byte-equivalence tests across supported versions and delayed/rejected
write tests that catch scratch reuse, reordered bytes, and lost errors.
Keep already-buffered writes and control-message behavior unchanged.

## Required

- [Browser benchmarks](/quest/m2/browser-benchmarks.md) - shared measurement and browser CI harness

## Related

- [Reader buffering](/quest/m2/stream-buffering.md) - receive assembly
- [IETF object properties](/quest/m2/js-hotpath/ietf-object-encode.md) - property construction
