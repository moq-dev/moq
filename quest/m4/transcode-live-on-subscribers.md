# [S] A transcode rung live-encodes only for live subscribers

## Goal

A fetch or a track-info query on a `moq-transcode` rung starts no live encode and no shared decode; only a live subscriber does, with or without a session budget.

## Plan

Today any track demand starts a rung's live path (`rs/moq-transcode/src/rung.rs`, `live`), and `track::Demand` counts fetches and queries. So a backfill fetch, or `moq fetch --group N`, also subscribes the source, runs the shared decode, and keeps a live encoder whose groups nobody reads until that fetch is done.
Under a session budget, `Want` already gates the live path on `track::Producer::subscription()`, since a fetch-started live encode held the only session the fetch then waited for.
Gate it the same way without a budget and delete `Want`'s demand branch, rewriting the tests that use a fetch or query as the rung's only demand to drive the live path (`retirement_rides_out_an_open_live_group`, `retirement_waits_for_an_unclaimed_fetch`, `ended_demand_finishes_the_open_live_group`, `ended_demand_opens_no_new_live_group`, and others in `rs/moq-transcode/src/lib.rs`).
A newest-group fetch subscribes (`moq_relay::fetch_group(None)`), so it still gets a live encode.

## Waiting

- Maintainer: keep fetch and query demand from starting a rung's live encode, reversing `rung.rs`'s documented "any demand, including a fetch" design? Options: gate on live subscribers always (recommended: a fetch never reads the live groups, so the live encode and shared decode it starts are waste), or keep any demand starting it without a budget.
