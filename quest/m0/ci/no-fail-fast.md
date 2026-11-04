# [XS] Local test runs honor NEXTEST_NO_FAIL_FAST

## Goal

`just check` and `sh/rs/nextest.sh` pass `--no-fail-fast` to nextest when
`NEXTEST_NO_FAIL_FAST` is set, so one host-specific failure doesn't hide the
rest of the suite's results.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs), milestone:

- ✅ m0, as a child of [CI cost and latency](/quest/m0/ci/README.md)
- m1
- m2

- Trace how `just check` reaches nextest (`sh/rs/nextest.sh` execs
  `sh/local.sh`) and pass the flag through there, so every entry point gets
  it.
- Check first whether the pinned cargo-nextest already reads the variable
  itself; if so, make sure nothing in between overrides it.
- Default behavior stays fail-fast.

Public API: none. Wire: none.
