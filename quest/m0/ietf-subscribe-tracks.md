# [S] SUBSCRIBE_TRACKS is refused per request

## Goal

A SUBSCRIBE_TRACKS (0x51) on drafts 18 and later gets REQUEST_ERROR with
NOT_SUPPORTED on its own stream, in Rust and JS, and the session stays open.

## Plan

Today Rust closes the session: drafts 17+ hit the `_` arm in
`rs/moq-net/src/ietf/session.rs` (`UnexpectedStream`, PROTOCOL_VIOLATION).
JS aborts the stream without a REQUEST_ERROR (`js/net/src/ietf/connection.ts`
default arm), and its `control.ts` check for drafts 18+ looks unreachable;
delete it if so. The drafts say limited endpoints SHOULD answer unsupported
messages with NOT_SUPPORTED (0x3 in every draft's registry). On drafts before
18, 0x51 is not a defined message and stays fatal. moq-dev/moq#4610 applies the same refuse-per-request rule to
other messages.

Decode enough of the message to reply on its stream, then refuse. Tests in
both languages: a draft-18+ SUBSCRIBE_TRACKS is refused NOT_SUPPORTED while
another subscription on the session keeps delivering.

Public API: none. Wire: none; fixes conformance.
