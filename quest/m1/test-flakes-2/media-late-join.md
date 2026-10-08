# [S] Media late join within one GOP

## Goal

Late join holds its one-GOP budget under load in both `just test media` and
the interop browser lane, or the regression behind it is fixed.

## Plan

It failed once after [#4181](https://github.com/moq-dev/moq/pull/4181):
"joined at frame 111, 16 frames behind 127", against a budget of one GOP
(15). First decide whether 16 frames is a real regression (the player
joining at the previous GOP's keyframe) or an off-by-one in how the fixture
samples the live edge. Fix whichever it is; don't widen the budget without a
reason.

The interop browser check "late join starts live"
(`test/interop/clients/js/media.ts`, `MAX_LATE_JOIN_LAG_FRAMES`, one GOP
compared with `<=`) fails the same way on `main`: 16 frames behind with a
15-frame GOP on 09-24, 09-25, 09-28, and in
[#4577](https://github.com/moq-dev/moq/pull/4577). It samples `live` just
before the page opens. Treat it as the same flake (decided 2026-10-01).

Recorded in the 2026-10-05 audit: on the test-flakes-2 line,
[#4719](https://github.com/moq-dev/moq/pull/4719) replaced
`MAX_LATE_JOIN_LAG_FRAMES` in the interop browser check with a
published-keyframe check: the latecomer's presented timestamp must be at or
after the published track's newest keyframe. That fixes the browser lane's
sampling. The original 16-frame failure was never reproduced, so what remains
is the Rust `just test media` late join under load.
