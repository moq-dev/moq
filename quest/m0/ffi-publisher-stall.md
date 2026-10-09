# [S] FFI publishers stay connected through interop

## Goal

Every cell of `just test interop --all` with a Go or Python publisher passes
reliably on `main` and in nightly CI. Today a moq-ffi publisher goes silent,
not even sending QUIC keep-alives, until the relay's 10 s idle timeout closes
its connection and it reconnects. The browser cells (`go -> js`,
`python -> js`) can't absorb that within their 30 s limit and fail at 32 to
33 s; `-> rust` and `-> gst` pass slowly at 10 to 11 s, and in some runs most
cells of one publisher fail at 11 s. The harness fails any cell whose
connection idles out, so a drop and reconnect can no longer pass as a slow
cell.

## Plan

The cause is known: a serve loop with work always ready never yields and
starves moq-ffi's single runtime thread until the relay times the publisher
out (found 2026-10-08 landing #4225). [Serve budget](/quest/m0/serve-budget.md)
fixes it, so this quest starts once it lands and no bisect is needed.

Decided 2026-10-08:

- In m0, moved from m1 and widened from the browser cells: the stall masks
  interop on every wire PR and may disconnect real FFI publishers.
- Re-check every Go and Python publisher cell once the serve budget lands.
  The #4225 root-cause comment saw a 64-iteration yield still time the
  publisher out, so if a cell still stalls, find the rest of the cause.
  Never raise a timeout or add a retry.
- The harness fails a cell when any connection in it idles out, so this
  class of regression fails every run instead of passing slowly.
- The harness shuts down every client it starts cleanly, so every idle-out
  counts. Today `gst-launch` dies on SIGPIPE and `moq-cli` can be killed
  under `timeout -k`, which may leave a QUIC connection to idle out. No
  exception list for killed clients: it would hide a real idle-out behind a
  harness kill.

Harness facts (2026-10-08):

- The one-byte subscribers exit on their first byte, so they pass even when
  the publisher stalls after it; `-> rust` and `-> gst` exit only on their
  next write after `head` closes, and the browser cells wait for rendered
  video, then audio, pause, and resume. Judge a fix by those.
- The js cell (`test/interop/interop.sh` js subscriber, `driver.ts`) runs
  each step within the cell's timeout, and `harness.ts` `waitForWatch` has
  its own hard-coded 30 s.
- One relay serves the whole run, and its `connection closed err=... timed
  out` warning (`rs/moq-relay/src/relay.rs`) names no connection and lands
  about 10 s after the peer went quiet, often in a later cell. Tie each
  idle-out to its connection first, by logging the connection there or
  having the client report its own drop.

Public API: none expected. Wire: none.

## Required

- [Serve budget](/quest/m0/serve-budget.md) - fixes the stall's cause; this quest verifies the cells and hardens the harness

## Related

- [FFI runtime](/quest/m1/ffi-runtime.md) - FFI apps run on more than one thread, which also hides the stall
- [Audio group duration](/quest/m1/audio-group-duration.md) - the go client's 2.5 ms Opus frames stop minting a group each
