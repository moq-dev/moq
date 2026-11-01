# [S] Copy small received frames out of shared receive buffers

## Goal

A small frame a session receives whole is not cached as a slice of the
transport's receive allocation, so the live heap behind cached received frames
stays close to what `Pool::used()` bills. Large frames crossing the in-memory
transport still arrive without a copy.

## Plan

[`backing`](/rs/moq-net/benches/backing.rs) measured this in
[moq-dev/moq#7522](https://github.com/moq-dev/moq/pull/7522). Run it with
`BACKING_ALLOCS=1 cargo bench -p moq-net --features fuzz --bench backing`.
[`Reader::poll_read_frame`](/rs/moq-net/src/coding/reader.rs) caches a frame
that arrives in one transport chunk as that chunk. Under QUIC, that is a slice
of a datagram. `moq-tokio` copies datagrams into shared 64 KiB receive
allocations, and a `moq-uring` GRO completion is a single allocation as well.
With 64 KiB receive allocations, 4 Mbps video and 64 kbps audio retained 1.9x
the pool's bill with nothing evicted. With the video groups evicted and the
audio kept, it retained 33x. Copying such frames brought every case back to
the control (1.01-1.19x). The copy cost 880-1,120 user instructions per frame
copied, adding 7-16% to ingest.

Pick the copy rule in moq-dev/moq#7522, then land it with the bench as its
evidence. The size threshold decides how far the copy reaches into
in-memory transport frames, which currently cross as the publisher's
allocation.

The fMP4 importer is not affected: every sample is copied into its own frame,
so its retained heap beyond the bill is a constant ~0.3 MB of importer state.
Its passthrough fragments do keep up to 2x capacity slack, but
[moq-dev/moq#7232](https://github.com/moq-dev/moq/pull/7232) removes it.
