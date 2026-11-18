# [S] moq-ffi's in-place shutdown test holds under load

## Goal

`test::shutdown_waits_for_an_in_place_poll` in `rs/moq-ffi/src/test.rs` passes however loaded the machine is.

## Plan

It failed once under a full `just check` on 2026-11-18 at load average 31, on a branch that touched no moq-ffi code ([moq-dev/moq#9123](https://github.com/moq-dev/moq/pull/9123)), and passed six runs alone.
The child process aborted with "a 994.299µs timer fired on the host clock, which KIO_DENY_HOST_TIMERS=1 denies": the held poll's `kio::clock::sleep(Duration::from_millis(1))` fired on the host clock before shutdown cancelled it.
Reproduce it under load first, then drive that sleep from a mocked clock, or wait on an event instead, so the test still shows that only the shutdown finishes the held call.
