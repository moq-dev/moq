# [M] Cancel pending Dart stream reads when listeners leave

## Goal

Cancelling an idle Dart announcement, grant, or request stream completes and
releases its pending operation without waiting for an unrelated native event
or closing a shared owner that remains in use.

## Plan

Implementation checkpoint is on [draft PR6420](https://github.com/moq-dev/moq/pull/6420). The unchanged announcement
async generator reproduced the deadlock with a deterministic pending read:
subscription cancellation and native cleanup stayed unfinished until an
external event completed the read. The generated bridge returned plain
Futures despite UniFFI exporting per-operation cancellation already.

The proposed generator-source patch, carried beside the existing Nix Dart
generator patch, returns `UniffiRustFuture<T> implements Future<T>`, adds
`cancel()`, and reports `UniffiCancelledException` when cancellation wins.
It polls cancellation through completion and frees the native operation once,
keeping its native callback alive through cleanup. Generated bindings come
from that patched generator. No Rust API, C ABI, checksum, or wire change.

One internal single-listener stream owns its pending read and at most one
paused result. Cancellation awaits cleanup; announcements then release their
owned cursor, while grants and requests retain their shared Auth or Server.
A request delivered concurrently with cancellation is cancelled and disposed.

Nine stream tests cover idle cancellation, delivery and error races,
backpressure, and exactly-once cleanup. Five bridge tests cover duplicate
cancellation, queued wakes, already-ready results, cleanup callbacks, and a
real announcement cursor reused after cancellation. A real server accepts a
new connection after cancelling an idle request subscription. No sleeps.

Remaining: settle the generated public cancellation API on the draft. Prefer
the Future subtype because current callers can keep awaiting calls while
stream wrappers retain cancellation ownership. An explicit operation handle
with a separate `future` field would make ownership clearer but require all
async callers to change. Keep the PR draft until the maintainer chooses;
final validation results and any environment limitations are in its body.
