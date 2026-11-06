# [XS] moq-v4l has every libv4l-rs fix it needs

## Goal

moq-v4l carries every fix
[raymanfx/libv4l-rs](https://github.com/raymanfx/libv4l-rs) has landed since
the last check, each ported with its test or recorded as not applying, and
`rs/moq-v4l/README.md`'s `Upstream` section names the newest upstream commit
checked.

## Plan

Recurring child of the [vendor sync](/quest/m3/vendor-sync/README.md), which
says how each run works and recurs.

Last checked (2026-10-31): upstream `master` through
[`30f6dbae`](https://github.com/raymanfx/libv4l-rs/commit/30f6dbaeed5cdb1b33760fc5b35c1b545cfe0e46)
(2024-10-05), with every commit since the `v4l-0.14.0` fork point dispositioned
in the README and no published advisories. Upstream has been dormant since.

Learned: the last run removed the unused `control` module and control APIs
instead of porting three fixes to them, so a fix to controls, the libv4l
backend, sys build scripts, examples, or FreeBSD has nothing to apply to.
Deleting an API with no consumer beats porting a fix to it.

## Required

- [Cut and publish the prepared breaking release](/quest/m3/release/cut.md) - the next cycle starts after the cut
