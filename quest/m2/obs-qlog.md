# [XS] CMake qlog option for OBS

## Goal

A CMake option builds moq-ffi with `--features qlog`, so an OBS build shows
the qlog directory setting (`moq::qlog_supported()`) and can capture traces.
It is off by default.

## Plan

Decided with the maintainer on 2026-10-17:

- The goal comes from the follow-ups of
  [#5726](https://github.com/moq-dev/moq/pull/5726).
- A standalone m2 quest, not a child of the obs-studio epic, which is about
  the native port into obs-studio rather than the plugin's build.

`cpp/moq/CMakeLists.txt` runs the `cargo build` for moq-ffi, but only on a
source build. `cpp/obs/CMakeLists.txt` builds `cpp/moq` in-tree only when
`MOQ_LOCAL` contains it; otherwise it fetches a published `moq-cpp` archive
for `MOQ_VERSION` with `FetchContent`. Both that archive and a prebuilt
`MOQ_FFI_DIR` package carry default features only, so the option requires
the in-tree source build and fails configuration with `FATAL_ERROR` on the
fetched archive or `MOQ_FFI_DIR` path instead of silently dropping the
feature.

Test: a CI configure of `cpp/obs` with the option on and `MOQ_LOCAL`
cleared must fail, and the in-tree build with it on must report
`moq::qlog_supported()` true. Mention the option in `doc/bin/obs.md`.

## Related

- [C++ FFI reuse](/quest/m2/cpp-ffi-reuse.md) - the shared prebuilt FFI carries default features only
