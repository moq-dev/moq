# [S] A C origin can discover eagerly, like a relay

## Goal

The C API (`rs/moq-c`, `moq.h`) can put an origin in relay mode, with the
eager discovery moq-net gives an origin handle on a relay link, so a C
application that forwards a client session's routes needs no local
announce watcher of its own to make discovery run.

## Plan

Decided 2026-10-31 (maintainer, the /quest-complete requeue session), a
follow-up of [moq-dev/moq#7246](https://github.com/moq-dev/moq/pull/7246)
(moq-c API gaps), whose follow-ups note that C has no way to make an origin
relay-mode for a client-ingest relay.

In moq-net, a handle's link decides it: `origin::Producer::with_link`
(`rs/moq-net/src/model/origin.rs`) takes `Link::Local`, `Link::Peer`, or
`Link::Upstream`, and a non-local link's sessions discover everything they
may (`relays()`). `moq_origin_create` (`rs/moq-c/src/api.rs`) always makes a
local one. Expose the choice in C, by a create option or a setter mirroring
`with_link`, following whatever moq-ffi exposes if it has gained the same
by then, and document it in `moq.h` and `doc/lib/c`. If the
[generated C line](/quest/m2/c/README.md) has replaced the hand-written crate
before this starts, add it to moq-ffi instead so every binding gets it.
Test a C origin forwarding a client session's routes with no announce
watcher.

Public API: additive in moq-c, or in moq-ffi if the generated C line has
replaced it. Wire: none.

## Related

- [moq-c at parity](/quest/m1/c/README.md) - the hand-written crate's m1 parity work, which this does not block
