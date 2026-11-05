# [L] Connect policy in moq-net

## Goal

moq-net owns client connect policy as poll machines: the connect deadline over
dial plus handshake, and reconnect with backoff, GOAWAY handling and the
candidate walk. moq-net does no I/O: the caller supplies each dial attempt and
the machine decides when to start, abandon, back off or move to the next
candidate. moq-tokio's `Client`/`Connection` drive these machines instead of
implementing the policy.

## Plan

Part of [Session policy lives in moq-net](/quest/m1/session-policy/README.md);
see it for the decisions and research. Decided 2026-11-04 (maintainer):
reconnect moves too, not just the deadline.

- Today's policy: the connect deadline in moq-tokio's `client.rs` (default 30 s
  in `connect.rs`), and `Backoff`, `Goaway`, `run`, `attempt_timeout`,
  `settles_walk` and `refreshed` in `connection.rs`. All of it runs on
  `kio::clock` and `select!`; only the dialing (DNS, Happy Eyeballs, sockets,
  TLS) is tokio-specific and stays in moq-tokio.
- Keep the current behaviour and defaults; port the existing
  `connection.rs` tests to the moq-net machines on kio's simulator, so the same
  timings are asserted without tokio.
- This also gives a future io_uring client
  ([uring-cluster-dial](/quest/m3/uring-cluster-dial.md)) the policy to reuse
  rather than copy.

Public API: breaking in moq-net's `Client` and new reconnect types; moq-tokio's
`Connection` API keeps its shape where possible, with MIGRATE notes otherwise.
Wire: none.
