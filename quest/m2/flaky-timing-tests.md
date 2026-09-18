# [S] Three timing tests stop flaking under load

## Goal

`just test` on a loaded machine no longer fails these three, each of which
passes alone and fails on a busy scheduler:

- `rs/moq-relay/tests/auth_lifetime.rs` `an_outage_keeps_the_session_until_expires`:
  real-time sleeps and a 100 ms timeout measure a 4 s grant.
- `rs/moq-mux/src/clock.rs` `copies_share_one_epoch`: reads a real clock
  twice and asserts equality, so a microsecond boundary fails it.
- `rs/moq-hls/src/server/routes.rs` `a_session_crossed_cache_miss_answers_404`:
  a real session pair polled to a cache miss under a 1-entry pool.

## Plan

Fix each at the cause, not by widening a sleep: the relay test runs under a
paused tokio clock or asserts on the lease's own expiry event; the clock test
stops sampling `elapsed()` twice (each `micros()` call reads the monotonic
clock anew, so a shared epoch still crosses a boundary) and instead compares
what the copies store, the epoch and wall anchor a `new_at` clock reports
through `section()`; the HLS test drives the cache miss deterministically
(evict, then request) instead of polling. Run each 50 times under `nice -n -5 cargo build` load to show
the flake is gone.
