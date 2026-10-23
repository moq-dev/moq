# [S] The interop tone check charges its grace only for audible playback

## Goal

The JS interop driver's tone check fails only when audio that played was
silent or wrong, never because the player was still refilling its ring. Under
CPU starvation a browser cell waits out the re-stall instead of failing fast
with silence or a false "wrong tone".

## Plan

Found while closing the cpp to js budget flake
([moq-dev/moq#6442](https://github.com/moq-dev/moq/pull/6442)), never seen in
CI. With 8 browsers on 2 CPUs, 7 of 500+ main browser cells failed in under
12 s: the auto playout delay rose to 1 to 5 s, the ring re-stalled to refill,
and the 3 s `TONE_GRACE_MS` in `test/interop/clients/js/driver.ts` expired on
silence. Once it read stuttered audio as a wrong tone.

Reproduce it under the same starvation, then measure the grace from audible
playback (the player's own stalled/playing state) rather than wall time since
the check began, keeping the overall cell budget unchanged.

Public API: none. Wire: none.
