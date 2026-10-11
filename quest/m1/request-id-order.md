# [S] A reused moq-transport Request ID is refused

## Goal

On drafts 14 to 16, `@moq/net` and `moq-net` refuse invalid incoming Request
IDs, including IDs whose requests already retired, without rejecting valid
independent-stream reordering. In-order requests and MAX_REQUEST_ID grants
continue to work.

## Plan

[PR #5235](https://github.com/moq-dev/moq/pull/5235) implements exact-next
admission in both languages. Regressions cover repeated, decreasing,
skipped, wrong-parity and retired IDs, including updates and both peer
roles. Request-limit errors remain distinct from invalid-ID errors.

Section 9.1 of drafts 14 to 16 starts at the peer's parity and increments
by two for each request. Draft 16 section 9.25 also puts SUBSCRIBE_NAMESPACE
on an independent bidi stream with that same Request ID space. Its arrival
can reorder against the control stream; one arrival-order counter does not
establish sender order across those streams.

Settle the draft-16 compatibility policy before completing this quest:

- Literal exact-next admission across all streams follows section 9.1 but
  can reject transport reordering.
- Exact order for drafts 14/15, with bounded spent-ID tracking for draft 16,
  preserves reordering. Recommended direction; define the lifetime/window
  bound before implementing it. Retired IDs must never become reusable.
- Buffer draft-16 gaps until the missing requests arrive. This preserves
  order but can delay refusing a genuinely skipped ID.

Test control/native arrival reordering deterministically, as well as
retired-ID reuse and long-lived namespace requests while control requests
retire. Do not add retries or timing allowances. Run `just check` and
`just test interop --all`.

Public API: matching named InvalidRequestId session errors in Rust and JS.
Wire: unchanged framing, stricter refusal with existing error 0x4; report
any draft-16 acceptance policy explicitly.
