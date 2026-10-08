# [S] Interop freezes on CI runners are attributed

## Goal

The 0.4 to 0.8 s freezes where both interop tracks stop at once on CI are
attributed to the runner, the relay, or the player, with evidence from the
nightly traces. A cause in our code is fixed at its source; a runner stall is
documented, and the interop checks tell it apart from a playback bug instead
of failing on it as one.

## Plan

Found in [#4529](https://github.com/moq-dev/moq/pull/4529) while chasing the
cold-start tone check: some CI traces show both tracks freezing together,
which that fix shortens but cannot remove. #4529 added the player's `delay` to
the interop trace, so a week of nightlies after it lands shows the freezes
without the delay-flush noise that hid them.

Collect the failing and passing nightly interop traces for that week, line up
each freeze against relay and player timestamps, and look for what the
runner was doing (CPU steal, GC, disk). Decided with the maintainer: diagnose
first from real nightlies rather than assume a runner stall.

Public API: none. Wire: none.
