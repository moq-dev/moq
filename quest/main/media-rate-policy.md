# [S] Settle the shared media rate-policy namespace

## Goal

The existing video rate policy lives at the shared location already planned
for audio and transcode, before removing its video namespace would break 0.1.
Actual audio/ladder bandwidth adaptation remains deferred.

## Plan

The bandwidth quest currently plans to delete the public moq_video::encode::rate
module and move Policy/Control to moq_mux::rate. Perform that API relocation
now, adapt video's existing consumer and docs, and remove the old export
without a compatibility shim. Preserve one policy implementation.

Move its tests with it and make construction/update agree on bounds: the
current initial target uses max.max(min), although update lowers an excessive
minimum to the ceiling. A policy must never initialize above its ceiling.
Test inverted bounds, initial target, decreases, hysteresis, and recovery in CI.

Public API: removes the old 0.0.x video namespace and adds the shared policy
under moq-mux without breaking that package's existing exports. Wire: none.

## Related

- [Audio bandwidth](/quest/next/2848-follow-the-bandwidth-grant-in-moq-audio-instead-of.md) - later adaptation uses the shared policy
