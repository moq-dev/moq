# [S] The 0.0.x crates follow the naming rules

## Goal

moq-sock, moq-uring, moq-archive, and moq-e2ee, all `0.0.x` with in-tree
consumers only, expose short role names under their modules and nothing
that is private in spirit. They do not gate the release.

## Plan

- moq-sock: `bind::Udp` becomes `bind::udp::Options` and
  `bind::udp_is_dual_stack` becomes `bind::udp::is_dual_stack`.
- moq-uring: `udp::TxBuf` becomes `udp::Staged`; the root `Config` re-export
  of `worker::Config` goes, since `udp`, `quic::endpoint`, `quic::client`,
  and `quic::server` each have their own; the `metrics::Snapshot` fields get
  doc lines and the crate turns on `missing_docs`.
- moq-archive: `store::List` / `Listed` become `store::list::{Query, Entry}`;
  the `path::{groups_prefix, segments_prefix, groups_offset}` duplicates of
  the `Store` methods become `pub(crate)`; `check_id` leaves the crate root;
  `info`, `path`, `segment`, and `store` get module docs.
- moq-e2ee: the crate-root re-exports of `datagram_payload_limit`, `MAX_U32`,
  `MAX_U53`, `varint_len`, and the raw-key `protect` / `open` / `nonce` go
  private; `TrackKey` becomes `track::Key`; the `DatagramEvent` alias goes;
  `catalog::protect_deflate` / `open_deflate` fold into `protect` / `open`
  keyed off the semantic name or a `Compression` argument.

Public API: breaking on four `0.0.x` crates; lands on main after the merge.
Wire: none.

## Required

- [Merge dev](/quest/m1/merge-dev.md) - the crates exist only on dev
