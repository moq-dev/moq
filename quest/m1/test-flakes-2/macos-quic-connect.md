# [S] moq-tokio QUIC connects hold up on macOS

## Goal

moq-tokio's loopback QUIC tests pass reliably on the macOS Platform job.
Recent queue runs failed there while the same tests pass on Linux:

- `close_code::abort_then_drop` (`rs/moq-tokio/tests/integration/close_code.rs:91`),
  "the connection did not close: Elapsed", in
  [run 39302614921](https://github.com/moq-dev/moq/actions/runs/39302614921/job/118231847253),
  whose PR only touched Windows video code.
- `client::tests::fixed_target_preserves_request_and_refuses_redirect`
  (`rs/moq-tokio/src/client.rs:860`), "QUIC failed: timed out" after 10 s
  against a pinned `127.0.0.1` server, in
  [run 39344768657](https://github.com/moq-dev/moq/actions/runs/39344768657/job/118377253144),
  whose PR only touched JS dependencies.
- `close_code::reject` at the same line in
  [run 39099065494](https://github.com/moq-dev/moq/actions/runs/39099065494/job/117591454363),
  on a PR that changed moq-quic's close path, so it may be that PR's own failure.

## Plan

Found by the 2026-10-19 test cost audit. On Linux, 30 stressed runs of each
test passed (120 total), so the cause is macOS-specific or load-specific.

Both shapes are a QUIC handshake to a loopback server that never completes.
`close_code` dials `localhost`, which resolves to `::1` as well as the
`127.0.0.1` the server binds, so first rule out an address race that waits
on the unbound `::1`. `fixed_target` dials `127.0.0.1` directly, so that
does not explain it. Next suspects: macOS UDP socket options (GSO, ECN,
receive buffer) dropping the client's Initial, then a lost Initial waiting
out the handshake timeout.

Reproduce on macOS with the job's `just` recipe in a loop before changing
anything. Per the epic's rules, never raise the 10 s bounds or add a retry.

Public API: none. Wire: none.
