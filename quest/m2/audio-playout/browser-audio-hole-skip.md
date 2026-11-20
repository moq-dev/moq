# [S] Browser audio's ordered reader grades no worse than before #8799 on the runner

## Goal

The Chromium `bursty` and `congestion` rows of `just test audio-quality`, with
browser audio reading its holes from `@moq/net`'s ordered reader, grade no
worse than before [moq-dev/moq#8799](https://github.com/moq-dev/moq/pull/8799)
on the nightly runner, and no scheduled night shows a browser row starving
(`stalled_share` near 1, or a playhead holding still while the render clock
runs) that this quest has not explained.

## Plan

Decided 2026-11-16 (maintainer, /quest-run): ✅ the transport decides an
audio hole, through an ordered reader whose budget is the ring's size
(rejected: first measure the playhead rule against a hand-written wait in
the container consumer; keep the playhead rule and the separate wait).

[moq-dev/moq#9068](https://github.com/moq-dev/moq/pull/9068) moved browser
audio onto the ordered reader. Its first scheduled night (2026-11-18,
[44342367698](https://github.com/moq-dev/moq/actions/runs/44342367698)) put
112 tap underruns a minute on `chromium-opus-48000-bursty`, where every
earlier night had none: about one 4 to 6 ms hole a second, with no player
underrun and no media missing from the capture. The PR for this quest found
and fixed both causes. A reader gap over keys a timestamp-keyed publisher
never used was marked a loss whenever the consumer judged it before the
group's first frame landed. And every gap restarted the Opus codec with a cut
at the frame's timestamp rather than a pre-skip earlier, which dropped 6.5 ms
of real audio. A local live run reproduced the night's numbers without the
fix (136.8 underruns a minute) and measured zero with it. Replaying that
night's captures, now possible since the replay declares a timestamp-keyed
publisher's skipped keys, graded `bursty` level with the player before
#9068. `congestion` trimmed up to 350 ms a minute more audio there, with
stalled and silence shares level: the audio the old player never received
now arrives late and is skipped onto the clock.

The 7.5 s starvation one local run showed during #9068 was not reproduced.
The mechanism suspected for it, an active group that opens but never delivers
a frame, is bounded: the ordered subscription expires such a group one hole
wait after its successor lands, which a consumer test now holds. So a trace
does not need to record when each group opens.

Work:

- Grade the Chromium `bursty` and `congestion` rows from the
  `audio-quality-run` artifacts of the first three scheduled nights after
  the fix lands, against the nights before #8799 (2026-11-13 to 11-15) and
  the PR's table. Opus `bursty` should file no tap underruns again.
- If a night shows a browser row starving, replay its capture
  (`test/audio-quality/README.md`, "To replay a capture") and explain it
  here.

## Related

- [moq-net's last pub items with a single container-reader caller go private](/quest/m2/unused-api/net.md) - the Rust container reader moving onto `Gap`, this quest's native counterpart
