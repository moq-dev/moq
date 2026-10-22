# [XS] Land cancellation-safe FFI reads and accepts

## Goal

On the current release line, cancelling a foreign read or accept cannot
consume the next frame or incoming session. Existing synchronous server
cancellation still releases its listener without deadlocking, and runtime
shutdown remains safe. Published signatures and wire behavior stay intact.

## Plan

The paired read/accept ownership fixes are implemented in
[the release backport](https://github.com/moq-dev/moq/pull/6355), preserving
published signatures and the release runtime/listener APIs. Its tests exercise
the actual UniFFI cancel/free boundary, including delivery before freeing.

The release-pinned check gate, 159 FFI tests, 73 Python tests, and all 32
interop publisher/subscriber pairs pass. Remaining work is to land that PR
after review and CI. Delete this quest and its links once
the backport merges into `release`; the tracking update alone does not finish
it. Keep the additive draining-shutdown API in its separate backport.

## Related

- [Draining shutdown](/quest/m0/ffi-shutdown-backport.md) - coordinate overlapping FFI files while keeping its additive drain API in its own backport
