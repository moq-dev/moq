# [M] Clean ALSA playback onset

## Goal

ALSA playback over PulseAudio starts without restarting output and without
audible clicks right after onset or after a switch.

## Plan

- From [moq-dev/moq#6676](https://github.com/moq-dev/moq/pull/6676): a
  transient EIO from `snd_pcm_avail_delay` counts toward `ERROR_LIMIT`
  (`rs/moq-audio/src/playback/driver.rs`) and restarts output; the first
  callbacks then stall about 1.5 s. Don't count an onset EIO toward the limit.
- Size the sink prefill below the target so a fresh sink lands on it, which
  should remove the onset clicks. Re-measure both before and after.
