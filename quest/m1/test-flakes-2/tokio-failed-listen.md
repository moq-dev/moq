# [XS] moq-tokio's failed-listen test keeps its port reserved

## Goal

`server::tests::a_failed_listen_binds_nothing` (`rs/moq-tokio/src/server.rs`)
passes under load. It binds a probe to port 0, drops it, and has the server
bind that port; another process can take the port in between, so the tcp bind
fails for the wrong reason or the final rebind check does.

## Plan

Seen during the 2026-10-20 merge batch. The same probe-then-release pattern
was fixed in the redial tests by
[#6032](https://github.com/moq-dev/moq/pull/6032), which holds the failing
listener for the test's lifetime. That fix does not transfer as is: a held
probe makes the server's tcp bind fail first, so the test would never reach
the tcp-success, unix-failure path it exists to cover.

- Keep the port owned by the test or the server from first bind to final
  check, so no other process can take it in between, while still proving the
  server releases its partial tcp bind when the unix bind fails. The
  reservation has to be one the server takes over, not one the test keeps
  holding.
- Grep moq-tokio and moq-relay for other tests that probe a port and drop it
  before use, and fix each with whichever shape fits: held, as in #6032, when
  the port is meant to stay unusable, or handed over when the code under test
  must bind it.

Public API: none. Wire: none.
