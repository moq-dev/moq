# [M] The browser publisher disables simulcast rungs the grant cannot fund

## Goal

`@moq/publish` (`js/publish`) stops encoding its top video renditions when its
bandwidth grant cannot fund them, advertising each as `enabled: false`, and
re-enables them with hysteresis once padding probes raise the estimate. The
lower renditions keep their share instead of every rung degrading together.

## Plan

Split from [native rung disable](/quest/m1/simulcast-rung-disable.md) by the
maintainer on 2026-10-20: the native publishers moved to m1 on native
probing, while a browser can only probe with
[padding](/quest/m2/probe/padding.md), which stays in m2.

- Mirror the native quest's rule and constants from `moq_mux::rate`, rather
  than tuning a JS-only one. Its decisions hold here too: catalog state
  follows what the encoder applied, recovery does not depend on demand, and a
  disabled rung keeps its reservation at its ceiling so the session keeps
  probing for it. The lowest rendition never disables either.
- Today each encoder reserves its ceiling and follows its own grant down
  (`js/publish/src/video/encoder.ts`). The manual `enabled` input already
  publishes `enabled: false` and stops encoding; this drives that state from
  the grant and settles how the two inputs combine.

Tests on `@moq/clock`'s `Manual`: a shrinking grant disables the top rung
first and keeps the lower ones at their share; a recovering grant re-enables
it only past the hysteresis and after its viewers left; a jittering grant near
the boundary does not flap the catalog. The epic's shaped-uplink browser test
proves recovery end to end.

Public API: whatever knob `@moq/publish` exposes for the rule. Wire: none.

## Required

- [Native rung disable](/quest/m1/simulcast-rung-disable.md) - the shared rule and constants this mirrors
- [Padding probes](/quest/m2/probe/padding.md) - the browser's sender, which raises the estimate so a disabled rung can resume

## Related

- [Closed rung disable](https://github.com/moq-dev/moq/pull/5850) - reference code from before the 2026-10-20 split, closed as superseded
