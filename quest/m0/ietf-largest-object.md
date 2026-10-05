# [S] REQUEST_OK accepts LARGEST_OBJECT

## Goal

On moq-transport, a REQUEST_OK or REQUEST_UPDATE_OK carrying LARGEST_OBJECT
(0x09) is accepted in `rs/moq-net` and `js/net`, instead of closing the
session with PROTOCOL_VIOLATION. The draft makes the parameter a MUST when
objects exist, so a conformant peer hits this on its first reply. It lands
before Seattle interop on 2026-10-12.

## Plan

Decided in the 2026-10-05 audit: split from [moq-transport request
codes](/quest/m2/ietf-request-codes.md), whose other cases stay in m2,
because a session closed by legal input is exactly what m0's relay hardening
rules out.

- `RequestOk::decode_msg` (`rs/moq-net/src/ietf/request.rs`) allows only
  0x08 and ACTIVE_COUNT today. Decode 0x09 as a `Location`, which
  `ietf/parameters.rs` already handles, and decide whether anything reads it
  or it is ignored like EXPIRES.
- Mirror in `js/net/src/ietf`.
- A regression test per language that fails on `main`: a REQUEST_OK with
  LARGEST_OBJECT decodes, on each draft that carries it.

Public API: none. Wire: none new; a legal reply stops closing the session.

## Related

- [moq-transport request codes](/quest/m2/ietf-request-codes.md) - the rest of the validator's request-level findings
