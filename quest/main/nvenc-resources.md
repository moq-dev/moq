# [L] A safe NVENC resource and completion contract

## Goal

The safe moq-nvenc facade covers its actual consumers and cannot submit forged
handles, dangling pointer-bearing settings, or buffers that disappear before
the driver finishes. Raw SDK access stays explicitly unsafe. Buffer reuse and
bitrate reconfiguration can coexist without a self-referential wrapper.

## Plan

`safe/buffer.rs` exposes implementable input/output traits and a safe generic
registration method taking an arbitrary pointer plus an unrelated marker.
`safe/session.rs` accepts raw picture structs containing pointers, and
`safe/encoder.rs` shallow-copies pointer-bearing configuration. Submission
borrows buffers only for the call, although completion can come later.

Narrow the safe facade to operations needed by moq-video. Seal handle traits
unless an external implementation is required. Retain the backing allocation,
device/context, registration, and configuration data for their actual use
lifetimes; make escape hatches unsafe with explicit obligations. Completion
must release resources in the right order on success, failure, cancellation,
and session teardown. Avoid recreating the entire SDK as a second codec API.

Choose owned resource/session relationships that permit reuse and rate changes;
the current Session borrows prohibit mutable reconfiguration while buffers
exist. Pooling itself is deferred. Repeated SDK EOS is permitted and is not
by itself a reason to make that operation terminal.

Use a test driver boundary to verify pending submissions, delayed completion,
cross-session rejection, partial initialization, and teardown in CI without
an NVIDIA device. Add compile-time coverage that safe callers cannot forge a
handle or free in-flight storage. Keep hardware encode/drain proof separate.

Public API: narrowing and ownership changes in moq-nvenc, with moq-video
adapted in the same PR. Wire: none.

## Related

- [NVENC reuse](/quest/next/nvenc-reuse.md) - pooling follows the ownership contract
- [NVENC recovery](/quest/next/nvenc-recovery.md) - failure and reconfiguration behavior beyond the ownership repair
