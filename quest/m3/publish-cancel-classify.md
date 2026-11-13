# [XS] The lite publisher tells transport faults from cancels

## Goal

`rs/moq-net`'s lite publisher no longer counts every `Error::Transport` as a
clean cancel: a peer walking away logs at debug, a real transport fault at
warn. The probe stream and publisher share one classifier, and a test covers
a clean probe cancel at the `Control` boundary.

## Plan

Planned 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7350](https://github.com/moq-dev/moq/pull/7350).

`is_cancel` (`rs/moq-net/src/lite/publisher.rs:1662`, "TODO better classify
WebTransport errors") counts every `Error::Transport(_)` and `IdleTimeout` as
a cancel. The classification it asks for already happens at the transport
boundary: `Error::from_transport` (`rs/moq-net/src/error.rs`) reads a stream
reset's or session close's code, and otherwise the transport's `CloseKind`,
so a cancel code becomes `StreamError::Cancel` or `SessionError::Cancel`, a
local close becomes `SessionError::Cancel`, and only an unclassified failure
is left as `Error::Transport`. [moq-dev/moq#7350](https://github.com/moq-dev/moq/pull/7350)'s probe ends in `lite/publisher.rs` and
`lite/subscriber.rs` already match only the three cancel variants.

- Drop `Error::Transport(_)` from `is_cancel`, decide whether `IdleTimeout`
  stays (a peer that went silent, not a fault of ours), and use that one
  helper at both probe-end sites too, rather than separate matches.
- Test: a subscriber cancelling a probe at the `Control` boundary logs no
  warning (or the classifier returns cancel for it), and an injected
  transport error is not a cancel.

Public API: none. Wire: none.
