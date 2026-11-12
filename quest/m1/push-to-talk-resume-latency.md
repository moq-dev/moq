# [S] Back-to-back push-to-talk presses keep their latency

## Goal

A press that starts while the previous press is still playing continues on the old schedule, so `just test push-to-talk`'s delay stays flat across back-to-back presses instead of growing by about 250 ms per press.

## Plan

Today each resume in `rs/moq-mux/src/sync.rs` (`Track::plan`/`resume`) starts one delay after the old tail drains, even when no gap separates the presses. The added hold (`held`) only shrinks at an eviction or a long mute.

Recommendation: when the new segment's first media is due no later than the old tail ends, plus one frame, map it onto the old schedule with no added hold. Re-buffer one delay only after a real gap, where the sink would underrun anyway. Mirror the same rule in `js/watch` if its audio sync has the same resume. Make `just test push-to-talk` fail when the delay grows across back-to-back presses.

Public API: none expected. Wire: none.

## Related

- [Gapless audio switch](/quest/m3/gapless-audio-switch.md) - the same schedule continuity across a track change
