# [M] Instant replay and DVR

## Goal

`moq-studio` replays the last seconds of any source or the program on demand:
several lengths, slow motion, and a one-click clip saved to disk, with no
separate always-on recording. Viewers can scrub back through the live
broadcast as far as the relay or archive holds it.

## Plan

The groups are already on hand: the studio's own recent groups and the
relay's cache, addressed by presentation time through subscription ranges
(`doc/concept/moq-lite.md`). A replay plays from those rather than a RAM-only
frame buffer like OBS's. Retention past the relay cache is the archive's job.

Sources and the program encode only while subscribed
([source tracks](/quest/m3/studio/compose/sources.md)), so a track nobody
watched has no groups to replay. Arming replay for a source or the program
is a local subscriber holding a retention window, so that track keeps
encoding with no external subscriber, and its encode cost shows like any
other subscriber's. An unarmed track replays only what was encoded, and the
gaps show rather than being skipped. Acceptance covers the case where every
viewer composes sources themselves and nothing outside the studio subscribes
to the program.
Replay published to viewers is a new source in the scene, so it composes like
any other.

## Related

- [Archive DVR](/quest/m2/archive-dvr.md) - viewer scrubbing beyond the relay cache
- [Recording](/quest/m3/studio/recording.md) - clips use the same file writer
