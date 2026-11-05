# [M] A multi-program TS import survives a source switch

## Goal

`--program all` (moq-mux's `container::ts::Programs`) handles an unannounced
source switch the way a single-program import does after
[moq-dev/moq#7780](https://github.com/moq-dev/moq/pull/7780): when the input
switches to a source whose PAT names a different `transport_stream_id` or
program set, or whose clocks all jump, each affected program starts a new
instance instead of the whole import ending. Done when a test feeding two
multi-program sources back to back, one with a different
`transport_stream_id`, keeps publishing every program across the switch.

## Plan

Planned 2026-11-04 with the maintainer, from #7780. That PR gave the
single-program `Import` a restart for an identity change (`Step::Identity`:
a PAT naming another identity restarts at that PAT rather than failing with
"the PAT changed the program's identity") and settled that nothing crosses a
restart: the new instance waits for the source's own PAT/PMT and next
keyframe. `Programs` (`rs/moq-mux/src/container/ts/programs.rs`) has no
restart path, so a changed `transport_stream_id` still ends the whole
import.

- Reuse #7780's step judgement and its "carry nothing" rule rather than a
  second mechanism; decide whether the switch restarts every program or only
  the ones whose identity or clock changed, and say why in the PR.
- Keep the existing [TS reliability](/quest/m1/ts-reliability/README.md)
  rules for PAT changes that aren't a source switch (a moved PMT PID, a
  program added or removed) as they are.

Public API: possibly additive on `Programs` (a restart result like
`Import`'s). Wire: none.

## Related

- [TS fidelity and recovery](/quest/m1/ts-reliability/README.md) - the single-program restart and PAT-change rules this extends
