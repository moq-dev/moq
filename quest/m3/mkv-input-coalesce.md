# [S] Byte-at-a-time MKV input imports as fast as chunked input

## Goal

The MKV importer fed one byte at a time is no slower than before
[moq-dev/moq#7234](https://github.com/moq-dev/moq/pull/7234) (about 81 µs
on its `mkv-chunks/1` bench, 155 µs after it), with the same memory bound:
readers lag the input by at most a small fixed buffer, so a block larger
than it stays readable before the block completes, and the importer retains
no more than that buffer, a split element header, and the fields it reads.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run), from [moq-dev/moq#7234](https://github.com/moq-dev/moq/pull/7234).
Milestone m3: real inputs arrive in 1500-byte or larger chunks, where [moq-dev/moq#7234](https://github.com/moq-dev/moq/pull/7234)
is as fast or about 2x faster.

[moq-dev/moq#7234](https://github.com/moq-dev/moq/pull/7234) streams each frame through `container::Producer::open`/`commit`,
so each input call's slice of a block payload becomes its own readable
write on the moq-net frame; fed byte by byte, that is one write per byte.
Already-larger chunks write their whole span at once, so only tiny calls
need help. Hold payload bytes in a small fixed buffer and write them when
it fills, when the payload ends, or on `finish()`/abort. Settle the bound
(a few KiB, well under one moq-net segment): it caps both the importer's
extra retention and how far readers lag the input. A payload shorter than
the buffer is therefore readable only once it ends; that delay is bounded by
the buffer and is the point of coalescing, so no flush per input call.

Measure with [moq-dev/moq#7234](https://github.com/moq-dev/moq/pull/7234)'s `mkv-payloads` and `mkv-chunks` benches in
`rs/moq-mux/benches`, and keep its retention tests passing.

Public API: none. Wire: none.
