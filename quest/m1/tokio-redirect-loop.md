# [S] moq-tokio escalates its backoff on GOAWAY redirect loops

## Goal

When two relays redirect a Rust client to each other after each session
outlives `backoff.initial`, moq-tokio's reconnect loop escalates its delay up
to `backoff.max` and gives up at `backoff.timeout` with a redirect-loop error,
instead of bouncing between them with no delay forever. A one-way move and a
same-place GOAWAY (an empty URI, a restart) still migrate at once.

## Plan

Found 2026-10-20 checking GOAWAY follow-ups of
[#5998](https://github.com/moq-dev/moq/pull/5998).
[#5959](https://github.com/moq-dev/moq/pull/5959) fixes this shape in JS
(`js/net/src/connection/reload.ts`) and lists Rust as unchecked. In
`rs/moq-tokio/src/connection.rs`, a GOAWAY off a session that lived at least
`backoff.initial` continues with no delay, because `Retry::settle` has
already reset the delay, start and last error for that session; only an
immediate redirect escalates.

- Mirror #5959's rule once it lands: while migrations are chained, remember
  the endpoints that redirected the client elsewhere, and route a redirect
  back to one of them through the escalating retry without the
  healthy-session reset. Clear the set when a session ends without a GOAWAY
  or a new sequence starts. Use the same error name as JS.
- Regression tests on a mocked clock, as #5959's: mutual A/B redirects are
  bounded and end at `backoff.timeout`; a one-way move and same-place
  GOAWAYs redial at once.

Decided 2026-10-20 (maintainer): no quest reissues individual requests a
GOAWAY interrupts. Same-epoch subscriptions already resume through route
swaps, namespaces re-advertise on the new session, and epochless requests
end, as instances are never stitched.

Public API: none. Wire: none. Behavior: a redirect loop now backs off and
can end the connection with an error.

## Related

- [#5959](https://github.com/moq-dev/moq/pull/5959) - the JS fix this mirrors
