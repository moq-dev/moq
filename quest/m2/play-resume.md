# [M] moq-cli play plays out pictures before a quick resume

## Goal

When new media arrives before the pictures buffered ahead of a declared end
have played (a source switch, say), `moq-cli play` plays the old timeline out
first instead of dropping it.

## Plan

#5288 fixed `rs/moq-cli/src/play/video.rs` so `Read::End` no longer discards
the buffered pictures, with the restart waiting for the first frame of a later
group. A resume that arrives before those pictures play still drops them, as
it did before #5288.

Playing them out needs a sequential timeline in `play`: queue the new group's
frames behind the remaining pre-end pictures, then rebase the clock at the
boundary. Queueing encoded frames is not enough when the decoder reorders: the
declared end has to reach the decode loop so it flushes the pictures it still
holds before decoding the resumed group, which today happens only when the
track ends.

✅ Decided 2026-10-13: audio queues with video. One sequential timeline: audio
and video both play out the old segment, then rebase together, which keeps
A/V sync. Not chosen: audio snaps to the new timeline; leave it open.

Test with a declared end followed immediately by a new group, using a decoder
that holds pictures (depth above zero): every pre-end picture is shown before
the first new one.

Public API: none. Wire: none.
