# [S] Every binding proves the audio config it exposes

## Goal

A regression in the moq-ffi audio surface fails a binding's own test, not a
consumer's. Today only Python asserts a sub-millisecond Opus frame duration
(`py/moq-rs/tests/test_local.py`), Go pins only the 20 ms default, Kotlin and
Swift never set one, no Kotlin test proves the fallible configuration setters
throw (#3642 added the Python and Swift cases and skipped `kt/`), and
`smoke-full` publishes audio from no binding with an explicit codec config.

## Plan

Add a 2.5 ms `frame_duration_us` case beside the existing audio smoke test in
`go/wrapper/moq_test.go`, `kt/.../SmokeTest.kt`, and
`swift/Tests/MoqTests/SmokeTests.swift`, asserting the encoder accepts it and
refuses a value outside the Opus set. Add a Kotlin test that a setter called
during an in-flight connect throws and the client is cancelled, mirroring
`py/moq-rs/tests/test_server.py`. Give the smoke publishers under
`test/smoke/` an Opus config with a non-default frame duration so
`just test smoke-full` exercises the audio path across bindings. Dart omits
audio by design ([Dart codecs](/quest/m2/dart-codecs.md)).

Public API: none. Wire: none.

## Required

- [Merge dev](/quest/m1/merge-dev.md) - the moq-ffi surface under test is dev's
