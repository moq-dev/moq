# [M] The bindings reach the new decode, route, and connection APIs

## Goal

moq-ffi, libmoq, and every wrapper (Python, Go, Swift, Kotlin, Dart) expose
three surfaces that landed in Rust only: the audio decode delay
(`decode::Options::delay` and `Consumer::delay()`), where a route came from
(`Route::source()` and `origin::Consumer::local()`), and libmoq's connection
timing (`failover_delay_us`, `resolution_delay_us`) beside the WebSocket
fallback settings. moq.pro's Python sidecar reads route sources from here.

## Plan

- One PR, each wrapper touched once, in its own idiom: durations as the
  language's duration type where the wrapper already uses one, handles over
  flat methods where a surface has more than one call.
- libmoq gets the decode delay and route source in `rs/libmoq` itself, with
  tests, not only in the C docs; regenerate `moq.h`. The additions are
  additive, so `cpp/obs/src` and `doc/bin/obs.md` change only if OBS uses them.
- Update `doc/lib/{py,swift,kt,go,dart,c}` in the same PR.
- Test each surface in every wrapper that has tests.

## Required

- Native decode delay (#3967) has merged
- Route source (#3972) has merged
- moq-ffi WebSocket fallback settings (#3961) have merged

## Related

- [FFI shape](/quest/m1/ffi-shape/README.md) - reshapes the flat methods this adds
