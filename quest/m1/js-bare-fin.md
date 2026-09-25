# [S] JS bare FIN

## Goal

A `@moq/net` subscriber aborts a track whose subscribe stream FINs before the
publisher declared its end, over moq-lite and IETF, as Rust does since #4083.
A bare FIN is a failed request, never a clean end.

## Plan

- The drafts agree: draft-19 section 3.3.2 treats a FIN before the required
  messages (PUBLISH_DONE) as a failure, and the moq-lite draft has a publisher
  FIN only after SUBSCRIBE_END.
- Abort with the error Rust uses, so both languages report the same thing. Add
  an interop case where a publisher FINs without declaring an end, in both
  directions.

The clean-end path this tightens landed in #4086.
