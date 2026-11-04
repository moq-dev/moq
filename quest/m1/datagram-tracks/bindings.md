# [M] Bindings datagram tracks

## Goal

moq-ffi, moq-c, and every wrapper (Python, Swift, Kotlin, Go, Dart, C++)
expose the separate datagram producer and consumer types instead of
datagram methods on stream tracks, mirroring Rust's names.

## Plan

Follow the [epic](/quest/m1/datagram-tracks/README.md)'s decisions.
The implementation in [moq-dev/moq#7847](https://github.com/moq-dev/moq/pull/7847)
removes the transitional stream/datagram coupling, exposes the separate types,
aggregate subscriber priority and `Requests::datagrams()` handlers, and updates
the wrappers, generated bindings and migration notes. No JS implementation is
owned here; [moq-dev/moq#7846](https://github.com/moq-dev/moq/pull/7846) owns it.

What remains is legacy on-demand lookup when both stream and datagram handlers
are alive. On lite-05/06, Rust's `lite::publisher::Legacy` first requests the
stream track, and only asks for datagrams after a not-found result. A stream
handler that leaves an unknown stream request pending therefore prevents the
datagram request from reaching its handler. The bindings cannot reject a stream
request as not-found: their rejection method carries an application error.
Published datagram-only tracks on lite-06 and on-demand datagram-only tracks on
lite-07 work without stream demand.

The reproduction changed `requested_datagrams_cross_lite07` in moq-ffi's tests
to negotiate the default versions while retaining both handlers: it timed out
waiting for the datagram request. Its lite-07 version passes. Resolve the
legacy lookup policy below, then finish the PR's checks and local review.

Public API: breaking for every binding's datagrams. Wire: none.

## Waiting

- Maintainer: choose how legacy on-demand kind lookup should work when both handlers are live. Options: (1) change Rust's legacy lookup to discover the datagram kind without waiting for the stream handler (recommended; preserves the independent binding API), or (2) require stream handlers to reject missing names and add a not-found rejection to the bindings. This changes compatibility behavior or the public rejection API, which this unattended implementation was not authorized to settle. Verify with the default-version reproduction above; lite-07 and the static default-version tests already pass.
