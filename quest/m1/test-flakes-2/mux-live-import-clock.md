# [S] moq-mux live import tests on the paused clock

## Goal

The moq-mux live import restart tests (`live_import_restarts_forward_after_idle`
for ts, fmp4, and flv) advance a paused clock across their idle gap and sleep
no real time.

## Plan

Facts (`origin/main`, 2026-10-01): the helpers (`live_import` in
`container/{ts,fmp4}/import_test.rs`, `import` in `flv/import_test.rs`) call
`std::thread::sleep(idle)` with a 300 ms idle. The gap is measured through
`clock::Anchor`, which reads `crate::Clock::now()`, and that is
`std::time::Instant::elapsed()`, so a paused tokio clock can't move it.

Decided (2026-10-01): `crate::Clock`'s monotonic epoch becomes a
`web_async::time::Instant`. That is tokio's clock on native, so the tests start
paused and call `tokio::time::advance`, and it takes `std::time::Instant` off the
wasm path. #4687 used the same `Instant` type, but only for the private SI
debounce after dropping its `crate::Clock`; `Clock` itself still reads
`std::time::Instant`. The wall mapping stays `SystemTime`. Rejected: injecting a `now` into the importers through
`translate_at`, which is plumbing only tests want.

`std::time::Instant` is public at the boundary: `Clock::at(epoch, wall)` is
re-exported from `moq-mux`, and the json and binary timed writes
(`json.rs` `update`/`append`, and their `binary.rs` twins) take
`Timed<_, Instant>` through the crate-private `Clock::stamp`/`capture`.
`moq_net::Timed`'s docs say moq-mux uses `std::time::Instant`. Callers pass
`std::time::Instant` from `test_util::late_clock`, moq-cli publish, moq-hls
export, moq-audio and moq-video capture, and the catalog producer tests.

Decided (maintainer, 2026-10-01): convert at the boundary. Public inputs stay
`std::time::Instant` and convert to the private epoch internally (native only),
so this is additive and stays on `main`. Rejected: switching the public types to
`web_async::time::Instant` as a break on `dev`.

Public API: none. Wire: none.
