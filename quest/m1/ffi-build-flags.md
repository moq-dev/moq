# [S] One definition of moq-ffi's default build flags

## Goal

Every binding that builds moq-ffi into the shared target directory takes its
features from one place, so a binding whose features drift cannot silently
overwrite `libmoq_ffi.so` for the others again.

## Plan

[moq-dev/moq#6716](https://github.com/moq-dev/moq/pull/6716) found that a
cdylib's file name carries no feature hash, so builds with different features
overwrite each other. Today the flags are repeated: `sh/kt/generate.sh`,
`sh/swift/check.sh`, `sh/go/stage.sh`, `sh/dart/generate.sh`, and
`cpp/moq/CMakeLists.txt` each pass `--features openh264`. Define them once,
and make a build that differs use its own target directory or fail loudly.

Public API: none. Wire: none.
