# [XS] Draft 17-18 withdraw by reset

## Goal

On drafts 17 and 18, moq-net withdraws a PUBLISH_NAMESPACE by resetting the
request stream and sending STOP_SENDING, as drafts 19 and later already do,
so quiche-moq no longer closes the session on withdrawal.

## Plan

Reported from interop on 2026-10-12. The withdraw path in
`rs/moq-net/src/ietf/publisher.rs` writes PUBLISH_NAMESPACE_DONE then FIN on
drafts 14-16, a plain FIN on 17-18, and aborts both directions on 19+.
Draft-17 §3.3.1 and draft-18 §3.3.2 both say a request is cancelled by
resetting or STOP_SENDING, and that a PUBLISH_NAMESPACE is withdrawn by
cancelling it. FIN is allowed only for SUBSCRIBE_NAMESPACE and
SUBSCRIBE_TRACKS.

Decided 2026-10-12:

- Abort both directions on every version after 16. Drafts 14-16 keep
  PUBLISH_NAMESPACE_DONE.
- Keep `fin_cancels` accepting a peer FIN as cancellation on 14-18: lenient
  on receive.
- Extend `namespace_requester_fin_is_version_gated` (or a sibling) to assert
  a reset on 17 and 18.
- Mirror in `js/net/src/ietf` if it has the same gap.
