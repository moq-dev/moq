# [S] OBS stats race test

## Goal

A regression test proves that `MoQOutput::TryGetConnectionStats` refuses a
snapshot when a `Stop()`, restart, or disconnect retires the session while
`stats()` runs, and leaves the caller's last accepted sample untouched. It
runs against the generated C++ bindings, in `just obs test`.

## Plan

[#4281](https://github.com/moq-dev/moq/pull/4281) replaced about 2k lines of
moq-c-stubbed OBS tests with suites that drive the real moq-ffi over an
in-process relay. The old "restart rejects uncommitted stats" case in
`cpp/obs/test/moq-output-test.cpp` (see `2ae9f9a5d^`) used a stubbed
`moq_session_snapshot` hook to restart the output inside the stats call.
Codex caught that the fix it guarded was lost
([r4113081126](https://github.com/moq-dev/moq/pull/4281#discussion_r4113081126)).
The fix was restored (the post-`stats()` recheck under `mutex`), but not the
test: the window sits inside a synchronous moq-ffi call, and the real-relay
suites have no seam to hold it open. The maintainer decided in the 09-28
merged-PR audit that the test comes back before the line lands.

The work is finding an honest seam. Options include a narrow hook in the
plugin between reading stats and committing them, or a way to make the
generated `Session::stats()` observable from the test. Prefer whatever
keeps production code free of test-only branches, and ask the maintainer if
every option needs one. The test must fail with the recheck removed.

If other scenarios the stubbed suites covered are clearly lost, list them as
follow-up quests rather than restoring them here.
