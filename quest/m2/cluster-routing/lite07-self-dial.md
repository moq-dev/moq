# [S] A relay refuses a lite-07 session with itself

## Goal

A relay whose cluster peer URL reaches itself under a spelling `PeerKey`
misses, such as an IP for its hostname, refuses that session on lite-07 as it
already does on lite-05, lite-06, and moq-transport.

## Plan

Planned 2026-11-08 (maintainer, approved follow-up of
[moq-dev/moq#8225](https://github.com/moq-dev/moq/pull/8225)). Milestone:
m2, since the likely fix changes lite-07-wip's SETUP, which the cut
publishes.

`rs/moq-relay/src/connection.rs` refuses a session whose `peer_hop()` is
this relay's own Hop ID, but lite-07 SETUP carries no Hop ID
(`lite::Version::has_routes`), so the check never fires there. The route key
cannot stand in: relays may share one on purpose.

- Decide, with the maintainer: a per-process node identity in lite-07 SETUP
  (a wire change, lands while lite-07 is wip), or a local check that this
  relay's own route came back over the session (no wire change; would move
  this to m1).
- Test a relay dialing itself by IP on lite-07.

Public API: none expected. Wire: possibly a lite-07-wip SETUP field.
