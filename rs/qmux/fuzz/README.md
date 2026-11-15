# Fuzzing qmux

Coverage-guided fuzzing of the QMux framing a relay reads from any peer: records over a
byte stream (TCP, TLS, a Unix socket) and messages over a WebSocket, the frames inside
them, and the session they drive.

```bash
cargo install --locked cargo-fuzz
just rs fuzz qmux
```

Extra arguments pass through to libFuzzer, so `just rs fuzz qmux -- -max_total_time=300`
bounds a run. The Nightly workflow runs it for fifteen minutes, resumes from the previous
run's minimized corpus, and uploads failure inputs as workflow artifacts.

## How it fits together

The target body is not in `fuzz_targets/`; it is in `qmux`'s `fuzz` module
(`src/fuzz/session.rs`), because it builds the peer from the crate's private transports,
and so `just check` replays it on the pinned stable toolchain. The shim here also registers
the counting allocator the body measures with.

`just rs fuzz` regenerates `seeds/` from `qmux::fuzz::seeds()` before each run. See
[moq-mux's README](../../moq-mux/fuzz/README.md) for the corpus layout and calling
`cargo fuzz` directly.

The input's first byte picks the QMux version, the framing (byte stream or WebSocket), and
whether the server reads what it accepts. The rest is a script for a client session: open,
write, finish, reset, and stop streams, read echoes, send datagrams, advance the clock,
close, or drop the socket. Three actions go around the session: a record of arbitrary frames
handed to its transport, arbitrary bytes written straight onto the socket, and a socket it
stops reading. The server is accepted as the relay accepts one (`ws::Upgraded::accept`, or
`Session::accept` over a byte stream) and echoes bidirectional streams, reads
unidirectional ones and datagrams, or holds them all unread. Everything runs on the
simulated clock with a fixed seed, so a failing input replays exactly.

## What the target asserts

Beyond "does not panic":

- The heap the server allocated for the session peaks within a budget its limits set: its
  receive and send windows, the datagram buffer, a record per stream it may hold, and its
  transport's buffers. The allocator tags what the server's tasks allocate, so the peer's
  own buffers never count, whichever side frees them.
- While the peer sent only well-formed traffic, every stream reached the server's app byte
  for byte and ended as the peer ended it, every echo came back the same, and every
  datagram arrived intact.
- Once the peer leaves and the clock runs past the idle timeout and the close bound, the
  server's driver and app tasks have finished and it holds none of what it allocated.

A session the peer's garbage ends is not a finding.

## When something fails

Fix the bug, then commit the input under `regressions/qmux/`, which
`fuzz::tests::regressions` replays as part of the normal test suite, heap checks included.
