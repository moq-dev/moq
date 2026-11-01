# [XS] moq-v4l has every libv4l-rs fix it needs

## Goal

moq-v4l carries every fix
[raymanfx/libv4l-rs](https://github.com/raymanfx/libv4l-rs) has landed since
the last check, each ported with its test or recorded as not applying, and
`rs/moq-v4l/README.md` gains an `Upstream` section naming the newest upstream
commit checked.

## Plan

Recurring child of the [vendor sync](/quest/m1/vendor-sync/README.md), which
says how each run works and recurs.

Last checked: the fork point, tag `v4l-0.14.0`
([`867d9b12`](https://github.com/raymanfx/libv4l-rs/commit/867d9b122fde603ff7f1bfcdf19de48845c9e1f7),
2023-05-12, per [moq-dev/moq#3867](https://github.com/moq-dev/moq/pull/3867)).
Upstream `master` was at
[`30f6dbae`](https://github.com/raymanfx/libv4l-rs/commit/30f6dbaeed5cdb1b33760fc5b35c1b545cfe0e46)
(2024-10-05) on 2026-10-30, 19 commits later and dormant since.

First run, check these three, found 2026-10-30 and still present in moq-v4l:

- [`b7a50013`](https://github.com/raymanfx/libv4l-rs/commit/b7a50013): wrong
  `size` in `VIDIOC_S_EXT_CTRLS` (`rs/moq-v4l/src/device.rs`, `set_controls`).
- [`6d171419`](https://github.com/raymanfx/libv4l-rs/commit/6d171419):
  `Type::Unknown`, since moq-v4l unwraps the control type in
  `rs/moq-v4l/src/control.rs`, so `query_controls` panics on a type it does
  not know.
- [`0394443c`](https://github.com/raymanfx/libv4l-rs/commit/0394443c): keep
  unknown flags rather than truncating them.

moq-video calls none of these paths (it issues `S_CTRL` itself), so all three
are latent in moq-v4l's API. Deleting an API with no consumer beats porting a
fix to it; say which was chosen in the PR.
