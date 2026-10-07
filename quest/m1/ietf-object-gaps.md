# [S] A gapped IETF object ID is refused like a subgroup

## Goal

A moq-transport group whose object IDs skip (legal per draft-22 section
11.3.1, but not representable in the model) is refused the same way a
non-zero subgroup is: the group aborts, a warning is logged, and the stream
is stopped with the error, in Rust and JS. An object ID that overflows
closes the session with PROTOCOL_VIOLATION in Rust, as the draft requires.

## Plan

Decided 2026-10-06, while declining object-level identity in #4926: the
model is the product, so subgroups, gapped object IDs, and arbitrary object
properties stay unsupported. Refusals must be loud and consistent.

Facts from `main`:

- Rust: `next_object_id` in `rs/moq-net/src/ietf/subscriber.rs` warns and
  returns `Unsupported`, but `recv_group` catches it, logs at debug, aborts
  the producer, and returns `Ok(())`, so no STOP_SENDING carries the reason.
  A non-zero subgroup returns `Err`, which the session's `stop_on_error`
  turns into STOP_SENDING. An overflowing object ID (`BoundsExceeded`) is
  aborted with the group the same way.
- A data stream's error never reaches the session: `stop_on_error` in
  `rs/moq-net/src/ietf/session.rs` only aborts the stream, even for a
  protocol violation. Closing the session on overflow needs a new route from
  the subgroup task to session teardown, like `run_subscribe_namespace`'s.
- JS: `Frame.decode` in `js/net/src/ietf/object.ts` throws on a gap, and
  `handleGroup` in `subscriber.ts` stops the stream without logging.

Neither closes the session for a gap; that would punish a peer following
the spec. JS refuses any non-zero delta before adding it, so it has no
overflow to close on; the overflow case is Rust-only.

Tests: a gapped group yields `Err(Unsupported)` from `recv_group` and
stops its stream, while the subscription keeps flowing (beside
`a_non_zero_subgroup_leaves_the_track_flowing`); an overflowing object ID
closes the session (Rust).

Public API: none. Wire: none.
