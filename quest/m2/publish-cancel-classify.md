# [XS] The lite publisher tells transport faults from cancels

## Goal

`rs/moq-net`'s lite publisher no longer counts every `Error::Transport` as a
clean cancel: a peer walking away logs at debug, a real transport fault at
warn. The probe stream and publisher share one classifier, and a test covers
a clean probe cancel at the `Control` boundary.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7350](https://github.com/moq-dev/moq/pull/7350): m2, as
recommended.

`is_cancel` (`rs/moq-net/src/lite/publisher.rs:1518`, "TODO better
classify WebTransport errors") matches all `Transport(_)` and `IdleTimeout`.
[moq-dev/moq#7350](https://github.com/moq-dev/moq/pull/7350) left it alone and wrote its own three-variant match for probe ends in
`lite/publisher.rs` and `lite/subscriber.rs`.

- Classify `Transport` errors by what the transport says (a stream reset or
  stop with the cancel code, or a closed session, is a cancel; anything else
  is a fault), then use one helper at every log site, including [moq-dev/moq#7350](https://github.com/moq-dev/moq/pull/7350)'s.
- Test: a subscriber cancelling a probe at the `Control` boundary logs no
  warning (or the classifier returns cancel for it), and an injected
  transport error is not a cancel.

Public API: none. Wire: none.
