# [M] Audit test coverage

## Goal

Run the `tests` audit: `quest audit tests` prints what it checks, what each finding becomes, and how to plan its next run.

## Plan

Baseline: 2026-11-23 at `890ea4e531` (Nightly `45242597151` `rust-coverage` at `b29b90be29`, measured via `just rs coverage`):
`moq-auth` 98.1%, `moq-mux` 97.7%, `moq-hls` 97.6%, `hang` 97.5%, `moq-net` 96.1%, `moq-quic` 96.0%, `libmoq` 96.0%, `moq-audio` 95.9%, `moq-gst` 95.7%, `moq-relay` 95.3%, `moq-congestion` 95.0%, `moq-json` 94.4%, `kio` 93.9%, `qmux` 93.6%, `moq-ffi` 93.2%, `moq-rtmp` 93.1%, `moq-webrtc` 93.0%, `moq-tokio` 92.4%, `moq-uring` 89.6%, `moq-cli` 87.2%, `moq-e2ee` 86.2%, `moq-video` 65.3%, `moq-nvenc` 44.9%, `moq-v4l` 17.4%, `moq-vaapi` 12.2%.
In Nightly `45242597151`, `js-coverage` stopped at `js/net` (`lite` 96.7%, `ietf` 94.3%, `origin` 97.8%, `track` 97.9%, `resume` 94.9%) due to a failure in `widened_max_age.test.ts`; the next night's passed, so start from a Nightly whose `js-coverage` covered every package. The flake was fixed by [moq-dev/moq#9607](https://github.com/moq-dev/moq/pull/9607).

Previous run: [moq-dev/moq#6175](https://github.com/moq-dev/moq/pull/6175). Test cost belongs to [audit-cost](/quest/m3/audit-cost.md).

### Check first

- Start from Nightly's `rust-coverage` and `js-coverage` artifacts (`just rs coverage`, `just js coverage`) rather than re-running the instrumented workspace. Confirm every Nightly test leg (`rs features`, `rs audio-devices`, `js coverage`) passed before calling a file untested.
- Uncovered `pub` functions: walk lcov `DA:` records outside tests/benches/fuzz. At `b29b90be29`, gaps clustered in `moq-ffi` (33 fns: datagram/cancel/error helpers), `libmoq` (20 fns: datagram tracks, session stats), `moq-quic` (20), `moq-relay` (12), and `moq-auth` (7: HTTP `serve`/`serve_unix`/`client::connect`).
- Bun undercounts a module loaded again with a query suffix; verify JS gaps before acting, and never merge packages into one `bun test` process.
- `just gh check` gates `#[ignore]` tests and unreachable targets against `sh/gh/test-inventory.allow`.
- Sample `fix` commits since `890ea4e531` for missing regression tests (counting `#[kio::test]`; `d28ba17ddc` BBR3 pacing was the lone non-bench fix without a unit test in the prior 80).
- Check wall-clock sleeps in binding tests (`go/wrapper/moq_test.go:1800` polls goroutine cleanup; `py/moq-ffi/tests/exit_inside_callback.py:49` intentionally releases the GIL).
- Re-check every `.agents/skills/verify-*/SKILL.md` TLA+ model and `js/*/verify/` model against its covered files since its sync commit (decided 2026-11-10).
- Check whether [interop bindings](/quest/m3/interop-bindings.md) and [GPU CI](/quest/m3/gpu-ci.md) landed before re-auditing the tests they own.

## Audit

- tests

## Scheduled

- 2026-11-30 - one week after the 2026-11-23 pre-0.22 audit run

## Required

- [Release cut](/quest/m3/release/cut.md) - the release in flight ships before this run starts
