# [XS] Worker socket count sees only its own listener

## Goal

`dropping_a_server_keeps_its_socket` in `rs/moq-tokio/tests/worker.rs` counts
only the sockets its own listener holds, so an unrelated UDP socket elsewhere
on the machine can't fail it.

## Plan

`udp_sockets_on(port)` counts every `/proc/net/udp` entry whose local address
ends in the port, so another process's socket on the same port number but a
different address (an ephemeral port on another interface, say) breaks both
the `>= 2` assertion and the final `== 0`.

- The listener binds `127.0.0.1:0` (`listen_config`), so match the full local
  address rather than the port suffix.
- The final `== 0` runs after the port is released, when a parallel test may
  bind the same address. If that window matters, count only this process's
  sockets by joining the table's inode column with `/proc/self/fd`. Prefer
  whichever holds with a stranger on the port at every assertion.
- Reproduce by holding a UDP socket on the same port on another local address
  while the test runs.

Public API: none. Wire: none.
