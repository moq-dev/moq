# [S] Media late join within one GOP

## Goal

`just test media` late join holds its one-GOP budget under load, or the
regression behind it is fixed.

## Plan

It failed once after [#4181](https://github.com/moq-dev/moq/pull/4181):
"joined at frame 111, 16 frames behind 127", against a budget of one GOP
(15). First decide whether 16 frames is a real regression (the player
joining at the previous GOP's keyframe) or an off-by-one in how the fixture
samples the live edge. Fix whichever it is; don't widen the budget without a
reason.
