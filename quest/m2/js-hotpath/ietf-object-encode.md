# [S] Encode IETF object properties without a temporary stream

## Goal

Reduce object-property encode allocations while emitting identical bytes
for every supported IETF version and preserving the public Writer API.

## Plan

`encodeObjectExtensions` in `js/net/src/ietf/object.ts` creates a temporary
Writer and WritableStream, copies their emitted chunks, and concatenates
those chunks for timestamp-bearing objects. Replace that internal staging
with bounded byte encoding after measuring it.

Derive storage size from the actual fields and negotiated varint encoding;
do not assume a fixed scratch size covers every supported version. Preserve
absolute versus delta property IDs, timestamp conversion, absent timestamps,
and numeric bounds. A reused buffer must remain owned until the outer write
has completed; overlapping writes must not overwrite its bytes.

Add version-matrix byte-equivalence tests to normal JS CI, including varint
boundaries, maximum supported timestamps, missing properties, and write
failures. Register a repeatable JS microbenchmark using the repository's
benchmark conventions, with small audio and large video payloads and with
and without timestamps. Measure property allocations separately from payload
storage. Retain a measured no-win without replacing the implementation.
Browser claims require a real WebTransport run in an identified browser.

## Required

- [Browser benchmarks](/quest/m2/browser-benchmarks.md) - shared measurement and browser CI harness

## Related

- [Coalesce stream writes](/quest/m2/js-hotpath/write-coalesce.md) - outer transport writes
