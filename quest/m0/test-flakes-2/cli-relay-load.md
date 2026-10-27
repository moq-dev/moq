# [S] moq-cli relay tests under load

## Goal

`real_relay_matches_http_fetch` (`rs/moq-cli/src/fetch.rs`) and the moq-cli
`announced` network tests pass under a parallel `just check`.

## Plan

- Reproduce under load first; the failure mode is not yet captured. The fetch
  test runs on the wall clock against a real relay and curls right after the
  publisher connects, so a routing race is the leading suspect.
- Fix by waiting on the relay's announce or route event before fetching. Keep
  the fetch test on the real relay, which is the HTTP routing and response
  coverage it exists for. Never a sleep or retry.
