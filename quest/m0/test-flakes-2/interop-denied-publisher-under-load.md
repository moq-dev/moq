# [S] Interop's denied-publisher check holds up under load

## Goal

`just test interop` passes `<pub> publisher refused: Unauthorized` on a
CPU-starved host, instead of `<pub> publisher did not fail with Unauthorized
naming <broadcast>`.

## Plan

Found 2026-11-13 while reproducing the cpp audio gap in
[moq-dev/moq#8611](https://github.com/moq-dev/moq/pull/8611). Every
`bash test/interop/interop.sh --publishers js,python --subscribers cpp` run in a
`systemd-run --user --scope -p CPUQuota=50%` (or 150%) scope failed it for
python, as did the full `--all` matrix under host-wide `stress-ng`, for
python, rust, go, and cpp. Unloaded, `--publishers python --subscribers rust`
passes it on both origin/main (65ca6b6ce) and #8611's branch, so load is the
trigger, not a change.

The failing publisher log shows the publisher running normally after the
denied publish, with no Unauthorized line before the round ends. Reproduce in
a CPU-quota cgroup (never starve the whole host), keep `pub-<lang>-denied.log`
and the relay log, and find whether the relay's refusal never reaches the
publisher in time or the harness's `check_denied` window closes before the
publisher logs it; fix whichever end is wrong rather than lengthening the
window.
