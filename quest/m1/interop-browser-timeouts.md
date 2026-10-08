# [M] Browser interop cells pass with FFI publishers

## Goal

The `go -> js` and `python -> js` cells of `just test interop --all` pass
reliably on `main` and in nightly CI, with the cause found and fixed. They
time out today at 32 to 33 s against a 30 s limit, on `main` too (run
37511624075), and on the auth line the browser's tone subscription is reset
with code 0.

## Plan

Decided 2026-10-07: investigate now rather than wait for nightly results
after #5005 (expiry-wakes, since landed), which may have been the cause: an
FFI publisher's 2 s audio max age parked hundreds of group serves.

- Reproduce locally with `just test interop --all`, under load and alone.
  The js cell (`test/interop/interop.sh` js subscriber, `driver.ts`) waits for
  rendered video, then audio, pause, and resume, each within the timeout;
  `harness.ts` `waitForWatch` has its own hard-coded 30 s.
- Find which step stalls and why, from the player trace and the publisher
  side. Check whether #5005 alone fixed it before changing anything else.
- Never raise the timeout or add a retry.
- Found 2026-10-08 while landing #4225: an FFI publisher's serve loop can
  starve moq-ffi's single runtime thread until the relay times it out.
  Re-check these cells once [Serve budget](/quest/m0/serve-budget.md) lands.

Public API: none expected. Wire: none.

## Related

- [Serve budget](/quest/m0/serve-budget.md) - likely the same stall
