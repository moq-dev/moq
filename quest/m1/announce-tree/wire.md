# [M] Upstream interest on the wire

## Goal

In moq-lite-07, a subscriber can send its upstream table on the announce
stream: for each source relay, its parent tie set and a backup per member. It
can replace the table mid-stream. A publisher that understands the table
applies each update at its position in the stream. Every other implementation
decodes and ignores it, and a subscriber below lite-07 never sends one.

## Plan

- ANNOUNCE_REQUEST gains an optional upstream table. A new
  ANNOUNCE_REQUEST_UPDATE, sent by the subscriber on the same stream, replaces
  it. Entries are `(source hop, [(member hop, backup hop or none)])`.
- An empty table means "flood me", the same as no table.
- Specify both messages in the lite draft alongside lite-07's other changes,
  the hidden opt-in and the prefix table. Implement the Rust codec, and bound
  the entry count and size as protocol violations.
- Only relays send a table, and only on cluster sessions. JS decodes and
  skips it.
- IETF gets no equivalent. IETF cluster links keep flooding, which is always
  correct, and they only occur with third-party peers.

Tests:

- round-trip the codec across versions;
- a lite-06 peer never receives the messages;
- an update arriving between two announces changes what follows it and
  nothing before it.

## Related

- [Hidden broadcasts](/quest/m1/hidden-broadcasts.md) - the other lite-07
  addition to ANNOUNCE_REQUEST
- [Prefix table](/quest/m2/announce-prefix-table.md) - lite-07's announce
  stream encoding
