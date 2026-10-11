# [S] A d19+ FETCH survives a request FIN on release

## Goal

On `release`, a FETCH or joining FETCH on draft 19+ keeps serving after the
requester FINs its side of the request stream; a reset or STOP_SENDING still
cancels it, and drafts 14-18 keep cancelling on FIN.

## Plan

#5133 gated SUBSCRIBE and SUBSCRIBE_NAMESPACE with `fin_cancels` in
`rs/moq-net/src/ietf/request_stream.rs` and left FETCH alone: release's
`run_fetch_stream` paths still watch `stream.reader.poll_closed` directly.
`main` already routes its FETCH paths through `request_stream::poll_cancel`,
so this is release-only. Do the same on `release`, with a version-swept test
like `requester_fin_is_version_gated_for_subscriptions`.

Raise its priority if a Seattle peer FINs after FETCH.
