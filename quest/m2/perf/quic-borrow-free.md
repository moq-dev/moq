# [M] QUIC connection state without runtime borrow checks

## Goal

A design report, with prototypes, on how a QUIC connection and its stream
handles can share state without a runtime borrow check (`RefCell`, `Mutex`)
on the data path, and a recommendation to adopt one or keep the lock. Each
prototype covers one stream's write and read path in moq-quic and moq-tokio's
driver, enough to run the fanout bench.

## Plan

Decided with the maintainer on 2026-10-25: `Rc<RefCell<T>>` and
`Arc<Mutex<T>>` are API warts, a sign the borrow checker could not do the job
so it happens at runtime. Today moq-quic's `Connection` is mutated by the
driver (packets, timers, transmit) and by stream handles in other tasks, so
moq-tokio locks it and moq-uring borrows it from a `RefCell` per core.

Compare:

- **Context passing.** Stream handles are owned, non-`Clone` tokens, and every
  operation takes `&mut Connection` (`conn.write(&mut stream, buf)`). One owner
  task holds the connection and passes it into its children's polls
  (`kio::Tasks`), like Zig's explicit `std.Io`. Cost to measure: moq-net's
  per-subscription tasks become children of the session task, and
  moq-net's `transport::poll` `&self` shape flips to `&mut`
  plus context.
- **Driver-owned state, handles as channel ends.** The driver alone owns
  moq-quic. A send stream is the writer of a per-stream lock-free log (the
  handle appends; the driver packetizes and retransmits by offset; acks
  evict), credit is an atomic the driver publishes, received data arrives
  through a per-stream queue, and the waits from
  [waits](/quest/m1/quic-api/waits.md) live in that per-stream cell. Keeps
  the `&self` API and fits both runtimes; msquic's one owner per connection is
  prior art. Costs: an `Arc` per stream and credit mirrored on both sides.
  `kio::log` evicts only from its writer, so acked eviction needs a path
  back from the driver.
- **Today's lock** as the baseline.

Rejected: branded cells (`qcell`, GhostCell). Access still needs a `&mut`
owner token, so they reduce to context passing with shared pointers, and a
lifetime brand fights `'static` tasks.

Measure each on [QUIC lock contention](/quest/m2/perf/quic-lock-contention.md)'s
sweep (connections × streams per connection × workers), on tokio and
moq-uring, and report the API each implies for moq-quic, the drivers, and
moq-net's `transport::poll` traits. Only context passing would revisit their
`&self` shape, and the report says so if it wins.

Public API: none; prototypes stay on a branch. The adoption is its own quest.

### Mechanism prototypes (2026-11-01)

[moq-dev/moq#7531](https://github.com/moq-dev/moq/pull/7531) has the full
report. The prototypes are on the `proto/quic-borrow-free` branch
([c368f72](https://github.com/moq-dev/moq/commit/c368f72c0b)), not to land:
`rs/moq-quic/benches/share.rs` runs a relay that writes new uni streams to N
subscribers over real moq-quic connections (plaintext, virtual time). Every
shape makes the same moq-quic calls through the [waits](/quest/m1/quic-api/waits.md)
`poll_*` forms. The four shapes are context passing, `RefCell`, `Mutex`, and a
driver-owned connection. In the driver-owned shape, each send stream is a
`kio::log` the handle appends to against published credit, and each received
lease goes through a cell to the reader and comes back for release.

Instructions per stream (`instructions:u`, pinned core, runs differ by under
0.6%):

- **`RefCell`** costs 0.2–1.0% more than context passing, and **`Mutex`**
  costs 1.1–3.5% more (about 290 instructions per stream, plus about 100 per
  extra write).
- **Channel** costs 5–29% more: about 4.2–4.8k instructions per stream plus
  about 320 per extra write. The cost is highest for one write per stream,
  which is fanout's shape. About 1.3k of the extra comes from `kio::log`'s
  epoch reclamation, about 1.1k from allocations (the cells, log segments,
  tasks, and lease boxes), and the rest from credit, the cell stacks, and the
  extra wake hop.
- **Threaded** (noisy host): with the relay's writes spread over 1–7 threads,
  `Mutex` had the lower median wall time in 13 of 16 cases. Channel was ahead
  only when two or four writers shared a single connection.

What remains: the driver-level prototypes in moq-tokio and moq-uring, the
fanout-bench sweep, and the recommendation. Whether they are still worth
building is open on the PR.

## Related

- [QUIC lock contention](/quest/m2/perf/quic-lock-contention.md) - measures the lock this would remove; its staging queue is a partial form of the channel approach
- [moq-quic owns its waits](/quest/m1/quic-api/waits.md) - the wait shape every prototype keeps
