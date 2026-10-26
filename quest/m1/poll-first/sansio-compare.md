# [M] Sans-IO compared

## Goal

A report that converts one moq-net protocol path, such as a lite subscription's
control flow, to a sans-IO core (synchronous `handle_*` and `poll_transmit` /
`poll_timeout`-style calls with no `Waiter`, as quinn-proto and rustls do), driven
by a thin kio layer. It compares this with the kio `poll_*` version on:

- implementation effort;
- testability (deterministic replay without moq-clock);
- rust2's TypeScript translation and FFI export of the result;
- compile time;
- runtime cost.

It recommends whether moq-net's protocol core should move toward sans-IO.

## Plan

Decided with the maintainer on 2026-10-25: compare it in the design, and adopt it
only if it clearly wins. moq-uring already runs on a sans-IO QUIC stack, so
reuse its driving pattern. Zig's `std.Io` (I/O passed as an explicit parameter)
is the model the maintainer asked about. In Rust without stackful coroutines,
sans-IO is the closest equivalent that removes coloring from the protocol core.

Public API: none; the prototype stays on a branch unless adopted.

## Related

- [moq-quic owns its waits](/quest/m1/quic-api/waits.md) - a sans-IO core that holds kio waiter lists but does no I/O, a middle point between the two styles compared here
