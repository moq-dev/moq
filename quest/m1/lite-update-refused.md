# [S] A refused lite SUBSCRIBE_UPDATE ends the subscription

## Goal

When the model refuses a lite SUBSCRIBE_UPDATE, the publisher's per-range state is left unchanged and the subscription ends with the refusal, as the initial SUBSCRIBE and the ietf publisher already do. Today `TrackRun::update` in `rs/moq-net/src/lite/publisher.rs` only uses `self.track.update(..).is_ok()` to decide `self.named`, then rewrites `ids`, `unstarted`, `frames`, `floor` and `cap` anyway, so the publisher's view and the model's diverge and SUBSCRIBE_STARTED can name a range with another range's start.

## Plan

Found by the 2026-11-07 invariant audit (lite-1). On `Err`, set `self.refused` and return before mutating, as the `Bounds::new` arm does; carve out `Closed` only if it is benign. Add a test: an update that swaps ranges for one the cache cannot settle ends the subscription instead of serving the removed range.
