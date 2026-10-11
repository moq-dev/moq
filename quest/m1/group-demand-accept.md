# [S] An accepted group request's demand ends cleanly

## Goal

Once a group request is answered with `accept`, its `group::Demand` ends the
way a dropped request's does: `used()`, `unused()`, and `closed()` return
`Error::Dropped`, which moq-ffi's `MoqGroupDemand` reports as `Closed`. A rejected request's demand
still fails with the reject reason.

## Plan

Today `group::Request::accept` stores `rejected = NotFound` so a fetch that
joins the answered request fails, and `Demand::abort_reason`
(`rs/moq-net/src/model/group.rs`, `DemandSource::Fetch`) reads that same
field, so every binding sees `NotFound` after an accept. Keep the
joined-fetch refusal in its own field and let the demand read only an actual
reject.

Decided 2026-10-10 (found in #5139): clean close after accept, reject keeps
its error. Add a moq-net test that both outcomes hold for `used()`,
`unused()`, and `closed()`, flip moq-ffi's
`group_request_demand_fails_after_accept`, and update the `MoqGroupDemand`
docs in Rust, Go, Python, and Swift. Hand-written moq-c is frozen, so it gets
no change.

Public API: behavior only (the error a demand wait returns). Wire: none.

## Related

- [Track request demand](/quest/m1/ffi-shape/track-request-demand.md) - its demand should end the same way
