# [S] moq-quic drops inherited surfaces with no consumer

## Goal

moq-quic's public API loses the inherited surfaces nothing calls:
`endpoint::{Accepting, Accepted}`, `Endpoint::{start_accept, finish_accept}`,
`Connection::{side, is_handshaking}`, and `stream::Id::{new, dir, initiator}`.
Each is removed, not just made private, unless a caller inside moq-quic needs
it, in which case it becomes private.

The config knobs nothing outside moq-quic sets become private at their
current defaults: `packet_threshold`, `time_threshold`,
`persistent_congestion_threshold`, `allow_spin`, `crypto_buffer_size`,
`pad_to_mtu`, `send_fairness`, `min_mtu`, `min_reset_interval`,
`retry_token_lifetime`, `incoming_buffer_size_total`, and `initial_rtt`.
moq-tokio, moq-uring, and qmux each trim their lists of moq-quic names
(20 in moq-tokio's `quic::driver`) to the items their own public signatures
use, and add no `pub use moq_quic;`, since
[reexports](/quest/m2/api-shape/reexports.md) drops every whole-crate
re-export. qmux keeps `Version`, its own wire concept. The PR's Impact section says how to
migrate; the release cut copies that into `MIGRATE.md`.

## Plan

Decided 2026-11-07 (maintainer, /quest-plan from
[moq-dev/moq#8088](https://github.com/moq-dev/moq/pull/8088)): remove them
before the cut, since removing public API after it would be another break.
Not chosen: folding the list into the post-cut
[dead-code audit](/quest/m3/audit-dead-code.md).

Added 2026-11-07 (maintainer, /quest-plan from the API shape audit): the
config knobs and the re-export fan-out, which grows the quest from [XS].

Changed 2026-11-07 (maintainer,
[moq-dev/moq#8233](https://github.com/moq-dev/moq/pull/8233)): the lists
name what each crate needs instead of becoming `pub use moq_quic;`, since
moq re-exports no whole crate.

Confirm each has no caller across the workspace, bindings, and vendored
crates before removing it.
