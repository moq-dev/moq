# [M] Preserve MPEG-TS timebase discontinuities through import and export

## Goal

A source-signalled timebase change, including a forward jump, reaches the exported
MPEG-TS clock as a discontinuity. Ordinary PCR rollover remains continuous.

## Plan

The controlled stimulus campaign on #2833 found that a +30 second source jump
recovers promptly after #3351, but the exported PCR jumps with no discontinuity
indicator. Rewind recovery in #3375 handles the container consumer's signal;
it does not establish that every input adaptation-field flag reaches that signal.

- Reproduce the forward-jump and signalled-restart arms at source, MoQ frames,
  and exported TS. Use the stimuli and oracle linked from the issue comment.
- Trace TS import's adaptation-field handling and container boundary publication.
  Preserve a clock discontinuity at a media boundary without treating continuity
  counter loss or a normal 33-bit rollover as a program timebase reset.
- Test forward and backward changes, a flag on a PCR-only packet, normal rollover,
  independent rendition gaps, and reordered B-frames. Check clock flags and output
  timing, not only input flag parsing.

## Related

- [Evidence on #2833](https://github.com/moq-dev/moq/issues/2833#issuecomment-5554907607)
- [Rewind recovery PR](https://github.com/moq-dev/moq/pull/3375)
- [Gap discontinuity](/quest/m1/gap-discontinuity.md) - durable boundary signalling
