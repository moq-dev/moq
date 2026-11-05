# [M] JS datagram tracks

## Goal

`@moq/net` has the same separate datagram types as Rust, and speaks the
lite-07 datagram request and datagram body against the Rust implementation.
Stream tracks lose `appendDatagram`, and datagram sequences no longer share
the group counter. Older lite versions keep today's group datagrams.

## Plan

Follow the [epic](/quest/m1/datagram-tracks/README.md)'s decisions and the
lite-07 wire that `drafts/draft-lcurley-moq-lite.md` specifies (Datagrams
Stream 0x9, DATAGRAMS, DATAGRAMS_UPDATE, DATAGRAMS_INFO, and the Track
Datagram body), which Rust speaks since
[moq-dev/moq#7752](https://github.com/moq-dev/moq/pull/7752).

- Remove from `js/net/src/track.ts` and the lite subscriber and publisher:
  - `appendDatagram` and the shared sequence counter (`track.test.ts`
    "appendDatagram shares the group sequence counter");
  - the join and refresh rules for datagrams ("a join wants past a datagram
    after the newest group");
  - the datagram cases in ranges.
- Map lite-01..06 group datagrams the same way Rust does.
- Update any first-party consumer of JS datagrams.
- Drop the sentence saying the JavaScript library still sends datagrams
  within a stream track from the Datagrams section of `doc/concept/moq-lite.md`.

Public API: breaking for `@moq/net` datagrams. Wire: lite-07-wip, matching
Rust.
