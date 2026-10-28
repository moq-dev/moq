# [S] Backport the @moq/flate inflate bounds to release

## Goal

`@moq/flate` on `release` stops inflating a hostile slice once it passes its
cap and stops pinning pako's scratch buffer, as
[moq-dev/moq#6900](https://github.com/moq-dev/moq/pull/6900) fixed on main, and
publishes a patch release (npm has 0.1.0 to 0.1.3).

## Plan

Decided 2026-10-27 (maintainer): one PR against `release` per backport, in m0.

`release`'s `js/flate/src/index.ts` is the older per-frame version (a 64 MiB
`maxFrameSize`, not main's 32 MiB group budget), so port the two fixes onto its
shape rather than main's budget:

- `onData` only returns past the cap, so pako inflates the rest of the slice.
  Stop inflation at the cap and fail the frame.
- `concat()` passes a single chunk through, pinning pako's scratch buffer.
  Copy it out.

Public API: none. Wire: none.
