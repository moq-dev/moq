# [L] Finish controlled-time test coverage

## Goal

Remaining test-only and private clock consumers use controlled time, and the
workspace lint prevents new direct reads outside backends and measurements.

## Plan

Production code and exported APIs already read time through `moq-clock` and
`@moq/clock`. This follow-up owns the legacy test conversions, the
workspace-wide lint ratchet, and the final audit of harness exceptions.

Decided with the m1 migrations, so this sweep doesn't reopen them:

- **Sim first.** A test with no real I/O runs on the `moq-clock` sim
  (`#[moq_clock::test]`). One that cannot run there yet uses paused tokio,
  whose clock `moq-clock` reads.
- **Real I/O syncs on events.** A test with sockets, browsers, codecs, or
  hardware never uses paused tokio, which auto-advances to the next timer while
  the OS event is still in flight. Wall time is allowed only for its watchdog
  and explicit performance measurements.
- **The ratchet.** Today each crate's `clippy.toml` bans direct clock reads and
  timers (`disallowed-methods`, `disallowed-types`), and tests opt out with
  `expect(clippy::disallowed_methods)`; the biome `noRestrictedGlobals`
  override skips `*.test.ts`. The sweep removes those opt-outs as it converts,
  leaving only the backend modules.
- **Third-party std clocks.** An API that takes `std::time::Instant` (str0m,
  srt-tokio, winit, qlog) converts with `Instant::into_std` and
  `Instant::from_std`. Device threads outside any runtime keep
  `std::thread::sleep` under a per-site `#[allow]`, since they have no ambient
  clock (decided 2026-10-20 by the maintainer, in
  [#6135](https://github.com/moq-dev/moq/pull/6135)).
