# [L] OBS plugin on the generated C++

## Goal

`cpp/obs` links `cpp/moq` instead of libmoq. The output and source hold
`moq::` objects instead of `int` handles, async work is a future with a
continuation on OBS's own executor, and the generation counters, condvars,
`SessionRef`/`callback_state` heap trampolines, and "never take signal_mutex
inside a libmoq call" lock-order rules that exist only to survive libmoq's
single callback thread are deleted. Behavior visible to OBS users is
unchanged: same settings, same dock, same reconnect and teardown timing.

## Plan

- Executor: install a `moq::Executor` that marshals continuations onto the
  thread OBS expects for each site (graphics thread for source frames, the
  output's own thread for status), so no continuation runs on the moq-ffi
  runtime thread and none can stall other sessions.
- Output (`moq-output.cpp`): `moq::Client::connect` returns a future; the
  `attempt` generation guard becomes future ownership (a superseded attempt
  drops its future, which cancels). Reconnect stays in the plugin. Publish
  through `moq::Broadcast` producers and the media importer with the same
  catalog handling.
- Source (`moq-source.cpp`): `subscribe_catalog` / `subscribe_media` futures
  replace `moq_consume_*` callbacks; frame lifetime is the returned frame
  object, not a `free` call. FFmpeg decode stays as is; the native decode
  replacement is the OBS native codecs questline and now starts from this
  code.
- Tests: `cpp/obs/test/*-test.cpp` currently stub libmoq's C symbols to race
  the OBS and runtime threads. Rewrite them against `moq::` interfaces with a
  fake executor; keep the scenarios (stop during connect, late terminal,
  superseded attempt).
- Build and release: `cpp/obs/CMakeLists.txt` consumes the package
  (in-tree `cpp/` for `MOQ_LOCAL`, the release tarball otherwise); `obs.yml`
  rides `release-cpp.yml` instead of `libmoq.yml`. `doc/bin/obs.md` says the
  plugin is C++ over the generated bindings.
- Verification: `just obs compile` and `just obs test` on all three
  platforms; a manual publish and watch round trip against a relay with
  reconnect and mid-stream source deletion.

## Required

- [Package](/quest/m2/cpp/package.md) - the wrapper and CMake package OBS links

## Related

- [OBS native codecs](/quest/m2/obs-moq-video/README.md) - its quests require this migration and build on moq-ffi's audio and video types
