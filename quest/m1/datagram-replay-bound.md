# [XS] A new subscriber's datagrams start within its max delay

## Goal

A new datagram subscriber starts at the first buffered datagram within the
subscription's `max_delay` of the newest datagram's timestamp, in Rust
moq-net and `@moq/net`. Since #4982 it may otherwise get the whole
64-datagram send buffer (`MAX_DATAGRAMS` in `rs/moq-net/src/model/track.rs`
and `js/net/src/track.ts`), which on a sparse track can be minutes old.
Untimed tracks keep today's behavior.

## Plan

Decided by the maintainer (2026-10-08): "fine to serve old datagrams from
the last ~50ms", so the bound is the subscription's own `max_delay` measured
against the newest datagram's timestamp, not wall time. Only where a new
datagram cursor starts changes; delivery after that is unchanged.

Test with mocked time in both languages: a sparse timed track with old and
recent datagrams buffered gives a new subscriber only those within its
`max_delay` of the newest, and an untimed track still gives the whole buffer.

Public API: none. Wire: none.
