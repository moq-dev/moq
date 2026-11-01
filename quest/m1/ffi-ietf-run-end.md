# [S] Bindings see a moq-transport run end as an end

## Goal

When a moq-transport publisher ends a run, a moq-ffi consumer (Python, Swift,
Kotlin, Go) sees the same kind of end it sees over moq-lite, not
`MoqError.Internal`. A binding can then follow a replaced publisher over
either protocol by error kind, without treating an IETF run's end as fatal.

## Plan

Decided 2026-10-31 (maintainer, /quest-plan of the follow-ups from
[moq-dev/moq.pro#3169](https://github.com/moq-dev/moq.pro/pull/3169)). That
PR's Go and Kotlin samples follow a lite run's end, which reaches the FFI as
a protocol reset (`MoqException.Protocol`), but over moq-transport the old
run's catalog ends with a stream reset that decodes as
`moq_net::Error::Remote(0)`. `rs/moq-net/src/error.rs` maps every `Remote(_)`
to `StreamError::Internal`, so the FFI raises `Internal` and the samples stop.

- First establish what code 0 means on the moq-transport drafts we keep
  ([the IETF drop](/quest/m1/ietf-drop/README.md)): a clean end of the
  publisher's run, or a real internal error. Only a code that means the run
  ended maps to an end; others stay errors.
- Map it in one place (moq-net's `Remote` conversion or the FFI's
  `From<moq_net::Error>`), keep the JS consumer consistent, and cover it with a
  moq-transport publisher that ends a run under an FFI consumer.
- The broadcast-closed signal itself already shipped on main as
  `MoqBroadcastConsumer.closed()`
  ([moq-dev/moq#7304](https://github.com/moq-dev/moq/pull/7304)); it reaches
  the bindings with the next release, which moq.pro's reader quest adopts.
