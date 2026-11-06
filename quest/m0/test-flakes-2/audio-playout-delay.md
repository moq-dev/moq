# [S] The Python audio playout-delay test reads its frames

## Goal

`test_audio_consumer_reports_its_playout_delay` in
`py/moq-rs/tests/test_local.py` reads both written frames from each of its
consumers in loaded runs and in the merge queue, fixed at its cause under
the [epic](/quest/m0/test-flakes-2/README.md)'s rules, never by raising its
5-second wait. Done when the PR explains the stalled read and shows the run
that reproduced it now passing.

## Plan

Added 2026-11-05 (maintainer, /quest-complete follow-ups), from the merge
queue entry for [moq-dev/moq#7951](https://github.com/moq-dev/moq/pull/7951)
at `04a53405db`,
[run 42145929336](https://github.com/moq-dev/moq/actions/runs/42145929336/job/127657266646)
(2026-11-06 01:51Z): `1 failed, 124 passed`. The first read timed out:

```text
>       assert isinstance(await asyncio.wait_for(anext(fixed), timeout=5.0), moq.audio.Frame)
moq-rs/tests/test_local.py:1630:
E                   TimeoutError
```

The consumer with the fixed 80 ms delay (`estimate=False`) never returned
its first frame within 5 s. The land fixer
([note](https://github.com/moq-dev/moq/pull/7951#issuecomment-6674925814))
found the test and its implementation unchanged by the PR, and the full
Python suite passed locally (125 tests). The PR landed on requeue. No other
failed run from 2026-11-03 to 2026-11-06 shows this test, and no other PR
comment names it.

It failed again on 2026-11-06 in the merge queue entry for
[moq-dev/moq#7904](https://github.com/moq-dev/moq/pull/7904) at `565d07a3f6`
(same TimeoutError). The land fixer found the PR unrelated and the full
Python suite passing locally, then stopped on the repeated failure. The test
is now skipped with `@pytest.mark.skip` pointing here; remove the skip with
the fix.

Lead, unverified: the test writes only two 20 ms Opus frames (40 ms) and
doesn't finish the track until after both reads. Any path that needs more
input before the first frame goes out can stall the read for good. Candidates
are the encoder's lookahead, the decoder's warmup trim, the fixed delay
waiting for 80 ms of buffered audio, or a subscription that isn't live when
the frames are written. Which one applies depends on timing, which fits a
failure that appears only under load.

Reproduce first: run the test repeatedly while `just check` loads the host,
and record whether the encoder published the frames and where the decoder
held them. Then fix whichever of the test or the decoder is wrong.

Public API: none. Wire: none.
