# [M] Auth outage tests on a paused clock

## Goal

The moq-relay and moq-auth outage tests run on tokio's paused clock again and
assert both bounds: a session (or grant) survives an auth outage until its
`expires`, and closes at `expires`, not later. No wall-clock sleeps, no
widened timeouts, and no dependence on how fast the OS delivers loopback.

## Plan

- The tests: `an_outage_keeps_the_session_until_expires` in
  `rs/moq-relay/tests/auth_lifetime.rs`
  ([#4244](https://github.com/moq-dev/moq/pull/4244)) and
  `an_outage_keeps_the_grant_until_expires` in `rs/moq-auth/src/client.rs`
  ([#4291](https://github.com/moq-dev/moq/pull/4291)). Both moved to the real
  clock because a paused clock auto-advances while the runtime waits on a
  real socket, so a virtual timer fired before macOS delivered loopback. That
  swapped one violation of "unit tests mock time" for another, and #4244
  dropped the upper bound. Read both PR descriptions: they list what was
  tried and why it failed (restoring a listener probe, pausing after setup,
  waiting on the log).
- The race is real sockets under virtual time, so fix it by taking the
  sockets out of these tests. Look at what the codebase already offers
  before building anything: `rs/moq-net/tests/support/mock.rs` (an in-memory
  session pair), `moq_relay::auth::Auth::embedded` with its `Admissions`
  (decides leases in-process), and the lease driver in moq-auth, which could
  be exercised against an in-process answer source instead of HTTP. If the
  relay's `Connection` or moq-auth's `Client` cannot take such a transport,
  prefer the small seam that lets them over a test-only shim.
- Decide where each assertion belongs. The outage semantics (a 503 keeps the
  grant until `expires`) are moq-auth's; the relay test may only need to show
  that a lease reaching `expires` closes the session as `Expired` and reports
  `end`. Don't keep two tests proving the same thing.
- Measure against tokio's clock, not `SystemTime`: the grant still carries a
  wall-clock `expires`, so pin how it maps onto the paused clock.
- Other paused-clock tests touch real sockets and would share the hazard
  once a timeout lands on their path. #4291's audit named
  `a_grant_within_clock_skew_stays_live` (moq-auth) and
  `fixed_addresses_keep_tls_name_and_request_host` (moq-tokio websocket);
  moq-auth's `clock_server` helper exists only to keep axum on the paused
  clock. Move those onto the same seam if it is cheap.
- Prove it: loop the tests with every core loaded, on macOS if available,
  and mutate the deadline both ways (close early, close late) to see each
  bound fail.

Public API: none unless a transport seam is needed; report it if so. Wire:
none.

## Related

- [More tests under load](/quest/m1/test-flakes-2.md) - the same rule
  applied to other load-only failures
- [moq-shaper virtual time](/quest/m1/shaper-virtual-time.md) - the same
  paused-clock-versus-real-socket fight in moq-shaper
- [#4280](https://github.com/moq-dev/moq/pull/4280) - moq-archive and
  moq-hls tests poll with real-clock sleeps, on the archive track-timeline
  line
- [#4281](https://github.com/moq-dev/moq/pull/4281) - OBS `WaitFor` polling,
  on the C++ line
- [Nightly 2026-09-26](https://github.com/moq-dev/moq/actions/runs/36240326747/job/108399481809) -
  the macOS relay tarball job failed this test with "publisher connect
  timeout", before #4244 landed
