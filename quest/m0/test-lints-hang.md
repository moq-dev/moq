# [XS] test-lints.test.py stalls just check on a busy host

## Goal

`python3 sh/rs/test-lints.test.py` finishes in seconds however loaded the
host is, so `just gh test`, which every `just check` runs, never stalls on it.

## Plan

Seen 2026-11-11: on `origin/main` (f190d90b34) the script prints one `.` and
then makes no progress for 5 minutes; inside `just check` it ran 80 minutes
before being killed. The same day another `just check` passed it on a quieter
host.

Decided 2026-11-11 (maintainer, /quest-unblock pick of
[moq-dev/moq#8517](https://github.com/moq-dev/moq/pull/8517)): ✅ keep this
quest, with this diagnosis (rejected: dropping it). The `.` is
`test_clippy_warning_fails_test_build`, which sets `LOCAL_SUPERVISED=1`.
`test_fail_fast_environment_reaches_nextest` does not, so each of its 20
runs (10 through `just rs test`, 10 calling `sh/rs/nextest.sh` directly) goes
through `sh/local.sh` to `quest gate` and waits for host room; on a loaded host that never comes in useful time. Set
`LOCAL_SUPERVISED=1` in its env as the clippy test does, and confirm the
script finishes while other gated work holds the host.
