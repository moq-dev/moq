# [S] A d19+ FETCH survives a request FIN on release

## Goal

On `release`, a FETCH or joining FETCH on draft 19+ keeps serving after the
requester FINs its side of the request stream; a reset or STOP_SENDING still
cancels it, and drafts 14-18 keep cancelling on FIN.

## Plan

#5133 gated SUBSCRIBE and SUBSCRIBE_NAMESPACE with `fin_cancels` in
`rs/moq-net/src/ietf/request_stream.rs` and left FETCH alone. Apply the same
gate to the FETCH paths in the IETF publisher, with a version-swept test like
`requester_fin_is_version_gated_for_subscriptions`. Check whether `main`'s
FETCH path already behaves this way; if not, land it there first.

Raise its priority if a Seattle peer FINs after FETCH.
