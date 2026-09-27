# [S] moq-nvenc refuses or drives held frames

## Goal

A session whose configuration makes NVENC hold frames back (lookahead or
B-frames) either fails at `start_session` or encodes correctly. Today every
such submission fails at `Submission::finish` with `InvalidParam`.

## Plan

NVENC answers a held frame with `NV_ENC_ERR_NEED_MORE_INPUT` and forbids
locking its output until a later submission returns success. The facade
returns a `Submission` for it anyway, and `finish` locks at once. P7 with
high-quality tuning enables lookahead (depth 28 on an RTX 3070 Ti), so it
fails on every frame. `moq-video` avoids this with low-latency tuning and no
B-frames, so no caller hits it today.

Recommended: refuse at `start_session` when the config enables lookahead or
sets `frameIntervalP > 1`, and when no config is given, check the preset
config for those settings. Driving held frames means returning outputs in
submission order across calls, which no consumer needs yet. The hardware
test `failed_submission_releases_the_session` in
`rs/moq-nvenc/src/safe/session.rs` relies on lookahead to force the failure;
move it to another forcing config when this lands.
