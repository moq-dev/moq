# [XS] CMake qlog option for OBS

## Goal

A CMake option builds moq-ffi with `--features qlog`, so an OBS build shows
the qlog directory setting (`moq::qlog_supported()`) and can capture traces.
It is off by default.

## Plan

Decided with the maintainer on 2026-10-17: the goal as drafted, from the
follow-ups of [#5726](https://github.com/moq-dev/moq/pull/5726).

`cpp/moq/CMakeLists.txt` runs the `cargo build` for moq-ffi, and `cpp/obs`
builds it in-tree. A prebuilt package (`MOQ_FFI_DIR`) is default-features
only, so the option must fail at configure with one instead of silently
dropping the feature. Mention the option in `doc/bin/obs.md`.

## Related

- [C++ FFI reuse](/quest/m2/cpp-ffi-reuse.md) - the shared prebuilt FFI carries default features only
