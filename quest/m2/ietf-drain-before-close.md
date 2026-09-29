# [S] IETF sessions drain before close

## Goal

`Session::close` on a moq-transport (IETF) session waits, like moq-lite, for
finished tracks to deliver their last groups and FIN before closing, within
the shared one second `CLOSE_TIMEOUT`.

## Plan

`Protocol::drained` in `rs/moq-net/src/driver.rs` returns `true` for IETF, so
its sessions close at once. Count the IETF publisher's in-flight subscribe and
fetch serves the way `lite::Publisher::drained` does, and report them there.
Extend `rs/moq-net/tests/session_close.rs` to cover an IETF version.

## Related

- [Session close](/quest/m1/session-close.md) - the graceful end that withdraws announces
