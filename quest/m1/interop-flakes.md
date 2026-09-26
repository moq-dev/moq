# [S] Interop harness runs clean in parallel

## Goal

`just test interop --all` passes reliably while other harness runs share the
machine. Two known flakes: concurrent Nix shells reserve the same port because
each keeps its reservations under its own `TMPDIR`, and the browser driver's
pause click is sometimes blocked by the canvas (`js -> js`).

## Plan

- Port reservations live under one root shared by every shell (not
  `TMPDIR`), or ports come from the OS (bind to 0 and pass the result). Prefer
  whichever removes the reservation file entirely.
- The pause control is driven in a way the canvas cannot intercept (an API or
  keyboard path, or waiting for the element to be actionable), not a retry.
- Prove it by running two `--all` harnesses at once, several times.

Public API: none. Wire: none.

## Related

- [#4181](https://github.com/moq-dev/moq/pull/4181) - where both flakes were seen
