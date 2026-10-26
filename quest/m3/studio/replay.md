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
Replay published to viewers is a new source in the scene, so it composes like
any other.

## Related

- [Archive time ranges](/quest/m2/archive-time-ranges.md) - ranges reaching past the cache
- [Archive DVR](/quest/m2/archive-dvr.md) - viewer scrubbing beyond the relay cache
- [Recording](/quest/m3/studio/recording.md) - clips use the same file writer
