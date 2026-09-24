# [M] Cluster stream

## Goal

moq-lite-07 defines a cluster stream that only relays open, one per direction
on a session between two relays. It carries two messages:

- RELAY: advertises, updates, or withdraws reachability to a relay, as
  `(hop, cost, chain)`;
- UPSTREAMS: replaces the sender's upstream table, which lists for each source
  relay its parent tie set and a backup per member.

A peer below lite-07, a customer, or an IETF session never opens a cluster
stream or sees one. Broadcast announcements and ANNOUNCE_REQUEST are
unchanged.

## Plan

- Specify the stream type and both messages in `drafts/draft-lcurley-moq-lite.md`
  alongside lite-07's other changes (the hidden opt-in and the prefix table).
- Only a relay that declared a hop in SETUP may open a cluster stream, and only
  on lite-07. Opening one is how a peer says it takes part in tree mode.
- A RELAY advert reuses the announce encodings for hop chains and `Cost`, so
  ranking and loop checks share code with routes.
- UPSTREAMS entries are `(source hop, [(member hop, backup hop or none)])`. An
  empty table means "flood me".
- Bound entry counts and sizes, and reject violations as protocol errors.
- Implement the Rust codec and session plumbing. JS never opens the stream and
  rejects one it receives.
- IETF gets no equivalent. IETF cluster links keep flooding, which is always
  correct, and they only occur with third-party peers.

Tests:

- round-trip both messages;
- a lite-06 peer and a customer session never see the stream;
- a session that drops takes its stream state with it.

## Related

- [Hidden broadcasts](/quest/m1/hidden-broadcasts.md) - lite-07's
  ANNOUNCE_REQUEST opt-in
- [Prefix table](/quest/m2/announce-prefix-table.md) - lite-07's announce
  stream encoding
