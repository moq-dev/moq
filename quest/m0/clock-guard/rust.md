# [M] Spike a Rust real-timer guard

## Goal

Find out whether `kio` can fail a Rust test whose timer fires on the real
clock, as the JS guard (`js/clock/src/preload.ts`) does, and land it if
so. Otherwise land a grep check that flags a new `#[tokio::test]` without
`start_paused` and without a stated real-I/O reason.

`clippy.toml` already bans `tokio::time::*` and `std::thread::sleep`, but
`kio::clock::sleep` is the approved replacement and waits in real time on an
unpaused runtime. Clippy can't scope a ban on it to tests, since production
code uses it.

## Plan

Decided 2026-11-06 (maintainer, /quest-complete); see the
[epic](/quest/m0/clock-guard/README.md). ✅ Spike first, with the grep
check as the fallback. Not chosen: the grep check only; leaving Rust out.

- `kio` already knows whether a runtime is paused
  (`rs/kio/src/clock/runtime.rs`). The open questions are how a test opts
  out (real sockets, the ~150 legitimate real-I/O tests), and how to keep
  the switch from leaking into the moq binaries that integration tests
  spawn, whose QUIC timers fire for real.
- Std channels' `recv_timeout` also waits in real time; decide whether the
  guard or a clippy entry covers it.
- Record the spike's outcome here before choosing; the guard lands with
  today's offenders opted out, as in JS.

Public API: none expected. Wire: none.
