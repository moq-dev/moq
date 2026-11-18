# [M] moq-quic stream reads wait instead of failing Blocked

## Goal

moq-quic's stream reads and writes report "nothing yet" as `Poll::Pending`,
not `Err(Blocked)`, and a peer's reset or stop code is read from its own
method rather than carried inside an error variant. moq-tokio's and
moq-uring's `ReadStatus` and their Pending-to-`Blocked`-and-back conversions
are gone, and no driver matches a moq-quic error variant to decide what to
do.

## Plan

Decided 2026-11-18 (maintainer, /quest-plan):

- ✅ Pending instead of `Blocked`: `recv::Error::Blocked` and
  `send::Error::Blocked` are deleted.
- ✅ A separate method gives the reset code instead of "smuggling it in the
  Error kind": `recv::Error::Reset` carries no code, and callers read it from
  `received_reset()`, which exists today. `send::Stream` gets a `stopped()`
  for the stop code, since every outside caller of `end()` collapses
  `End` to that code; narrow or drop `End` once nothing outside moq-quic
  needs it.
- ✅ Full permission to change the API shape; quinn compatibility is not a
  goal (rejected: wrapping at the drivers to stay close to quinn).

Reads already return `Ok(None)` when finished, so no separate finished poll
is needed. Today `Chunks::next` fails `Blocked` while `poll_next` returns
`Pending`, and the drivers convert one into the other
(`rs/moq-tokio/src/quic/driver/recv.rs`, `rs/moq-uring/src/quic/driver/recv.rs`);
pick one sync shape for the drivers, which poll without a waker in places.
Also reshape `CidQueue::insert`, whose `InsertError::Retired` is a normal
outcome sent through `Err`. Most `Error::Reset` matches in the drivers only
copy the code into their own error; those become a call to the code method.

Public API: breaking in moq-quic (`recv::Error`, `send::Error`, `End`).
moq-tokio and moq-uring change internally. Wire: none.
