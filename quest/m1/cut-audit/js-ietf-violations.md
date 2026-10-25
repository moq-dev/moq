# [S] JS moq-transport sessions close on peer violations and bound reflected namespaces

## Goal

A JS moq-transport session closes with the violation's code when the peer
breaks the protocol, as Rust does, never leaves an unhandled rejection
behind a reset SUBSCRIBE_TRACKS, and bounds the namespace entries a peer can
make it hold with the same announce slots Rust uses.

## Plan

Found by the 2026-10-24 pre-cut audit at 151a529670. Skip anything that only
affects drafts 14-17 or 19-21; [moq-transport 18 and 22 only](/quest/m1/ietf-drop/README.md)
deletes them (including the control-stream adapter's request-ID stall the
audit also found).

- **Reflected NAMESPACE entries take no slot** (source-inferred, new on main
  from [moq-dev/moq#5932](https://github.com/moq-dev/moq/pull/5932)):
  `js/net/src/ietf/subscriber.ts:641` stores a reflected entry in `live`
  without an `#announced` slot. A peer that learned our Hop ID from SETUP
  can grow `live` without bound. Take the slot for every live
  advertisement before deciding, as Rust does
  (`rs/moq-net/src/ietf/subscriber.rs:1449`, test
  `a_reflected_namespace_holds_an_announce_slot`), and release it on
  NAMESPACE_DONE or stream end.
- **Unhandled rejection** (reproduced under Bun, new on main):
  `ietf/publisher.ts:2347` derives `this.#quic.closed.then(() => "closed")`
  with no rejection handler; a SUBSCRIBE_TRACKS reset early leaves it
  unobserved when the transport later dies. In Node or Bun hosts that kills
  the process by default. Handle both outcomes, as line 1676 does.
  SUBSCRIBE_TRACKS is under a week old; read the rest of its JS path while
  there.
- **Violations close with the wrong code** (source-inferred, inherited), all
  through the existing `#fail`/`#violated` path:
  - `ietf/subscriber.ts:1281` closes a duplicate track alias with code 0;
    Rust uses the Duplicate code (`subscriber.rs:3423`).
  - `ietf/connection.ts:501` closes cleanly on an unsolicited
    PUBLISH_NAMESPACE; Rust returns ProtocolViolation.
  - `ietf/connection.ts:445` aborts only the stream when REQUEST_UPDATE is a
    bidi stream's first message; Rust closes the session with
    `UnexpectedStream`.

Public API: none. Wire: only the close code the peer sees.
