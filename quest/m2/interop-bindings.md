# [L] Kotlin, Swift, and Dart join the interop matrix

## Goal

`just test interop --all` publishes and subscribes through the Kotlin, Swift,
and Dart bindings against every other client, so a wrapper regression in
network delivery fails CI.

## Plan

At the m0 coverage audit the matrix had five publishers (Rust, Python, Go,
C++, browser JS) and nine subscribers (those five, native Node, native Bun,
C, GStreamer). The binding tests cover less than it looks: Kotlin's and
Swift's smoke tests never deliver a frame over a connection (only a connect
that fails), and Dart's round trip stays inside one Dart process.

Every binding wraps moq-ffi, so this tests each wrapper's marshalling of
timestamps, catalogs, and close, not the Rust core the Python, Go, and C++
clients already cover.

- Add `test/interop/clients/{kt,dart,swift}` speaking the same CLI contract
  as the Python and Go clients, built from this checkout, and register them
  in `test/interop/interop.sh` for both roles.
- Kotlin on the JVM and Dart run on the existing Linux interop runner.
- Open: Swift. On Linux it needs a Swift toolchain in the interop job; the
  alternative is a macOS nightly job that runs only the Swift pairs.
  Recommendation: Linux, so one matrix covers every pair, unless the
  toolchain pushes the job past its time budget.
- Add `kt/`, `swift/`, and `dart/` to the interop workflow's PR path filter
  once their clients are in the matrix.

Public API: none. Wire: none.

## Related

- [Mobile completion](/quest/m2/mobile-completion.md) - the Swift and Kotlin bindings over Rust-owned capture
