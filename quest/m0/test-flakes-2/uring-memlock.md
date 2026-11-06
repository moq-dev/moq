# [XS] The dev host's locked-memory limit fits parallel sessions

## Goal

moq-relay's and moq-uring's io_uring tests stop failing ring setup with
`ENOMEM` on the user's shared `RLIMIT_MEMLOCK` when many agent sessions
share the maintainer's host. Done when `ulimit -Hl` in a new agent session
on that host prints at least 65536, the 64 MiB hosted CI already uses.

## Plan

Decided 2026-11-06 (maintainer, /quest-complete): ✅ raise the host limit and
keep this quest as a host action. Not chosen: cap the `io-uring` test group at
one thread (about +45 s per workspace check), or first make the 10 s endpoint
tests fast; add a public ring-geometry knob so tests use small rings.

Measured 2026-11-06 in [moq-dev/moq#8051](https://github.com/moq-dev/moq/pull/8051)
on that host (kernel 7.0, 8 MiB limit). The tests are not what fills the
budget:

- Processes other than tests held 4.1 to 4.9 MiB. Most of it was Node.js:
  libuv gives every event loop a 256-entry epoll-batching ring (28 KiB), and
  `UV_USE_IO_URING` does not turn that ring off. Each agent session's
  chrome-devtools-mcp alone holds nine of them (about 250 KiB), so the
  baseline grows with the number of sessions.
- One `just rs test -p moq-uring -p moq-relay --features moq-relay/io-uring`
  run needs 0.5 to 1 MiB of headroom. Twelve such runs started at once all
  passed at that baseline.
- The failures reported in [moq-dev/moq#7313](https://github.com/moq-dev/moq/pull/7313)
  (eleven moq-uring tests in one workspace run) leave the baseline above
  7 MiB at the time. No test-side cap brings that back under 8 MiB.

Measured and set aside:

- Capping the `io-uring` nextest group at one thread halves the headroom a
  run needs, but slows `just rs test --workspace --features moq-relay/io-uring`
  from 101 to 108 s to 149 to 153 s. Three endpoint tests in
  `rs/moq-uring/tests/integration/endpoint.rs` each sit out moq-quic's 10 s
  handshake timeout while holding a ring, which makes the serial group the
  tail.
- A lock shared by every invocation counts its wait against nextest's
  hang timeout. It also needs nextest's experimental wrapper scripts or a
  hook in every ring test.
- Giving integration tests the smaller unit-test rings needs a public
  geometry knob and stops testing production geometry.

## Waiting

- Maintainer: raise the host user's hard `RLIMIT_MEMLOCK` to 64 MiB, for
  example `LimitMEMLOCK=64M` in a `/etc/systemd/system/user@.service.d/`
  drop-in plus `<user> hard memlock 65536` in `/etc/security/limits.d/` for
  login shells. An agent cannot do this because it needs root;
  `sh/rs/nextest.sh` raises the limit with `sudo prlimit` only on hosted CI.
  Check with `ulimit -Hl` in a fresh session.
