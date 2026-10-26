# [S] One typed SETUP parse

## Goal

One function turns SETUP parameters into path, token, implementation, and
`Peer` for every caller, replacing the three copies in
`rs/moq-net/src/server.rs` (legacy CLIENT_SETUP) and
`rs/moq-net/src/ietf/session.rs` (`accept_setup`, `decode_peer_setup`).

## Plan

- The legacy copy builds `Peer` by hand without cluster, auth, or epoch;
  check whether that difference is intended before unifying.
