# [S] Backport the Lite SETUP violation closes to release

## Goal

On `release`, a moq-lite peer that repeats a SETUP Parameter ID or opens a
second Setup Stream gets the session closed with PROTOCOL_VIOLATION (0x3), as
`drafts/draft-lcurley-moq-lite.md` requires, so a later 0.14.x CLI and 0.3.x
moq-net patch pass moq-dev/smoke's moq-lite conformance rows
L06-7-3-MUST-112 and L06-6-3-1-MUST-092.

## Plan

Reported in [#5521](https://github.com/moq-dev/moq/issues/5521) against `moq`
0.14.4. Verified 2026-10-15 that `main` already closes both cases:

- A second Setup Stream: `PeerSetup::claim` (`rs/moq-net/src/lite/setup.rs`)
  claims the slot as soon as the stream type is Setup, from 7ddfa101b in
  #4704. Tests: `duplicate_setup_closes_the_session`,
  `accept_setup_refuses_two_incomplete_setup_streams`,
  `accepted_lite_setup_refuses_a_second_setup_stream`.
- A SETUP that fails to decode (a duplicate parameter returns
  `DecodeError::Duplicate`): the `UniServe` Err branch in
  `rs/moq-net/src/lite/subscriber.rs` returns ProtocolViolation, from
  0f6a8d1f9 in #4704. No test sends a repeated Parameter ID.

On `release`, `UniServe` aborts a stream that fails to decode and returns
`Ok`, and `peer_setup.set` overwrites a second SETUP. The server's
`accept_setup` path already maps a decode error to ProtocolViolation there.

Decided 2026-10-15 (maintainer):

- One PR onto `release` (CONTRIBUTING) that ports only the two closes: the
  `PeerSetup` claim and the close on a SETUP decode error, with their tests.
  Not the rest of #4704 (early stream retention) or #5550.
- Add a repeated-Parameter-ID regression test on both `main` and `release`,
  one that fails without the close. Check whether js/net closes on a repeated
  parameter too; if it does not, report it as a follow-up rather than
  widening this backport.
- The crate patch bumps happen at the release cut, not in this PR.

Public API: none. Wire: none (`release` starts enforcing rules the draft
already states).

## Closes

- [#5521](https://github.com/moq-dev/moq/issues/5521) - moq-lite-06: a repeated SETUP parameter or a second Setup Stream does not close the session
