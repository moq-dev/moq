# [M] A reattached viewer gets the live groups its relay asked for again

## Goal

A viewer that detaches every session and reattaches within seconds plays live
again: the relay serves it the live groups its new upstream subscription
receives, instead of aborting them as `Old`. The `reattach reaches live` check
of `just test media` passes on Chromium in the scheduled Interop run.

## Plan

Found by the triage of scheduled Interop run
[45041226494](https://github.com/moq-dev/moq/actions/runs/45041226494)
(2026-11-22, at `040a566d40`), on `moq-lite-06`:
`FAIL reattach reaches live: held a frame 2100ms behind the fixture 2000ms
after the first frame ...; audio running and buffering with nothing anchored`.
It is the first night this check failed; 11-20 and 11-21 passed it.

What the run's `interop-failure` artifact shows (`relay.log`, and the player's
Playwright trace, whose console carries every `__moqInteropSample`):

- The player detached at 16:51:08, releasing every session. It reattached at
  16:51:09.4 on a new session. The relay subscribed upstream to the JS fixture
  again (ids 10 to 12), fetched the audio group the join landed in
  (`fetch started ... track=audio group=43294150`, served), and abandoned the
  video one 6 ms after starting it (`fetch abandoned track=video/1
  group=43230900`).
- From 16:51:10.94 the relay's subscriber side refused the fixture's live
  groups: six `unknown uni stream err=old`, then at 16:51:11.155 seven
  consecutive audio groups at once (`group error err=old group=44754150` to
  `44874150`, 20 ms apart), all with the same microsecond timestamp.
- The player received no audio byte after reattaching (`audioBytes` stayed at
  154063, its count before the detach) and 14 video frames, then nothing:
  `videoFrames` stopped at 1007, so the picture held.

Local runs did not reproduce it: 6 of 6 `just test media --media-cases
detach` and the detach case of 4 full `just test media` runs passed on `main`
at `a5345c7dba` (2026-11-23), on a host at load 20 to 28 of 24 CPUs. Those
full runs did fail other cases on audio underruns the load explains (13 long
animation frames in the player), which the CI run did not show.

Next: find which `moq-net` path aborts a relay copy's groups `Old` (the
`group error` comes from `lite/subscriber.rs` after the group opened, so
`track.poll_aborted` or `group.handle().poll_aborted`, not the peer). The
candidates are what decides a group is behind its copy's read position: the
join's fill (`model/track/fill.rs`, `Error::Old`), the resumed reader
(`model/resume.rs`), and the copy a detached reader left. Write a lite-06
integration test of the sequence above (a copy's only reader leaves, a new
reader joins within a second, the publisher keeps appending) before fixing.
The 92 commits between the 11-21 and 11-22 runs include several in exactly
that code ([moq-dev/moq#9316](https://github.com/moq-dev/moq/pull/9316),
[moq-dev/moq#9326](https://github.com/moq-dev/moq/pull/9326),
[moq-dev/moq#9278](https://github.com/moq-dev/moq/pull/9278),
[moq-dev/moq#9307](https://github.com/moq-dev/moq/pull/9307)); bisect once a
reproduction fails reliably.

Public API: none expected. Wire: none expected.
