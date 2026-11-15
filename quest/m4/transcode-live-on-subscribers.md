# [S] A transcode rung live-encodes only for live subscribers

## Goal

A fetch, a track-info query, or a subscription over past groups only on a `moq-transcode` rung starts no live encode and no shared decode; only a subscriber that reaches the live edge does, with or without a session budget.

## Plan

Decided 2026-11-14 (maintainer, picking [moq-dev/moq#8725](https://github.com/moq-dev/moq/pull/8725)): ✅ gate the live path on subscribers that reach the live edge always (rejected: keep any demand starting it without a budget). A fetch never reads the live groups, so the live encode and shared decode it starts are waste.

Today any track demand starts a rung's live path without a budget (`rs/moq-transcode/src/rung.rs`, `live`), and `track::Demand` counts fetches and queries. So a backfill fetch, or `moq fetch --group N`, also subscribes the source, runs the shared decode, and keeps a live encoder whose groups nobody reads until that fetch is done.
Under a session budget, `Want` already gates the live path on `reaches_live`: some range of the aggregate subscription is open, counts back from the latest group, or ends past the rung's newest group.
Apply that gate without a budget too and delete `Want`'s demand branch, rewriting the tests that use a fetch or query as the rung's only demand to drive the live path (`retirement_rides_out_an_open_live_group`, `retirement_waits_for_an_unclaimed_fetch`, `ended_demand_finishes_the_open_live_group`, `ended_demand_opens_no_new_live_group`, and others in `rs/moq-transcode/src/lib.rs`).
A newest-group fetch subscribes (`moq_relay::fetch_group(None)`), so it still gets a live encode.
