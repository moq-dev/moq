# [XS] The ring drains moq-transport sessions with GOAWAY

## Goal

A test proves the io_uring workers send moq-transport 17+ sessions their
drain-window GOAWAY, the same as the tokio runtime.

## Plan

Today only `shutdown_signal.rs` covers the drain GOAWAY, through the shared
runtime. Extend the ring's drain test (`uring_workers_drain_on_the_trigger`
in `rs/moq-relay/tests/integration/runtime_uring.rs`, from #5821) to a
moq-transport 17+ client. Closes delivered on the ring is part of the
packaged-relay parity bar.

Public API: none. Wire: none.
