# [XS] `test-lints.test.py` hangs `just check`

## Goal

`python3 sh/rs/test-lints.test.py` finishes, so `just gh test`, which every
`just check` runs, ends instead of hanging until it is killed.

## Plan

Seen 2026-11-11: on `origin/main` (f190d90b34) the script prints one `.` and
then makes no progress for 5 minutes; inside `just check` it ran 80 minutes
before being killed. Reproduce it with `timeout 300 python3
sh/rs/test-lints.test.py`, find what the second test waits on, and fix the
root cause.
