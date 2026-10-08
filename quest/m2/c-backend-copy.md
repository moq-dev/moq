# [S] One payload copy out of the C backend

## Goal

A frame payload crossing moq-ffi's C backend (uniffi-bindgen-cpp fork,
#5007) is copied once, not twice, measured by a benchmark that lands with
the change.

## Plan

Find both copies (FFI buffer conversion and the C struct fill), remove one,
and add a benchmark over payload sizes so a regression shows up. Whether to
offer the C backend upstream (LiveKit, NordSecurity) is the maintainer's call
and out of scope.
